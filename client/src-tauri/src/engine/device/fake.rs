//! 실제 장치 없이 엔진 전체를 돌리기 위한 가짜 오디오 장치.
//!
//! 실제 장치처럼 입력과 출력을 **별도 스레드**에서 각자의 클럭으로 호출한다(리뷰 "테스트 신뢰도" 1):
//! - 입력/출력 버퍼 크기, 레이트, 클럭 오차(ppm)를 따로 줄 수 있다.
//! - 출력 시작 위상(`out_phase_us`)과 콜백 기상 지연(`wake_jitter_us`, 늦으면 따라잡으며
//!   몰아서 호출 = 버스트)을 흉내 낸다.
//! - 열기 지연(`open_delay`)과 스트림 에러 주입(`error_after`)을 지원한다.
//! - 열기 문(`open_gate`): 테스트가 장치 열기를 붙잡아 두고 원하는 시점에 놓는다(벽시계 대기 없는 결정적 동기화).
//!
//! 시간 모델: 입력 샘플 i는 `t0 + i/in_rate`에 캡처되고, 출력 샘플 j(=sink 인덱스/채널)는
//! `t0 + out_phase + j/out_rate`에 재생된다. 따라서 임펄스 지연 = out_phase + j/out_rate − i/in_rate.

use std::any::Any;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::engine::config::{DriverKind, EngineConfig};
use crate::engine::rt::{InputRt, OutputRt};
use crate::engine::stats::AudioCounters;

use super::backend::AudioBackend;
use super::types::*;
use parking_lot::{Condvar, Mutex};
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::{Duration, Instant};

/// 장치 열기를 테스트가 붙잡아 두는 문. `open`은 들어온 횟수를 올리고, 문이 닫혀 있으면 열릴 때까지 기다린다.
/// "시작 중(장치 열기 중)" 구간을 `open_delay`(벽시계) 대신 이것으로 만들면 러너 속도와 무관하게 재현된다.
#[derive(Default)]
pub struct OpenGate {
    /// (들어온 횟수, 닫힘)
    st: Mutex<(u32, bool)>,
    cv: Condvar,
}

impl OpenGate {
    /// 닫힌 문.
    pub fn closed() -> Arc<Self> {
        let g = Self::default();
        g.st.lock().1 = true;
        Arc::new(g)
    }

    pub fn close(&self) {
        self.st.lock().1 = true;
    }

    pub fn release(&self) {
        self.st.lock().1 = false;
        self.cv.notify_all();
    }

    /// 지금까지 `open`이 문에 들어온 횟수.
    pub fn entered(&self) -> u32 {
        self.st.lock().0
    }

    /// 들어온 횟수가 `n` 이상이 될 때까지 기다린다. `timeout`은 행(hang) 감지용 상한이다.
    pub fn wait_entered(&self, n: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.st.lock();
        while g.0 < n {
            if self.cv.wait_until(&mut g, deadline).timed_out() {
                return g.0 >= n;
            }
        }
        true
    }

    fn pass(&self) {
        let mut g = self.st.lock();
        g.0 += 1;
        self.cv.notify_all();
        while g.1 {
            self.cv.wait(&mut g);
        }
    }
}

/// 입력 신호 생성기: (절대 프레임 인덱스, 채널 수, 인터리브 버퍼).
pub type SourceFn = dyn Fn(u64, usize, &mut [f32]) + Send + Sync;

#[derive(Clone)]
pub struct FakeBackend {
    pub in_rate: u32,
    pub out_rate: u32,
    pub channels: usize,
    /// 입력 콜백 크기. None이면 설정의 bufferSize.
    pub in_buffer: Option<usize>,
    /// 출력 콜백 크기. None이면 설정의 bufferSize.
    pub out_buffer: Option<usize>,
    /// 장치 클럭 오차(ppm). 양수면 실제보다 빠르게 돈다.
    pub in_drift_ppm: f64,
    pub out_drift_ppm: f64,
    /// 출력 스레드 시작 지연(µs).
    pub out_phase_us: u64,
    /// 콜백마다 0..이 값(µs) 사이로 늦게 깨어난다. 다음 콜백은 원래 일정으로 따라잡는다.
    pub wake_jitter_us: u64,
    /// 출력 콜백이 격자보다 0..이 값(µs) **이르게** 불린다(실장비 WASAPI 독점 프로브의 이른 콜백, PR #11 M3).
    /// `wake_jitter_us`와 합치면 격자 기준 ± 지터. 재생 시각(sink 모델)은 격자 그대로다.
    pub out_early_us: u64,
    pub open_delay: Duration,
    /// 있으면 장치 열기가 이 문을 지난다(`OpenGate`).
    pub open_gate: Option<Arc<OpenGate>>,
    /// 이 시간 뒤 스트림 에러 1회 보고(장치 분리 흉내).
    pub error_after: Option<Duration>,
    pub source: Arc<SourceFn>,
    /// 출력 콜백이 기록한 샘플(인터리브, `channels`채널).
    pub sink: Arc<Mutex<Vec<f32>>>,
    pub record_output: bool,
    pub seed: u64,
    /// (주기, 멈춤): 출력 콜백 스레드가 주기마다 멈췄다가 몰아서 따라잡는다 → 출력 언더런 유발.
    pub out_stall: Option<(Duration, Duration)>,
    /// 이 시간 뒤 입력·출력 콜백이 멈춘다(장치 분리 흉내).
    pub halt_after: Option<Duration>,
    /// 공유하면 동시에 한 번만 열 수 있다(배타 장치 흉내).
    pub exclusive: Option<Arc<AtomicBool>>,
    /// 클럭 원점. 두 가짜 장치가 같은 원점을 쓰면 입력 인덱스와 출력 인덱스로 종단 간 지연을 잴 수 있다.
    /// None이면 열 때의 시각. 원점이 과거면 콜백은 현재 시각에 해당하는 인덱스부터 시작한다.
    pub t0: Option<Instant>,
    /// 출력 sink 첫 샘플의 절대 프레임 인덱스(원점 기준).
    /// sink 프레임 j의 재생 시각 = t0 + out_phase + (origin + j)/out_rate.
    pub sink_origin: Arc<AtomicU64>,
    /// `StreamInfo`에 실을 "드라이버 보고 지연"(입력, 출력 프레임). 기본 (0, 0) = 알 수 없음.
    pub reported_latency: (u32, u32),
}

impl FakeBackend {
    pub fn new(rate: u32, source: Arc<SourceFn>) -> Self {
        Self {
            in_rate: rate,
            out_rate: rate,
            channels: 2,
            in_buffer: None,
            out_buffer: None,
            in_drift_ppm: 0.0,
            out_drift_ppm: 0.0,
            out_phase_us: 0,
            wake_jitter_us: 0,
            out_early_us: 0,
            open_delay: Duration::ZERO,
            open_gate: None,
            error_after: None,
            source,
            sink: Arc::new(Mutex::new(Vec::new())),
            record_output: true,
            seed: 1,
            out_stall: None,
            halt_after: None,
            exclusive: None,
            t0: None,
            sink_origin: Arc::new(AtomicU64::new(0)),
            reported_latency: (0, 0),
        }
    }

    pub fn silence(rate: u32) -> Self {
        Self::new(rate, Arc::new(|_, _, buf: &mut [f32]| buf.fill(0.0)))
    }

    /// 입출력 모두 같은 클럭 오차(같은 장치의 듀플렉스).
    pub fn with_drift(mut self, ppm: f64) -> Self {
        self.in_drift_ppm = ppm;
        self.out_drift_ppm = ppm;
        self
    }
}

struct FakeGuard {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    exclusive: Option<Arc<AtomicBool>>,
}

impl Drop for FakeGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Some(ex) = &self.exclusive {
            ex.store(false, Ordering::SeqCst);
        }
    }
}

/// `t0 + offset + period·(k + first)`에 `f(k)`를 부르는 실시간 루프.
/// 기상 지연은 `jitter_us` 이내 난수이며, 늦어지면 다음 호출들이 몰려서(버스트) 따라잡는다.
struct LoopCfg {
    t0: Instant,
    offset: Duration,
    period: f64,
    first: u64,
    jitter_us: u64,
    /// 0..이 값(µs)만큼 이르게 부른다.
    early_us: u64,
    seed: u64,
    /// (주기, 멈춤): 주기마다 콜백 스레드가 이만큼 멈췄다가 몰아서 따라잡는다(CPU 스파이크 흉내).
    stall: Option<(Duration, Duration)>,
    /// 이 시간 뒤 콜백을 더 부르지 않는다(장치 분리 흉내).
    halt_after: Option<Duration>,
}

fn clocked_loop(stop: Arc<AtomicBool>, c: LoopCfg, mut f: impl FnMut(u64)) {
    // 실제 장치 콜백 스레드(cpal WASAPI/ASIO)는 시간 임계 우선순위로 돈다. 가짜도 같게 둔다.
    let _prio = crate::sys::prio::promote_current_thread();
    let mut rng = rand::rngs::StdRng::seed_from_u64(c.seed);
    let mut k: u64 = 0;
    let mut next_stall = c.stall.map(|(every, _)| every);
    while !stop.load(Ordering::Relaxed) {
        if c.halt_after.is_some_and(|h| c.t0.elapsed() >= h) {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if let (Some(at), Some((every, len))) = (next_stall, c.stall) {
            if c.t0.elapsed() >= at {
                std::thread::sleep(len);
                next_stall = Some(at + every);
            }
        }
        let jitter = if c.jitter_us > 0 { Duration::from_micros(rng.gen_range(0..c.jitter_us)) } else { Duration::ZERO };
        let early = if c.early_us > 0 { Duration::from_micros(rng.gen_range(0..c.early_us)) } else { Duration::ZERO };
        let grid = c.t0 + c.offset + Duration::from_secs_f64(c.period * (k + c.first) as f64);
        let due = grid.checked_sub(early).unwrap_or(grid) + jitter;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        f(k);
        k += 1;
    }
}

impl AudioBackend for FakeBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        let mk = |is_input| DeviceInfo {
            id: device_id(DriverKind::WasapiShared, "Fake"),
            name: "Fake".into(),
            driver: DriverKind::WasapiShared,
            is_input,
            is_default: true,
            channels: self.channels as u16,
            sample_rates: vec![if is_input { self.in_rate } else { self.out_rate }],
        };
        Ok(vec![mk(true), mk(false)])
    }

    fn open(
        &self,
        cfg: &EngineConfig,
        mut input: InputRt,
        mut output: OutputRt,
        counters: Arc<AudioCounters>,
    ) -> Result<(StreamInfo, Box<dyn Any>), String> {
        if input.required_device_channels() > self.channels {
            return Err("가짜 장치 채널 부족".into());
        }
        // 배타 장치 흉내: 이미 열려 있으면 실패(ASIO 드라이버 1개, WASAPI 배타, 고정 포트와 같은 성질)
        if let Some(ex) = &self.exclusive {
            if ex.swap(true, Ordering::SeqCst) {
                return Err("장치가 이미 사용 중입니다".into());
            }
        }
        std::thread::sleep(self.open_delay);
        if let Some(g) = &self.open_gate {
            g.pass();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let ch = self.channels;
        let in_buf = self.in_buffer.unwrap_or(cfg.buffer_size as usize).max(1);
        let out_buf = self.out_buffer.unwrap_or(cfg.buffer_size as usize).max(1);
        let t0 = self.t0.unwrap_or_else(Instant::now);
        // 원점이 과거면 지금 시각에 해당하는 콜백 인덱스부터 시작한다(밀린 콜백을 몰아서 부르지 않음).
        let start_k = move |period: f64, offset: f64| -> u64 {
            let el = t0.elapsed().as_secs_f64() - offset;
            if el <= 0.0 {
                0
            } else {
                (el / period).ceil() as u64
            }
        };

        let me = self.clone();
        let st = stop.clone();
        let in_period = in_buf as f64 / me.in_rate as f64 / (1.0 + me.in_drift_ppm * 1e-6);
        let in_thread = std::thread::Builder::new()
            .name("band-fake-in".into())
            .spawn(move || {
                let mut buf = vec![0f32; in_buf * ch];
                let seed = me.seed;
                let k0 = start_k(in_period, 0.0);
                // 입력 콜백 k는 블록 [k, k+1)×in_buf의 캡처가 끝난 t0 + (k+1)·period에 불린다.
                let lc = LoopCfg {
                    t0,
                    offset: Duration::ZERO,
                    period: in_period,
                    first: 1 + k0,
                    jitter_us: me.wake_jitter_us,
                    early_us: 0,
                    seed,
                    stall: None,
                    halt_after: me.halt_after,
                };
                clocked_loop(st, lc, |k| {
                    (me.source)((k + k0) * in_buf as u64, ch, &mut buf);
                    input.process(&buf, ch);
                });
            })
            .map_err(|e| format!("가짜 입력 스레드 생성 실패: {e}"))?;

        let me = self.clone();
        let st = stop.clone();
        let out_period = out_buf as f64 / me.out_rate as f64 / (1.0 + me.out_drift_ppm * 1e-6);
        let spawn_out = std::thread::Builder::new().name("band-fake-out".into()).spawn(move || {
            let mut buf = vec![0f32; out_buf * ch];
            let mut errored = false;
            let phase = Duration::from_micros(me.out_phase_us);
            let seed = me.seed.wrapping_add(17);
            let k0 = start_k(out_period, phase.as_secs_f64());
            me.sink_origin.store(k0 * out_buf as u64, Ordering::SeqCst);
            // 출력 콜백 k는 t0 + phase + k·period에 불려 재생 구간 [k, k+1)×out_buf를 채운다.
            let lc = LoopCfg {
                t0,
                offset: phase,
                period: out_period,
                first: k0,
                jitter_us: me.wake_jitter_us,
                early_us: me.out_early_us,
                seed,
                stall: me.out_stall,
                halt_after: me.halt_after,
            };
            clocked_loop(st, lc, |_k| {
                output.process(&mut buf, ch);
                if me.record_output {
                    me.sink.lock().extend_from_slice(&buf);
                }
                if !errored && me.error_after.is_some_and(|d| t0.elapsed() >= d) {
                    errored = true;
                    counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                }
            });
        });
        let out_thread = match spawn_out {
            Ok(t) => t,
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                let _ = in_thread.join();
                return Err(format!("가짜 출력 스레드 생성 실패: {e}"));
            }
        };
        let info = StreamInfo {
            in_rate: self.in_rate,
            out_rate: self.out_rate,
            in_channels: ch,
            out_channels: ch,
            buffer_frames: cfg.buffer_size,
            in_backend: BackendKind::Fake,
            out_backend: BackendKind::Fake,
            in_period_frames: in_buf as u32,
            out_period_frames: out_buf as u32,
            in_reported_latency_frames: self.reported_latency.0,
            out_reported_latency_frames: self.reported_latency.1,
            in_device_tag: device_tag(cfg.input_device.as_deref().unwrap_or("fake::default")),
            out_device_tag: device_tag(cfg.output_device.as_deref().unwrap_or("fake::default")),
        };
        Ok((info, Box::new(FakeGuard { stop, threads: vec![in_thread, out_thread], exclusive: self.exclusive.clone() })))
    }
}
