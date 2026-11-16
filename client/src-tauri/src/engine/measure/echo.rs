//! 에코 피어 측정(`engine_set_echo_mode`·`engine_measure_echo_peer`): 워커가 쥐는 프로브(`EchoProbe`), 측정 스레드 ↔ 워커
//! 큐(`EchoPort`·`EchoRt`), 측정 작업(`EchoJob`). 왕복 식은 `measure` 모듈 문서.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use serde::Serialize;

use super::analysis::{analyze_echo, click_template, echo_message, echo_summary, summarize, MeasureVerdict, CANCELLED, MAX_ECHO_MS, MAX_PEER_ECHO_DELAY_US};
use band_dsp::codec::Encoder;
use band_dsp::config::{CodecKind, INTERNAL_RATE, MAX_PAYLOAD_BYTES};
use crate::net::NetShared;

// ============================================================================
// 에코 피어
// ============================================================================

/// 클릭 간격(48k 샘플) = 300ms. 프레임 크기(120/240/480)의 배수라 클릭이 늘 프레임 시작에 놓인다.
pub const CLICK_INTERVAL: u64 = 14_400;
/// 에코가 시작된 뒤 지터 버퍼를 다시 채우고 통계가 안정될 때까지 기다리는 시간.
const ECHO_SETTLE: Duration = Duration::from_millis(1500);
/// 측정 끝에 상대가 에코를 멈출 때까지 기다리는 최대 시간(그동안 상대에게는 무음을 보낸다).
const ECHO_STOP_WAIT: Duration = Duration::from_secs(4);

/// 측정이 끝나고 워커가 프로브를 돌려주기를 기다리는 상한. 워커는 반복(≤ 약 1ms)마다 본다 — 못 받으면 워커가 멈춘 것이다.
const ECHO_RETURN_WAIT: Duration = Duration::from_millis(200);
/// 재생 시각 탭 버퍼 여유(듣는 시간에 더하는 초).
const ECHO_TAP_SLACK_S: f64 = 0.5;
/// 한 측정의 클릭 수 상한.
pub const ECHO_MAX_RUNS: u32 = 20;
const WORKER_UNRESPONSIVE: &str = "오디오 워커가 응답하지 않습니다";

/// 용량 `cap`의 빈 버퍼. 측정 스레드에서 **미리 한 번 만져 둔다** — `with_capacity`만 하면 워커가 처음 채울 때 4KB마다
/// 페이지 폴트로 커널에 들어간다(#140 리뷰 L1). 0이 아닌 값으로 채운다(0은 페이지를 안 만지고 끝낼 수 있다).
fn prefaulted<T: Copy + From<f32>>(cap: usize) -> Vec<T> {
    let mut v = Vec::with_capacity(cap);
    v.resize(cap, T::from(1.0));
    v.clear();
    v
}

/// 클릭을 `runs`회 보낸 뒤 돌아오기를 듣는 시간(초).
fn echo_listen_s(runs: u32) -> f64 {
    runs as f64 * CLICK_INTERVAL as f64 / INTERNAL_RATE as f64 + MAX_ECHO_MS / 1000.0 + 0.1
}

/// 에코 측정 상태(엔진 ②b-5). **측정 동안 워커가 혼자 가진다** — 측정 스레드가 만들어(인코더·버퍼 사전할당 포함) 명령 큐로
/// 넘기고, 워커가 쓰다가 반환 큐로 돌려준다. 만들고 버리는 것은 측정 스레드다. 워커는 여기서 할당하지 않는다:
/// 버퍼는 용량 안에서만 채우고, 넘치면 버리고 센다.
///
/// 프로브가 워커에 있는 동안 워커는 그 피어에게 내 입력 대신 무음(+ 클릭)을 보낸다. 그래서 프로브는 **상대가 에코를 멈출
/// 때까지** 워커에 남아야 한다 — 먼저 거두면 상대가 반사를 멈추기 전에 내 실제 소리가 내게 되돌아온다.
pub struct EchoProbe {
    /// 측정 번호(1부터, 측정마다 +1). 워커는 이것으로 `EchoCtl`의 신호가 이 프로브의 것인지 판정한다.
    id: u32,
    pub peer_id: String,
    pub peer_ssrc: u32,
    /// 클릭 스트림 전용 인코더(설정 코덱과 같은 인자). 다른 피어와 코덱 상태를 섞지 않는다.
    enc: Encoder,
    ich: usize,
    frame: Vec<f32>,
    payload: Vec<u8>,
    click: Vec<f32>,
    clicks_started: bool,
    /// 남은 클릭 수. 0이면 무음만 보낸다(측정 전후: 상대가 내 실제 소리를 되돌리지 않게).
    clicks_left: u32,
    until_next: u64,
    cursor: Option<usize>,
    /// 클릭 송신 시각(워커 epoch µs, 프레임 시작 = 송신 시각 − 프레임 길이). 용량 = 클릭 수 상한.
    pub emitted_us: Vec<f64>,
    /// 켜져 있으면 워커가 그 피어의 소스 링에 넣는 샘플(모노)과 재생(쓰기) 예정 시각을 모은다.
    pub tap: bool,
    pub arrivals: Vec<f32>,
    pub arrival_us: Vec<f64>,
    /// 탭 버퍼가 차서 버린 샘플 수(0이 아니면 측정이 불안정하다).
    pub overflow: u64,
    /// 탭 버퍼 상한(샘플). 두 `Vec`의 실제 용량은 이 이상이다 — 넘침은 이 값으로 판정한다(용량이 서로 달라도 늘지 않게).
    tap_cap: usize,
}

impl EchoProbe {
    /// 측정 스레드에서 만든다. `runs` = 보낼 클릭 수(탭 버퍼 크기).
    pub fn new(id: u32, peer_id: String, peer_ssrc: u32, stream: EchoStream, runs: u32) -> Result<Box<Self>, String> {
        let EchoStream { codec, channels, opus_bitrate: bitrate, frame_samples: frame } = stream;
        let ich = channels.max(1);
        let tap_cap = ((echo_listen_s(runs) + ECHO_TAP_SLACK_S) * INTERNAL_RATE as f64) as usize;
        Ok(Box::new(Self {
            id,
            peer_id,
            peer_ssrc,
            enc: Encoder::new(codec, ich, bitrate)?,
            ich,
            frame: vec![0.0; frame * ich],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            click: click_template(),
            clicks_started: false,
            clicks_left: 0,
            until_next: 0,
            cursor: None,
            emitted_us: Vec::with_capacity(ECHO_MAX_RUNS as usize),
            tap: false,
            arrivals: prefaulted(tap_cap),
            arrival_us: prefaulted(tap_cap),
            tap_cap,
            overflow: 0,
        }))
    }

    /// 다음 프레임 경계부터 `n`개 클릭을 보낸다.
    pub fn start_clicks(&mut self, n: u32) {
        self.clicks_started = true;
        self.clicks_left = n;
        self.until_next = 0;
        self.emitted_us.clear();
        self.arrivals.clear();
        self.arrival_us.clear();
        self.tap = true;
    }

    pub fn clicks_done(&self) -> bool {
        self.clicks_left == 0 && self.cursor.is_none()
    }

    /// 이 피어에게 보낼 프레임(`ich` 채널 인터리브, 48k)을 `out`에 만든다. `now_us` = 송신 시각(워커 epoch).
    fn fill_frame(&mut self, out: &mut [f32], now_us: f64) {
        let ich = self.ich;
        let f = out.len() / ich;
        out.fill(0.0);
        for pos in 0..f {
            if self.clicks_left > 0 && self.until_next == 0 && self.cursor.is_none() {
                self.cursor = Some(0);
                self.clicks_left -= 1;
                self.until_next = CLICK_INTERVAL;
                if self.emitted_us.len() < ECHO_MAX_RUNS as usize {
                    self.emitted_us.push(now_us - (f - pos) as f64 * 1e6 / INTERNAL_RATE as f64);
                }
            }
            self.until_next = self.until_next.saturating_sub(1);
            if let Some(c) = self.cursor {
                let v = self.click.get(c).copied().unwrap_or(0.0);
                if let Some(fr) = out.get_mut(pos * ich..pos * ich + ich) {
                    fr.fill(v);
                }
                self.cursor = (c + 1 < self.click.len()).then_some(c + 1);
            }
        }
    }

    /// 워커(송신): 이 피어에게 보낼 프레임(무음 + 클릭)을 만들어 전용 인코더로 인코드한다. 반환: 대상 피어 id와 페이로드
    /// (인코드 실패면 None — 그 프레임은 그 피어에게 평소 소리가 간다, 전과 같다). `now_us` = 송신 시각(워커 epoch).
    pub fn encode_next(&mut self, now_us: f64) -> Option<EchoProbeFrame<'_>> {
        let mut frame = std::mem::take(&mut self.frame);
        self.fill_frame(&mut frame, now_us);
        let n = self.enc.encode(&frame, &mut self.payload);
        self.frame = frame;
        Some(EchoProbeFrame { peer_id: &self.peer_id, payload: &self.payload[..n.ok()?] })
    }

    /// 워커(공급): 소스 링에 넣는 48k 스테레오 청크와 첫 샘플의 재생(쓰기) 예정 시각.
    pub fn record(&mut self, stereo: &[f32], first_us: f64) {
        if !self.tap {
            return;
        }
        let step = 1e6 / INTERNAL_RATE as f64;
        for (m, fr) in stereo.as_chunks::<2>().0.iter().enumerate() {
            if self.arrivals.len() >= self.tap_cap {
                self.overflow += 1;
                continue;
            }
            self.arrivals.push((fr[0] + fr[1]) * 0.5);
            self.arrival_us.push(first_us + m as f64 * step);
        }
    }
}

/// 클릭 스트림의 형식(설정 코덱과 같다 — 전에 워커가 클릭용 인코더를 만들던 인자).
#[derive(Debug, Clone, Copy)]
pub struct EchoStream {
    pub codec: CodecKind,
    /// 내부 채널 수.
    pub channels: usize,
    pub opus_bitrate: u32,
    /// 프레임 크기(48k 프레임 수).
    pub frame_samples: usize,
}

/// `EchoProbe::encode_next`가 빌려 주는 것: 클릭 스트림을 받을 피어와 이번 프레임의 페이로드.
pub struct EchoProbeFrame<'a> {
    pub peer_id: &'a str,
    pub payload: &'a [u8],
}

/// 측정 스레드 → 워커 신호(원자값만). 셋 다 **측정 번호에 묶인 상태**다(사건이 아니다): 측정 스레드만 쓰고 번호는 늘기만
/// 한다. 워커는 반복마다 쥔 프로브의 번호와 비교하므로 설치와 신호를 어느 순서로 보아도 결과가 같다 — 놓칠 수 없다.
#[derive(Debug, Default)]
pub struct EchoCtl {
    /// 측정 번호 << 32 | 횟수: "이 번호의 측정은 클릭 n회를 시작한다".
    clicks: AtomicU64,
    /// "이 번호까지는 탭을 끈다"(결과 수거 시점). 프로브는 워커에 남는다.
    tap_off: AtomicU32,
    /// "이 번호까지는 프로브를 돌려준다"(상대가 에코를 멈춘 뒤).
    stop: AtomicU32,
}

/// 에코 프로브 큐 용량: 측정은 하나씩이다(`busy`). 지난 측정의 프로브가 아직 워커에 있어도 하나는 더 들어간다.
const ECHO_QUEUE: usize = 2;

/// 측정 스레드 쪽 끝(엔진이 갖고 `EchoJob`에 `Arc`로 준다). `Mutex` 둘은 측정 스레드만 잡는다(`Sync`여야 해서 있을 뿐) —
/// 워커는 잡지 않는다.
pub struct EchoPort {
    cmd: Mutex<HeapProd<Box<EchoProbe>>>,
    ret: Mutex<HeapCons<Box<EchoProbe>>>,
    ctl: Arc<EchoCtl>,
    busy: AtomicBool,
    next_id: AtomicU32,
}

/// 워커 쪽 끝.
pub struct EchoPortRt {
    cmd: HeapCons<Box<EchoProbe>>,
    ret: HeapProd<Box<EchoProbe>>,
    ctl: Arc<EchoCtl>,
}

/// 명령·반환 링을 여기서 사전할당한다.
pub fn echo_port() -> (EchoPort, EchoPortRt) {
    let (cp, cc) = HeapRb::<Box<EchoProbe>>::new(ECHO_QUEUE).split();
    let (rp, rc) = HeapRb::<Box<EchoProbe>>::new(ECHO_QUEUE).split();
    let ctl = Arc::new(EchoCtl::default());
    (EchoPort { cmd: Mutex::new(cp), ret: Mutex::new(rc), ctl: ctl.clone(), busy: AtomicBool::new(false), next_id: AtomicU32::new(0) }, EchoPortRt { cmd: cc, ret: rp, ctl })
}

impl EchoPort {
    /// 측정 시작: 이미 측정 중이면 거부하고, 아니면 새 측정 번호를 준다. 끝나면 반드시 `end`.
    pub fn begin(&self) -> Result<u32, String> {
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err("이미 에코 측정 중입니다".into());
        }
        Ok(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    pub fn end(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// 프로브를 워커에 넘긴다. 먼저 반환 큐를 비운다(지난 측정이 제때 못 받은 프로브가 뒤늦게 와 있을 수 있다 — 여기,
    /// 측정 스레드에서 해제된다). 명령 큐가 차 있으면 워커가 멈춘 것이다(프로브는 여기서 해제).
    pub fn install(&self, probe: Box<EchoProbe>) -> Result<(), String> {
        while self.ret.lock().try_pop().is_some() {}
        self.cmd.lock().try_push(probe).map_err(|_| WORKER_UNRESPONSIVE.to_string())
    }

    /// 측정 `id`의 클릭 `n`회를 시작하라.
    pub fn start_clicks(&self, id: u32, n: u32) {
        self.ctl.clicks.store((id as u64) << 32 | n as u64, Ordering::Release);
    }

    /// 측정 `id`까지는 탭을 꺼라(프로브는 워커에 남는다).
    pub fn tap_off(&self, id: u32) {
        self.ctl.tap_off.fetch_max(id, Ordering::AcqRel);
    }

    /// 측정 `id`까지는 프로브를 돌려 달라.
    pub fn stop(&self, id: u32) {
        self.ctl.stop.fetch_max(id, Ordering::AcqRel);
    }

    /// 워커가 돌려준 측정 `id`의 프로브를 `timeout`까지 기다려 받는다(1ms 간격). 그보다 오래된 것은 여기서 버린다.
    pub fn recover(&self, id: u32, timeout: Duration) -> Option<Box<EchoProbe>> {
        let t0 = Instant::now();
        loop {
            while let Some(p) = self.ret.lock().try_pop() {
                if p.id == id {
                    return Some(p);
                }
            }
            if t0.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// 워커가 갖는 에코 상태(조정자 소유): 큐의 워커 쪽 끝 + 지금 쥔 프로브. "본 세대"를 기억하는 필드는 없다 — 판정은 전부
/// "쥔 프로브의 번호와 `EchoCtl`의 비교"다.
pub struct EchoRt {
    port: EchoPortRt,
    probe: Option<Box<EchoProbe>>,
}

impl EchoRt {
    pub fn new(port: EchoPortRt) -> Self {
        Self { port, probe: None }
    }

    /// 워커 반복 맨 앞. 락·할당 없음(원자값 읽기와 큐 pop/push).
    /// ① 쥔 프로브가 없으면 명령 큐에서 하나 받는다
    /// ② 쥔 프로브의 번호 ≤ `stop`이면 반환 큐로 돌려주고 끝(방금 받은 것도 마찬가지 — 설치보다 취소가 먼저 보였어도 갇히지 않는다)
    /// ③ 번호 ≤ `tap_off`이면 탭을 끈다
    /// ④ `clicks`의 번호가 같고 아직 시작하지 않았으면(탭을 끄라는 신호가 없을 때만) 클릭을 시작한다
    pub fn poll(&mut self) {
        if self.probe.is_none() {
            self.probe = self.port.cmd.try_pop();
        }
        let Some(p) = self.probe.as_mut() else {
            return;
        };
        let ctl = &self.port.ctl;
        if p.id <= ctl.stop.load(Ordering::Acquire) {
            // 반환 큐가 차 있으면(측정 스레드가 아직 안 비움) 쥔 채 다음 반복에 다시 — 워커가 Box를 떨어뜨리지 않는다.
            if let Some(p) = self.probe.take() {
                if let Err(p) = self.port.ret.try_push(p) {
                    self.probe = Some(p);
                }
            }
            return;
        }
        if p.id <= ctl.tap_off.load(Ordering::Acquire) {
            p.tap = false;
        } else if !p.clicks_started {
            let c = ctl.clicks.load(Ordering::Acquire);
            if (c >> 32) as u32 == p.id {
                p.start_clicks(c as u32);
            }
        }
    }

    /// 송신: 프로브가 있으면 그 피어에게 내 입력 대신 무음 + 클릭을 보낸다.
    pub fn probe_mut(&mut self) -> Option<&mut EchoProbe> {
        self.probe.as_deref_mut()
    }

    /// 공급: 탭이 켜져 있을 때만(그 피어 소리의 재생 시각을 모은다).
    pub fn tap_mut(&mut self) -> Option<&mut EchoProbe> {
        self.probe.as_deref_mut().filter(|p| p.tap)
    }

    /// 워커가 루프를 나갈 때: 쥔 프로브와 명령 큐에 남은 것을 반환 큐로(측정 스레드가 해제한다).
    pub fn close(&mut self) {
        while let Some(p) = self.probe.take().or_else(|| self.port.cmd.try_pop()) {
            if self.port.ret.try_push(p).is_err() {
                break; // 반환 큐가 찼다: 남은 것은 큐·워커와 함께 해제된다(엔진 정지 중)
            }
        }
    }
}


#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoReport {
    pub verdict: MeasureVerdict,
    pub message: String,
    pub peer_id: String,
    /// 클릭마다 왕복(ms), 못 찾으면 null.
    pub runs: Vec<Option<f64>>,
    pub detected_runs: u32,
    /// 통계·판정에 쓴 클릭 수(= 검출 − 제외).
    pub used_runs: u32,
    /// 단일 이상치로 뺀 클릭 수(0 또는 1, `single_outlier`).
    pub excluded_runs: u32,
    /// 뺀 클릭의 `runs` 인덱스.
    pub outlier_index: Option<u32>,
    /// 네트워크 포함 왕복 중앙값(ms): 프레임 + 망 왕복 + 상대 되돌리기 + 내 지터 버퍼(소스 링 포함).
    /// 중앙값·최소·최대·표준편차는 뺀 클릭을 제외한 값이다.
    pub round_trip_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub std_dev_ms: Option<f64>,
    /// 판정에 쓴 흩어짐(최대 − 최소, ms). `ECHO_UNSTABLE_SPREAD_MS`보다 크면 불안정.
    pub spread_ms: Option<f64>,
    /// 상대가 보고한 로컬 처리 지연(되돌리기, ms).
    pub peer_echo_delay_ms: f64,
    /// 편도 추정 = (왕복 − 상대 처리 지연) / 2.
    pub one_way_ms: Option<f64>,
    /// 측정 시점 핑 RTT(ms).
    pub network_rtt_ms: Option<f32>,
    /// 측정 중 내 지터 버퍼(`bufferMs`, 소스 링 포함).
    pub jitter_buffer_ms: f32,
    pub frame_ms: f32,
    /// 내 로컬 처리 지연(`estLatencyMs`)과 그 장치 버퍼 몫.
    pub local_latency_ms: f32,
    pub input_buffer_ms: f32,
    pub output_buffer_ms: f32,
    /// 입→귀 추정(GOAL 1절) = 왕복 − 상대 처리 − 핑 RTT/2 + 입력 버퍼 + 출력 버퍼
    /// = 입력 버퍼 + 프레임 + 망 편도 + 지터 버퍼 + 출력 버퍼(상대도 나와 같은 설정이라고 가정).
    pub mouth_to_ear_ms: Option<f64>,
    /// 설명되지 않는 몫 = 왕복 − (프레임 + 핑 RTT + 상대 처리 + 지터 버퍼). 0에 가까우면 통계가 맞다.
    pub residual_ms: Option<f64>,
    pub path: Option<band_dsp::types::PathKind>,
}

/// 에코 측정 작업(측정하는 쪽). 엔진 락 밖에서 돈다.
pub struct EchoJob {
    /// 에코 프로브 큐의 측정 스레드 쪽 끝(엔진 ②b-5).
    pub port: Arc<EchoPort>,
    pub net: Arc<NetShared>,
    pub peer_id: String,
    pub peer_ssrc: u32,
    /// 측정 대상 피어의 미디어 상태(지터 재시작·버퍼 깊이 수집, 엔진 ③).
    pub media: Arc<band_dsp::peer_media::PeerMediaShared>,
    /// 클릭 스트림 형식(설정 코덱과 같다).
    pub stream: EchoStream,
    pub frame_ms: f32,
    pub local_latency_ms: f32,
    pub input_buffer_ms: f32,
    pub output_buffer_ms: f32,
}

/// 듣기가 끝난 시점에 net·통계에서 읽은 재료(분석은 프로브를 회수한 뒤에 한다).
struct EchoMaterials {
    delay_us: u32,
    rtt: Option<f32>,
    jitter: f32,
    path: Option<band_dsp::types::PathKind>,
}

/// `EchoPort::begin`의 짝을 drop에서 맞춘다(#140 리뷰 L4): 측정 도중 패닉해도 워커가 프로브를 돌려주고(`stop`) 다음 측정을
/// 시작할 수 있다(`end`). 둘 다 다시 불러도 같다.
struct EchoSession<'a> {
    port: &'a EchoPort,
    id: u32,
}

impl Drop for EchoSession<'_> {
    fn drop(&mut self) {
        self.port.stop(self.id);
        self.port.end();
    }
}

impl EchoJob {
    /// 상대 동의를 `wait`까지 기다린 뒤 `runs`회(1..=20) 클릭을 보내 왕복을 잰다. 블로킹.
    pub fn run(&self, runs: u32, wait: Duration, cancel: &AtomicBool) -> Result<EchoReport, String> {
        let id = self.port.begin()?;
        let session = EchoSession { port: &self.port, id };
        let mut installed = false;
        let res = self.run_inner(id, runs.clamp(1, ECHO_MAX_RUNS), wait, cancel, &mut installed);
        // 정리: 요청을 거두고, 상대가 에코를 멈출 때까지 무음을 보낸 뒤(프로브는 그동안 워커에 남는다) 프로브를 거두고,
        // 지터 버퍼를 상대 소리로 다시 맞춘다.
        let _ = self.net.set_echo_request(&self.peer_id, false);
        let t0 = Instant::now();
        while t0.elapsed() < ECHO_STOP_WAIT && self.net.remote_echo(&self.peer_id).is_ok_and(|(on, _)| on) {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.port.stop(id);
        let probe = if installed { self.port.recover(id, ECHO_RETURN_WAIT) } else { None };
        self.media.request_restart();
        drop(session);
        let m = res?;
        // 프로브(수집 버퍼)를 못 받으면 수집한 값은 버린다. 받은 프로브는 여기(측정 스레드)에서 해제된다.
        let probe = probe.ok_or(WORKER_UNRESPONSIVE)?;
        let per = analyze_echo(&probe.emitted_us, &probe.arrivals, &probe.arrival_us);
        let mut rep = self.report(per, m.delay_us, m.rtt, m.jitter, m.path);
        if probe.overflow > 0 && rep.verdict == MeasureVerdict::Ok {
            rep.verdict = MeasureVerdict::Unstable;
        }
        Ok(rep)
    }

    fn sleep(&self, d: Duration, cancel: &AtomicBool) -> Result<(), String> {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn run_inner(&self, id: u32, runs: u32, wait: Duration, cancel: &AtomicBool, installed: &mut bool) -> Result<EchoMaterials, String> {
        self.net.set_echo_request(&self.peer_id, true)?;
        let t0 = Instant::now();
        loop {
            if self.net.remote_echo(&self.peer_id)?.0 {
                break;
            }
            if t0.elapsed() > wait {
                return Err("상대가 에코 측정을 수락하지 않았습니다(시간 초과)".into());
            }
            self.sleep(Duration::from_millis(20), cancel)?;
        }
        // 동의를 받은 뒤에야 상대에게 내 소리 대신 무음(+ 클릭)을 보낸다. 상대가 허용 여부를 고르는 동안에는
        // 평소 소리를 듣는다(PR #13 L6). 동의 직후 잠깐은 내 실제 소리가 내게 되돌아오지만 나만 듣는다.
        // 프로브(인코더·탭 버퍼)는 여기, 측정 스레드에서 만들어 워커에 넘긴다 — 워커는 다음 반복에 받는다.
        let probe = EchoProbe::new(id, self.peer_id.clone(), self.peer_ssrc, self.stream, runs)?;
        self.port.install(probe)?;
        *installed = true;
        // 이제 상대가 보내는 것은 되돌아온 내 무음 스트림이다. 이전(상대 소리) 도착 분포를 버리고 다시 채운다.
        self.media.request_restart();
        self.sleep(ECHO_SETTLE, cancel)?;
        self.port.start_clicks(id, runs);
        let listen = Duration::from_secs_f64(echo_listen_s(runs));
        let t1 = Instant::now();
        let mut jitter_ms = Vec::new();
        while t1.elapsed() < listen {
            self.sleep(Duration::from_millis(100), cancel)?;
            // 워커가 발행한 스냅샷(락 없음, 엔진 ③-a1)
            let s = self.media.stats();
            if s.playing {
                jitter_ms.push(s.js.buffer_ms as f64);
            }
            if !self.net.remote_echo(&self.peer_id)?.0 {
                return Err("상대가 에코를 멈췄습니다".into());
            }
        }
        // 결과 수거 시점: 탭만 끈다(원자값 하나). 프로브는 워커에 남아 상대가 에코를 멈출 때까지 무음을 보낸다.
        self.port.tap_off(id);
        let (_, delay_us) = self.net.remote_echo(&self.peer_id)?;
        let rtt = self.net.peer_rtt_ms(&self.peer_id);
        let path = self.net.peer_path(&self.peer_id);
        let jitter = summarize(&jitter_ms).map_or(0.0, |s| s.0) as f32;
        Ok(EchoMaterials { delay_us, rtt, jitter, path })
    }

    fn report(
        &self,
        per: Vec<Option<f64>>,
        delay_us: u32,
        rtt: Option<f32>,
        jitter: f32,
        path: Option<band_dsp::types::PathKind>,
    ) -> EchoReport {
        let s = echo_summary(&per);
        let mut verdict = s.verdict;
        // 상대 보고값은 인증된 상대가 정한 값이다. 되돌리기는 디코드 없이 바로 하므로 수 ms를 넘을 수 없다.
        // 범위를 벗어나면 상한으로 자르고 불안정으로 표시한다(PR #13 L2).
        let peer_bogus = delay_us > MAX_PEER_ECHO_DELAY_US;
        if peer_bogus && verdict == MeasureVerdict::Ok {
            verdict = MeasureVerdict::Unstable;
        }
        let peer_ms = delay_us.min(MAX_PEER_ECHO_DELAY_US) as f64 / 1000.0;
        let sum = s.stats;
        let median = sum.map(|s| s.0);
        let one_way = median.map(|m| (m - peer_ms) / 2.0);
        let mouth = median.zip(rtt).map(|(m, r)| m - peer_ms - r as f64 / 2.0 + (self.input_buffer_ms + self.output_buffer_ms) as f64);
        let residual = median.zip(rtt).map(|(m, r)| m - peer_ms - r as f64 - self.frame_ms as f64 - jitter as f64);
        let message = echo_message(&s, one_way, peer_bogus.then_some(delay_us));
        EchoReport {
            verdict,
            message,
            peer_id: self.peer_id.clone(),
            detected_runs: s.detected as u32,
            used_runs: s.used as u32,
            excluded_runs: (s.detected - s.used) as u32,
            outlier_index: s.outlier.map(|i| i as u32),
            round_trip_ms: median,
            min_ms: sum.map(|s| s.1),
            max_ms: sum.map(|s| s.2),
            std_dev_ms: sum.map(|s| s.3),
            spread_ms: s.spread,
            peer_echo_delay_ms: peer_ms,
            one_way_ms: one_way,
            network_rtt_ms: rtt,
            jitter_buffer_ms: jitter,
            frame_ms: self.frame_ms,
            local_latency_ms: self.local_latency_ms,
            input_buffer_ms: self.input_buffer_ms,
            output_buffer_ms: self.output_buffer_ms,
            mouth_to_ear_ms: mouth,
            residual_ms: residual,
            path,
            runs: per,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_probe_emits_frame_aligned_clicks_and_detects_them() {
        let mut p = EchoProbe::new(1, "b".into(), 1, EchoStream { codec: CodecKind::Pcm16, channels: 1, opus_bitrate: 0, frame_samples: 120 }, 3).unwrap();
        p.start_clicks(3);
        let f = 120;
        let mut frame = vec![0f32; f];
        let mut stream = Vec::new();
        let mut now = 0.0;
        for _ in 0..(3 * CLICK_INTERVAL as usize / f + 10) {
            now += f as f64 * 1e6 / 48_000.0;
            p.fill_frame(&mut frame, now);
            stream.extend_from_slice(&frame);
        }
        assert_eq!(p.emitted_us.len(), 3);
        assert!(p.clicks_done());
        // 첫 클릭은 첫 프레임 시작(송신 시각 − 프레임 길이 = 0)
        assert!(p.emitted_us[0].abs() < 1e-6, "{:?}", p.emitted_us);
        assert!((p.emitted_us[1] - 300_000.0).abs() < 1e-6);
        // 23.4ms 늦게 재생되는 스트림으로 되돌아왔다고 하자.
        p.tap = true;
        let delay_us = 23_400.0;
        let stereo: Vec<f32> = stream.iter().flat_map(|&v| [v, v]).collect();
        for (n, ch) in stereo.chunks(128).enumerate() {
            p.record(ch, delay_us + (n * 64) as f64 * 1e6 / 48_000.0);
        }
        let per = analyze_echo(&p.emitted_us, &p.arrivals, &p.arrival_us);
        assert_eq!(per.len(), 3);
        for v in per {
            let v = v.expect("검출");
            assert!((v - 23.4).abs() < 0.05, "{v}");
        }
    }

    fn test_probe(id: u32) -> Box<EchoProbe> {
        EchoProbe::new(id, "b".into(), 1, EchoStream { codec: CodecKind::Pcm16, channels: 1, opus_bitrate: 0, frame_samples: 120 }, 2).unwrap()
    }

    /// 엔진 ②b-5: 워커 쪽 상태(`EchoRt`)가 측정 번호에 묶인 신호를 따른다. **탭을 꺼도 프로브는 `stop`까지 워커에 남는다**
    /// (그동안 그 피어에게 무음이 나간다 — 설계 3.3절 H1).
    #[test]
    fn echo_rt_follows_measure_signals() {
        let (port, rt) = echo_port();
        let mut rt = EchoRt::new(rt);
        let id = port.begin().unwrap();
        assert!(port.begin().is_err(), "측정은 하나씩");
        rt.poll();
        assert!(rt.probe_mut().is_none());
        port.install(test_probe(id)).unwrap();
        rt.poll();
        assert!(rt.probe_mut().is_some(), "받은 뒤로는 그 피어에게 무음을 보낸다");
        assert!(rt.tap_mut().is_none(), "클릭을 시작하기 전에는 탭이 꺼져 있다");
        port.start_clicks(id, 2);
        rt.poll();
        assert!(rt.tap_mut().is_some());
        // 클릭 하나를 내보낸 뒤 다시 poll해도 클릭을 처음부터 다시 시작하지 않는다(상태 판정: `clicks_started`)
        let p = rt.probe_mut().unwrap();
        assert!(p.encode_next(1_000.0).is_some());
        assert_eq!((p.emitted_us.len(), p.clicks_left), (1, 1));
        rt.poll();
        assert_eq!(rt.probe_mut().unwrap().emitted_us.len(), 1);
        // 결과 수거: 탭만 꺼진다. 프로브는 남는다.
        port.tap_off(id);
        rt.poll();
        assert!(rt.tap_mut().is_none());
        assert!(rt.probe_mut().is_some(), "탭 끝과 반환은 다른 시점이다");
        assert!(port.recover(id, Duration::ZERO).is_none());
        // 상대가 에코를 멈춘 뒤: 돌려준다. 수집한 값은 프로브 안에 그대로 있다.
        port.stop(id);
        rt.poll();
        assert!(rt.probe_mut().is_none());
        let back = port.recover(id, Duration::ZERO).expect("프로브가 측정 스레드로 돌아온다");
        assert_eq!((back.id, back.emitted_us.len()), (id, 1));
        port.end();
        assert_eq!(port.begin().unwrap(), id + 1, "측정 번호는 1씩 는다");
    }

    /// 설계 7.3절: 설치와 취소가 어떤 순서로 보여도 프로브가 워커에 갇히지 않는다(사건이 아니라 상태로 판정).
    #[test]
    fn echo_rt_returns_probe_whatever_order_install_and_stop_arrive() {
        let (port, rt) = echo_port();
        let mut rt = EchoRt::new(rt);
        // (1) 설치와 stop이 워커의 한 반복 사이에 다 일어남: 받은 그 poll에서 바로 돌려준다
        let id = port.begin().unwrap();
        port.install(test_probe(id)).unwrap();
        port.stop(id);
        rt.poll();
        assert!(rt.probe_mut().is_none());
        assert_eq!(port.recover(id, Duration::ZERO).map(|p| p.id), Some(id));
        port.end();
        // (2) 워커가 stop을 먼저 보고(쥔 것이 없어 아무 일도 없음) 그 뒤에 설치가 보임: 받은 poll에서 돌려준다
        let id = port.begin().unwrap();
        port.stop(id);
        rt.poll();
        port.install(test_probe(id)).unwrap();
        rt.poll();
        assert!(rt.probe_mut().is_none());
        assert_eq!(port.recover(id, Duration::ZERO).map(|p| p.id), Some(id));
        port.end();
        // (3) 탭 끝 없이 stop만(취소, 에러): 돌려준다
        let id = port.begin().unwrap();
        port.install(test_probe(id)).unwrap();
        rt.poll();
        port.start_clicks(id, 2);
        rt.poll();
        port.stop(id);
        rt.poll();
        assert_eq!(port.recover(id, Duration::ZERO).map(|p| p.id), Some(id));
        port.end();
        // (4) 탭을 끄라는 신호가 이미 있으면 뒤늦게 본 클릭 시작으로 탭을 다시 켜지 않는다
        let id = port.begin().unwrap();
        port.start_clicks(id, 2);
        port.tap_off(id);
        port.install(test_probe(id)).unwrap();
        rt.poll();
        assert!(rt.probe_mut().is_some() && rt.tap_mut().is_none());
        // (5) 지난 측정 번호의 신호는 새 프로브에 적용되지 않는다
        port.stop(id);
        rt.poll();
        let _ = port.recover(id, Duration::ZERO).unwrap();
        port.end();
        let id2 = port.begin().unwrap();
        port.install(test_probe(id2)).unwrap();
        rt.poll();
        assert!(rt.probe_mut().is_some(), "앞 측정의 stop({id})이 새 프로브({id2})를 돌려보내지 않는다");
        assert!(rt.tap_mut().is_none(), "앞 측정의 클릭 시작도 적용되지 않는다");
        rt.close();
        assert_eq!(port.recover(id2, Duration::ZERO).map(|p| p.id), Some(id2), "워커가 끝날 때 쥔 프로브를 돌려준다");
    }

    /// 워커는 프로브를 하나만 쥔다. 쥔 채로 새 것이 오면 명령 큐에 남았다가, 쥔 것을 돌려준 다음 반복에 받는다.
    /// 반환 큐가 차 있으면 떨어뜨리지 않고 쥔 채 다시 시도한다. `close`는 쥔 것과 큐에 남은 것을 모두 돌려준다.
    #[test]
    fn echo_rt_holds_one_probe_and_never_drops_it() {
        let (port, rt) = echo_port();
        let mut rt = EchoRt::new(rt);
        // 워커가 멈춘 동안 측정 둘이 지나갔다고 하자(첫 측정은 프로브를 못 받고 끝남).
        port.install(test_probe(1)).unwrap();
        port.install(test_probe(2)).unwrap();
        assert!(port.cmd.lock().try_push(test_probe(3)).is_err(), "큐 용량 2");
        port.stop(1);
        rt.poll();
        assert!(rt.probe_mut().is_none(), "앞의 것(1)은 받자마자 돌려준다");
        rt.poll();
        assert_eq!(rt.probe_mut().map(|p| p.id), Some(2), "다음 반복에 새 것을 받는다");
        // 반환 큐를 채운다(측정 스레드가 아직 안 비움): 1이 이미 있고 하나 더 넣는다
        assert!(rt.port.ret.try_push(test_probe(9)).is_ok());
        port.stop(2);
        rt.poll();
        assert_eq!(rt.probe_mut().map(|p| p.id), Some(2), "반환 큐가 차 있으면 쥔 채 다음 반복에 다시");
        // install이 반환 큐를 먼저 비운다 → 다음 poll에서 돌려줄 수 있다
        port.install(test_probe(4)).unwrap();
        rt.poll();
        assert!(rt.probe_mut().is_none());
        assert_eq!(port.recover(2, Duration::ZERO).map(|p| p.id), Some(2));
        // 4는 큐에 있다. close가 돌려준다.
        rt.close();
        assert_eq!(port.recover(4, Duration::ZERO).map(|p| p.id), Some(4));
    }

    /// 설계 6.3절, 7.3절 첫 행: 워커가 계속 도는 동안 설치, 클릭 시작, 탭 끝, 취소를 무작위 간격으로 되풀이해도 모든
    /// 프로브가 측정 스레드로 돌아온다(한 번이라도 못 받으면 실패).
    #[test]
    fn echo_rt_random_install_cancel_always_recovers_probe() {
        use rand::{Rng, SeedableRng};
        let (port, rt) = echo_port();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut rt = EchoRt::new(rt);
                let mut rng = rand::rngs::StdRng::seed_from_u64(9);
                let mut frames = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    rt.poll();
                    if let Some(p) = rt.probe_mut() {
                        let _ = p.encode_next(frames as f64);
                        frames += 1;
                    }
                    if let Some(p) = rt.tap_mut() {
                        p.record(&[0.0; 128], 0.0);
                    }
                    // 워커 반복 간격을 흉내 낸다(가끔 길게)
                    match rng.gen_range(0..20) {
                        0 => std::thread::sleep(Duration::from_micros(300)),
                        1..=5 => std::thread::yield_now(),
                        _ => {}
                    }
                }
                rt.close();
                frames
            })
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(4);
        let wait = |rng: &mut rand::rngs::StdRng| match rng.gen_range(0..6) {
            0 => std::thread::sleep(Duration::from_micros(rng.gen_range(1..400))),
            1 | 2 => std::thread::yield_now(),
            _ => {}
        };
        for round in 0..1_500 {
            let id = port.begin().unwrap();
            port.install(test_probe(id)).unwrap_or_else(|e| panic!("{round}회째 설치: {e}"));
            wait(&mut rng);
            if rng.gen_bool(0.6) {
                port.start_clicks(id, 2);
                wait(&mut rng);
            }
            if rng.gen_bool(0.5) {
                port.tap_off(id);
                wait(&mut rng);
            }
            port.stop(id);
            let back = port.recover(id, Duration::from_secs(5)).unwrap_or_else(|| panic!("{round}회째 프로브({id})가 돌아오지 않음"));
            assert_eq!(back.id, id);
            port.end();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap() > 0, "워커가 프로브를 실제로 썼다");
    }

    /// 탭 버퍼는 용량 안에서만 찬다(워커가 늘리지 않는다). 넘친 샘플은 세고 버린다.
    #[test]
    fn echo_probe_tap_is_bounded() {
        let mut p = test_probe(1);
        p.start_clicks(2);
        let cap = p.tap_cap;
        let caps = (p.arrivals.capacity(), p.arrival_us.capacity());
        assert!(cap >= ((echo_listen_s(2) + ECHO_TAP_SLACK_S) * 48_000.0) as usize && cap < 96_000, "{cap}");
        let chunk = [0.25f32; 128];
        for _ in 0..cap / 64 + 10 {
            p.record(&chunk, 0.0);
        }
        assert_eq!((p.arrivals.len(), p.arrival_us.len()), (cap, cap));
        assert_eq!((p.arrivals.capacity(), p.arrival_us.capacity()), caps, "버퍼가 늘지 않았다");
        assert!(p.overflow > 0);
        // 클릭 송신 시각도 용량(상한 20) 안에서만
        assert!(p.emitted_us.capacity() >= ECHO_MAX_RUNS as usize);
    }
}
