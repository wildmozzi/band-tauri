//! 엔진 워커(비RT 스레드 1개, 설계 문서 1.1절).
//!
//! - 입력 링 → (리샘플) → 프레임 단위 인코드 → 피어별 암호화/송신
//! - 출력(콜백 구동 믹스, P1-0): 믹스는 출력 콜백이 한다(`rt.rs`). 워커는 소스(로컬 1 + 피어별)마다
//!   디코드된 PCM을 소스 링에 공급한다.
//!   - 피어: 받은 데이터는 미리 꺼내 두고, 은닉(PLC)은 출력 콜백 마감 직전에만 한다(`PeerFeed`).
//!     출력 콜백이 돌 때마다 지터 버퍼 깊이를 **재생 위치**(소스 링 머리) 기준으로 잰다.
//!   - 로컬(모니터·탭): 콜백이 소비한 만큼 `OUT_CHUNK` 단위로 로컬 FIFO에서 꺼내 다시 채운다.
//! - 깊이 측정 시각은 워커 기상 시각이 아니라 `DeviceClock`이 추정한 출력 콜백 시각이다.
//!
//! ## 파일
//! - `mod.rs`: `Worker` 조립과 루프(입력 펌프·송신·자동 코덱·메트로놈·피어 슬롯·로컬 공급·장치 감시), 정지
//! - `clock.rs`: 출력 콜백 시각 추정(`DeviceClock`, `CallbackTracker`, `FeedTiming`)
//! - `local.rs`: 로컬 입력 FIFO(`LocalFifo`, 모니터·믹스 탭)
//! - `peer_feed.rs`: 피어 소스 공급(`PeerFeed`, 콜백 구동 믹스 규칙)
//! - `tests.rs`: 결정적 단위 시뮬레이션

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, CodecKind, EngineConfig, INTERNAL_RATE, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use super::device::StreamInfo;

use super::measure::EchoProbe;
use super::metronome::{choose_hidden_out, hidden_out_latency_us, measured_hidden_out_us, BeatScheduler, MetronomeCtl, MetronomePlan, OutClock};
use super::mixer::MixControls;
use super::record::{MarkEvent, MarkKind, MarkSink};
use super::relay_codec::{auto_opus_perc, auto_reason, pcm_relay_load, LossCodecPolicy, PolicyInputs, RelayCodecPolicy, DEFAULT_OPUS_LOSS_PERC};
use super::resample::{StreamResampler, CHUNK as RESAMPLE_CHUNK};
use super::rt::{SourceBank, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN, SLOT_FREE};
use super::stats::{AudioCounters, PathKind};
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::peer::Peer;
use crate::net::{AutoPayload, NetShared, MAX_PEERS};

mod clock;
mod local;
mod peer_feed;
#[cfg(test)]
mod tests;

pub use clock::{CallbackTracker, DeviceClock, FeedTiming};
pub use local::LocalFifo;
pub use peer_feed::{FeedEvents, PeerFeed, DEADLINE_GUARD_US};

/// 워커 폴링 주기. Windows에서도 std::thread::sleep은 고해상도 타이머를 쓴다.
pub(super) const POLL: Duration = Duration::from_micros(500);
/// 피어 은닉 마감이 가까울 때의 폴링 주기.
pub(super) const FINE_POLL: Duration = Duration::from_micros(150);
/// 피어별 Opus 자동 전환 정책(릴레이 부하 P1-4c, 손실 피드백 P1-4e)을 다시 보는 간격.
const AUTO_CODEC_CHECK: Duration = Duration::from_millis(200);
/// 출력 생산 단위(48kHz 프레임). 1.33ms.
pub const OUT_CHUNK: usize = 64;

/// 워커 → 엔진 통계 공유값.
#[derive(Debug, Default)]
pub struct WorkerStats {
    /// 모니터 경로 점유량 시간평균(µs): 입력 리샘플러 + 로컬 FIFO + 로컬 리샘플러 + 로컬 소스 링.
    pub monitor_occupancy_us: AtomicU64,
    pub local_corrections: AtomicU64,
    pub local_underruns: AtomicU64,
    /// 피어 소스를 출력 콜백 마감 직전에 은닉으로 채운 횟수(패킷이 마감까지 안 옴).
    pub deadline_fills: AtomicU64,
    /// 피어 소스 링이 모자라 콜백이 0을 채우고, 그만큼 스트림을 건너뛴 장치 프레임 누적(워커 마감 놓침).
    pub skipped_frames: AtomicU64,
    /// 입력 또는 출력 콜백이 1초 넘게 멈췄다(장치 분리 등). 다시 돌면 false.
    pub device_stalled: AtomicBool,
    /// 워커 스레드 우선순위 설정 결과(P1-2).
    pub thread_prio: super::prio::PrioSlot,
}

/// 콜백이 이 시간 동안 멈추면 장치를 잃은 것으로 본다.
const DEVICE_STALL: Duration = Duration::from_secs(1);


// ============================================================================
// 워커
// ============================================================================

pub struct WorkerParams {
    pub cfg: EngineConfig,
    pub info: StreamInfo,
    pub in_cons: HeapCons<f32>,
    /// 소스 링 생산자(슬롯별)와 공유 상태. 믹스 탭 소비자는 엔진이 가진다.
    pub prods: Vec<HeapProd<f32>>,
    pub bank: Arc<SourceBank>,
    pub counters: Arc<AudioCounters>,
    pub controls: Arc<MixControls>,
    pub net: Option<Arc<NetShared>>,
    pub stop: Arc<AtomicBool>,
    pub stats: Arc<WorkerStats>,
    /// 지터 버퍼 수신 시각과 같은 기준(µs)의 시계.
    pub epoch: Instant,
    /// 에코 측정(P1-1): 있으면 그 피어에게 내 입력 대신 무음 + 클릭을 보내고, 그 피어 소리의 재생 시각을 모은다.
    pub probe: Arc<parking_lot::Mutex<Option<EchoProbe>>>,
    /// 공유 메트로놈 계획·상태(P2-3).
    pub metronome: Arc<MetronomeCtl>,
    /// 녹음 마커 큐(P2-4a): 녹음 중이면 피어별 은닉·버림을 출력 타임라인 위치로 넣는다.
    pub rec_marks: Arc<MarkSink>,
}

pub struct Worker {
    p: WorkerParams,
    beats: BeatScheduler,
    metro_plan: (u64, MetronomePlan),
    metro_stats_at: u64,
    metro_stats_gen: u64,
    frame: usize,
    ich: usize,
    in_rs: StreamResampler,
    /// 로컬(모니터) 소스: 48k → 장치 레이트.
    local_rs: StreamResampler,
    encoder: Option<Encoder>,
    /// 에코 측정 대상 피어용 인코더(측정 중에만 있음). 다른 피어와 코덱 상태를 섞지 않는다.
    alt_encoder: Option<Encoder>,
    alt_frame: Vec<f32>,
    alt_payload: Vec<u8>,
    /// PCM16 설정의 피어별 Opus 자동 전환(릴레이 부하 P1-4c, 수신 손실 피드백 P1-4e). 피어마다 전용 인코더라
    /// 전환 순간 예열과 예상 손실(%)이 피어별이다. 네트워크가 있고 설정이 PCM16일 때만 쓴다.
    auto: Vec<AutoTx>,
    /// 미리 만들어 둔 Opus 인코더(피어 수만큼, 시작 시). 새 피어가 자동 전환 대상이 될 때 생성 지연이 없게 한다.
    auto_pool: Vec<Encoder>,
    auto_enabled: bool,
    auto_flags: u8,
    /// 직전 캡처 프레임(전환 시 인코더 예열용).
    relay_prev: Vec<f32>,
    relay_policy: RelayCodecPolicy,
    auto_check_at: Duration,
    flags: u8,
    frame_code: u8,
    ts: u32,
    tracker: CallbackTracker,
    local: LocalFifo,
    feeds: Vec<PeerFeed>,
    in_tmp: Vec<f32>,
    /// 48k 입력 블록(리샘플 직후, 내부 채널).
    in48_buf: Vec<f32>,
    /// 인코더용 프레임 누적.
    cap_acc: VecDeque<f32>,
    frame_buf: Vec<f32>,
    stereo_buf: Vec<f32>,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    local_buf: Vec<f32>,
    chunk_buf: Vec<f32>,
    out_tmp: Vec<f32>,
    reported_errors: u32,
    encode_errors: u64,
    monitor_ewma_us: f64,
    safety: u64,
    seen_xruns: u64,
    last_xrun: Duration,
    last_played: u64,
    last_played_at: Duration,
    last_captured: u64,
    last_captured_at: Duration,
    slot_full_logged: bool,
}

/// 피어 하나의 Opus 자동 전환 송신 상태(PCM16 설정, P1-4c·P1-4e). 키는 `PeerFeed`와 같은 (피어 주소, ssrc).
struct AutoTx {
    key: (usize, u32),
    /// 피어 id(에코 측정 클릭 스트림 대상인지 가릴 때, L2).
    id: String,
    enc: Encoder,
    payload: Vec<u8>,
    /// 이번 프레임의 Opus 페이로드 길이(대상이 아니거나 인코드 실패면 None → 설정 코덱으로 보낸다).
    len: Option<usize>,
    loss: LossCodecPolicy,
    /// 손실 피드백 전환이 켜짐(`loss.active()`, 또는 측정 도구의 강제 `force_auto_opus`).
    loss_on: bool,
    /// 직전 점검 때 오디오를 릴레이로 보내는 피어였다(릴레이 부하 정책의 대상).
    on_relay: bool,
    /// 직전 프레임에서 이 인코더로 인코드했다(아니었다가 대상이 되면 초기화·예열한다).
    encoding: bool,
    /// 인코더에 설정한 예상 손실(%).
    perc: u8,
    seen: bool,
}

impl AutoTx {
    fn new(key: (usize, u32), id: String, mut enc: Encoder) -> Self {
        let _ = enc.set_loss_perc(DEFAULT_OPUS_LOSS_PERC);
        Self {
            key,
            id,
            enc,
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            len: None,
            loss: LossCodecPolicy::default(),
            loss_on: false,
            on_relay: false,
            encoding: false,
            perc: DEFAULT_OPUS_LOSS_PERC,
            seen: true,
        }
    }
}

/// `acc`에서 `frame.len()` 샘플짜리 프레임을 가능한 만큼 꺼내 `f(프레임, 시작 시각)`에 넘긴다. 한 패스에서 나온
/// 프레임은 모두 같은 `pass_at`을 받는다(리뷰 M1: 프레임마다 `now()`를 찍으면 앞 프레임 뒤에서 기다린 시간이 빠진다).
fn drain_frames(acc: &mut VecDeque<f32>, frame: &mut [f32], pass_at: Instant, mut f: impl FnMut(&[f32], Instant)) -> usize {
    let n = frame.len();
    let mut count = 0;
    while n > 0 && acc.len() >= n {
        for (d, s) in frame.iter_mut().zip(acc.drain(..n)) {
            *d = s;
        }
        f(frame, pass_at);
        count += 1;
    }
    count
}

/// 지원하는 장치 레이트 범위.
const MIN_DEVICE_RATE: u32 = 8_000;
const MAX_DEVICE_RATE: u32 = 384_000;

impl Worker {
    /// 리샘플러/인코더 생성. 실패하면 엔진 시작을 실패시킨다(리뷰 M10).
    pub fn new(p: WorkerParams) -> Result<Self, String> {
        let frame = p.cfg.frame_samples();
        let ich = p.cfg.channels as usize;
        for (what, r) in [("입력", p.info.in_rate), ("출력", p.info.out_rate)] {
            if !(MIN_DEVICE_RATE..=MAX_DEVICE_RATE).contains(&r) {
                return Err(format!("지원하지 않는 {what} 장치 레이트: {r}Hz"));
            }
        }
        if p.prods.len() != MAX_SOURCES {
            return Err("소스 링 수가 맞지 않습니다".into());
        }
        let in_rs = StreamResampler::new(p.info.in_rate, INTERNAL_RATE, ich)?;
        let local_rs = StreamResampler::new(INTERNAL_RATE, p.info.out_rate, 2)?;
        let encoder = if p.net.is_some() {
            Some(Encoder::new(p.cfg.codec, ich, p.cfg.opus_bitrate())?)
        } else {
            None
        };
        let mut flags = 0;
        if encoder.as_ref().is_some_and(|e| e.is_opus()) {
            flags |= FLAG_OPUS;
        }
        if ich == 2 {
            flags |= FLAG_STEREO;
        }
        let frame_code = frame_code_from_samples(frame).ok_or("잘못된 프레임 크기")?;
        let auto_enabled = p.net.is_some() && p.cfg.codec == CodecKind::Pcm16;
        let mut auto_pool = Vec::with_capacity(MAX_PEERS);
        if auto_enabled {
            for _ in 0..MAX_PEERS {
                auto_pool.push(Encoder::new(CodecKind::Opus, ich, p.cfg.opus_bitrate())?);
            }
        }
        let auto_flags = FLAG_OPUS | if ich == 2 { FLAG_STEREO } else { 0 };
        // 로컬 소스는 항상 켜져 있다(모니터 꺼짐 = 버스 게인 0, 탭에는 계속 나감).
        let local_slot = &p.bank.slots[LOCAL_SOURCE];
        local_slot.set_gains((0.0, 0.0), (1.0, 1.0));
        local_slot.state.store(SLOT_ACTIVE, Ordering::Release);
        Ok(Self {
            frame,
            ich,
            in_rs,
            local_rs,
            encoder,
            alt_encoder: None,
            alt_frame: vec![0.0; frame * ich],
            alt_payload: vec![0u8; MAX_PAYLOAD_BYTES],
            auto: Vec::with_capacity(MAX_PEERS),
            auto_pool,
            auto_enabled,
            auto_flags,
            relay_prev: vec![0.0; frame * ich],
            relay_policy: RelayCodecPolicy::default(),
            auto_check_at: Duration::ZERO,
            flags,
            frame_code,
            ts: rand::random(),
            tracker: CallbackTracker::new(p.info.out_rate),
            local: LocalFifo::new(),
            feeds: Vec::with_capacity(MAX_SOURCES),
            in_tmp: vec![0.0; 4096],
            in48_buf: vec![0.0; 2048 * ich],
            cap_acc: VecDeque::with_capacity((frame + 4096) * ich),
            frame_buf: vec![0.0; frame * ich],
            stereo_buf: vec![0.0; 2048 * 2],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            local_buf: vec![0.0; OUT_CHUNK * 2],
            chunk_buf: vec![0.0; OUT_CHUNK * 2],
            out_tmp: vec![0.0; 8192],
            reported_errors: 0,
            encode_errors: 0,
            monitor_ewma_us: 0.0,
            safety: 0,
            seen_xruns: 0,
            last_xrun: Duration::ZERO,
            last_played: 0,
            last_played_at: Duration::ZERO,
            last_captured: 0,
            last_captured_at: Duration::ZERO,
            slot_full_logged: false,
            beats: BeatScheduler::new(),
            metro_plan: (0, MetronomePlan::default()),
            metro_stats_at: 0,
            metro_stats_gen: 0,
            p,
        })
    }

    pub fn run(mut self) {
        let _prio = super::prio::promote_engine_thread(&self.p.stats.thread_prio);
        log::info!("워커 시작: frame {} ch {}", self.frame, self.ich);
        while !self.p.stop.load(Ordering::Relaxed) {
            self.reconfigure_local();
            self.pump_input();
            let timing = self.timing();
            self.service_metronome(&timing);
            let urgent = self.service_peers(&timing);
            self.service_local();
            self.track_occupancy();
            self.watch_device();
            let errs = self.p.counters.stream_errors.load(Ordering::Relaxed);
            if errs != self.reported_errors {
                log::warn!("오디오 스트림 에러 누적 {}회(장치 분리 등)", errs);
                self.reported_errors = errs;
            }
            // 은닉 마감이 가까우면 짧게 잔다(Windows 고해상도 타이머라도 0.5ms 폴링은 마감을 넘길 수 있다).
            std::thread::sleep(if urgent { FINE_POLL } else { POLL });
        }
        // 정지: 피어 슬롯을 닫는다(콜백이 아직 돌면 남은 소리를 버린다).
        for f in self.feeds.drain(..) {
            let sh = &self.p.bank.slots[f.slot];
            sh.playing.store(false, Ordering::Relaxed);
            sh.state.store(SLOT_DRAIN, Ordering::Release);
        }
        log::info!("워커 종료");
    }

    fn to48(frames: u32, rate: u32) -> usize {
        (frames as u64 * INTERNAL_RATE as u64).div_ceil(rate.max(1) as u64) as usize
    }

    fn in_burst48(&self) -> usize {
        let n = match self.p.counters.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.p.info.buffer_frames,
            n => n,
        };
        Self::to48(n, self.p.info.in_rate)
    }

    fn out_burst_dev(&self) -> u64 {
        match self.p.counters.out_cb_frames.load(Ordering::Relaxed) {
            0 => match self.p.info.out_period_frames {
                0 => self.p.info.buffer_frames as u64,
                n => n as u64,
            },
            n => n as u64,
        }
    }

    fn reconfigure_local(&mut self) {
        let out48 = Self::to48(self.out_burst_dev() as u32, self.p.info.out_rate);
        // 모니터/탭은 프레임 누적 전에 분기하므로 입력 방출 단위는 리샘플러 청크(패스스루면 1)다.
        let granularity = if self.in_rs.is_passthrough() { 1 } else { RESAMPLE_CHUNK };
        self.local.configure(self.in_burst48(), out48, granularity);
    }

    /// 입력·출력 콜백이 1초 넘게 멈추면 장치를 잃은 것으로 본다(리뷰 M9 / PR #5 M-2).
    /// cpal 에러 콜백은 비치명 에러도 보내므로 그것만으로는 판단하지 않는다.
    fn watch_device(&mut self) {
        let now = self.p.epoch.elapsed();
        let played = self.p.counters.out_played_frames.load(Ordering::Relaxed);
        let captured = self.p.counters.in_captured_frames.load(Ordering::Relaxed);
        if played != self.last_played {
            self.last_played = played;
            self.last_played_at = now;
        }
        if captured != self.last_captured {
            self.last_captured = captured;
            self.last_captured_at = now;
        }
        let stalled = (played > 0 && now.saturating_sub(self.last_played_at) > DEVICE_STALL)
            || (captured > 0 && now.saturating_sub(self.last_captured_at) > DEVICE_STALL);
        if stalled != self.p.stats.device_stalled.load(Ordering::Relaxed) {
            if stalled {
                log::error!("오디오 장치 콜백이 멈췄습니다(장치 분리 가능성)");
            } else {
                log::info!("오디오 장치 콜백 재개");
            }
            self.p.stats.device_stalled.store(stalled, Ordering::Relaxed);
        }
    }

    fn pump_input(&mut self) {
        // 송신 큐 지연의 시작점(P1-3, 리뷰 M1): 이 패스에서 링을 비우기 직전 시각. 이번 패스에서 완성되는 프레임은
        // 모두 이 시각을 쓴다 — 같은 패스의 뒤 프레임은 앞 프레임의 인코딩·송신을 기다린 시간까지 포함된다.
        // (cap_acc에 남는 것은 프레임 미만이므로, 여기서 나가는 프레임의 마지막 샘플은 모두 이번 패스에 들어왔다.)
        let pass_at = Instant::now();
        loop {
            let n = self.p.in_cons.pop_slice(&mut self.in_tmp);
            if n == 0 {
                break;
            }
            let n = n - n % self.ich;
            self.in_rs.push(&self.in_tmp[..n]);
        }
        let muted = self.p.controls.input_muted.load(Ordering::Relaxed);
        // 리샘플 직후 모니터/탭으로 바로 보낸다(프레임 누적 대기 없음, PR #5 M-5).
        loop {
            let mut buf = std::mem::take(&mut self.in48_buf);
            let frames = self.in_rs.pop(&mut buf);
            if frames == 0 {
                self.in48_buf = buf;
                break;
            }
            let block = &mut buf[..frames * self.ich];
            // 입력 뮤트: 송신/모니터/탭 모두 무음. 패킷은 계속 보내 수신측 타이밍을 유지한다.
            if muted {
                block.fill(0.0);
            }
            self.push_local(block);
            self.cap_acc.extend(block.iter().copied());
            self.in48_buf = buf;
        }
        let mut fb = std::mem::take(&mut self.frame_buf);
        let mut acc = std::mem::take(&mut self.cap_acc);
        drain_frames(&mut acc, &mut fb, pass_at, |f, at| self.on_capture_frame(f, at));
        self.cap_acc = acc;
        self.frame_buf = fb;
    }

    fn push_local(&mut self, block: &[f32]) {
        if self.ich == 2 {
            self.local.push(block);
        } else {
            let sb = &mut self.stereo_buf;
            for (d, &s) in sb.as_chunks_mut::<2>().0.iter_mut().zip(block.iter()) {
                d[0] = s;
                d[1] = s;
            }
            let n = block.len() * 2;
            let sb = std::mem::take(&mut self.stereo_buf);
            self.local.push(&sb[..n]);
            self.stereo_buf = sb;
        }
    }

    /// 에코 측정 대상 피어용 프레임(무음 + 클릭)을 만들어 인코드한다. 반환: (피어 id, 페이로드 길이).
    fn probe_frame(&mut self) -> Option<(String, usize)> {
        let now_us = self.p.epoch.elapsed().as_secs_f64() * 1e6;
        let mut g = self.p.probe.lock();
        let Some(pr) = g.as_mut() else {
            self.alt_encoder = None;
            return None;
        };
        if self.alt_encoder.is_none() {
            match Encoder::new(self.p.cfg.codec, self.ich, self.p.cfg.opus_bitrate()) {
                Ok(e) => self.alt_encoder = Some(e),
                Err(e) => {
                    log::warn!("에코 측정 인코더 생성 실패: {e}");
                    return None;
                }
            }
        }
        pr.next_frame(&mut self.alt_frame, self.ich, now_us);
        let enc = self.alt_encoder.as_mut()?;
        let n = enc.encode(&self.alt_frame, &mut self.alt_payload).ok()?;
        Some((pr.peer_id.clone(), n))
    }

    /// 피어별 Opus 자동 전환 정책 점검(200ms마다, PCM16 설정). 릴레이 부하 정책(전체)과 피어마다의 손실 피드백
    /// 정책을 갱신하고, 인코더 예상 손실을 맞춘다(relay_codec.rs). 인코더 생성(풀이 비었을 때)도 여기, 워커에서 한다.
    fn update_auto_codec(&mut self) {
        if !self.auto_enabled {
            return;
        }
        let Some(net) = self.p.net.clone() else {
            return;
        };
        let now_d = self.p.epoch.elapsed();
        // 직전 프레임에 릴레이 부하 전환 대상인데 PCM으로 나간 피어가 있으면(새 피어, 직접→릴레이) 바로 다시 본다(L1)
        let refresh = net.take_auto_refresh();
        if now_d < self.auto_check_at && !refresh {
            return;
        }
        self.auto_check_at = now_d + AUTO_CODEC_CHECK;
        let now = Instant::now();
        let force = net.force_auto_opus();
        let (relay_peers, measured_over) = net.relay_audio_load();
        let inputs = PolicyInputs {
            pcm_configured: true,
            relay_peers,
            measured_over,
            predicted_pcm_load: pcm_relay_load(self.frame, self.ich, relay_peers),
        };
        match self.relay_policy.update(now, inputs) {
            Some(true) => {
                net.set_relay_auto_opus(true);
                log::info!(
                    "릴레이 대역: 릴레이 피어 {relay_peers}명에게 Opus로 전환(예상 PCM 부하 {:.0}%, 측정 초과 {measured_over})",
                    inputs.predicted_pcm_load * 100.0
                );
            }
            Some(false) => {
                net.set_relay_auto_opus(false);
                log::info!("릴레이 대역: PCM16으로 복귀(릴레이 피어 {relay_peers}명, 예상 PCM 부하 {:.0}%)", inputs.predicted_pcm_load * 100.0);
            }
            None => {}
        }
        for a in &mut self.auto {
            a.seen = false;
        }
        let (ich, bitrate) = (self.ich, self.p.cfg.opus_bitrate());
        let (auto, pool) = (&mut self.auto, &mut self.auto_pool);
        net.for_each_peer(|peer| {
            let key = (peer as *const Peer as usize, peer.ssrc);
            let idx = match auto.iter().position(|a| a.key == key) {
                Some(i) => i,
                None => {
                    let enc = match pool.pop() {
                        Some(e) => e,
                        None => match Encoder::new(CodecKind::Opus, ich, bitrate) {
                            Ok(e) => e,
                            Err(e) => {
                                log::warn!("{}: 자동 전환용 Opus 인코더 생성 실패: {e}", peer.id);
                                return;
                            }
                        },
                    };
                    auto.push(AutoTx::new(key, peer.id.clone(), enc));
                    auto.len() - 1
                }
            };
            let a = &mut auto[idx];
            a.seen = true;
            a.on_relay = {
                let c = peer.conn.lock();
                c.can_send_audio() && c.path.is_some_and(|(_, k)| k == PathKind::Relay)
            };
            let report = *peer.remote_loss.lock();
            let loss = report.map(|(r, at)| (r.loss_pct, at));
            match a.loss.update(now, true, loss) {
                Some(true) => log::info!(
                    "{}: 수신 손실 {:.1}%(상대 보고) — 이 피어에게 Opus로 전환(손실 피드백)",
                    peer.id,
                    loss.map_or(0.0, |l| l.0)
                ),
                Some(false) => log::info!(
                    "{}: 수신 손실 회복(상대 보고 {}) — PCM16으로 복귀",
                    peer.id,
                    loss.map_or("없음".to_string(), |l| format!("{:.1}%", l.0))
                ),
                None => {}
            }
            a.loss_on = a.loss.active() || force;
            // 인코더 예상 손실: 손실 피드백이면 보고 손실에 맞추고, 릴레이 부하로만 켜졌으면 기본값(설정 Opus와 같음)
            let perc = auto_opus_perc(a.loss_on, a.on_relay, loss.map(|l| l.0));
            if perc != a.perc && a.enc.set_loss_perc(perc).is_ok() {
                a.perc = perc;
            }
        });
        // 사라진 피어의 인코더는 풀로 돌려준다(다음 피어가 새 스트림으로 쓴다 — 켤 때 초기화한다)
        let mut i = 0;
        while i < self.auto.len() {
            if self.auto[i].seen {
                i += 1;
            } else {
                let a = self.auto.swap_remove(i);
                self.auto_pool.push(a.enc);
            }
        }
    }

    /// 자동 전환 대상 피어마다 이 프레임을 그 피어 전용 Opus 인코더로 인코드한다(대상이 아니면 건너뜀).
    /// `alt_peer` = 에코 측정 클릭 스트림을 받는 피어: 그동안은 설정 코덱(클릭)이 가므로 인코드하지 않고,
    /// 측정이 끝나 다시 대상이 되면 새 스트림으로 초기화·예열한다(#66 리뷰 L2).
    fn auto_frames(&mut self, frame: &[f32], alt_peer: Option<&str>) {
        let relay_on = self.relay_policy.active();
        for a in &mut self.auto {
            a.len = None;
            let want = auto_reason(relay_on, a.on_relay, a.loss_on).is_some() && alt_peer != Some(a.id.as_str());
            if want && !a.encoding {
                // 새 스트림: 상태 초기화 + 직전 프레임으로 예열(출력 버림). 첫 패킷이 Opus 알고리즘 지연(2.5ms)만큼
                // 0으로 시작하지 않고 직전 소리에서 이어진다(전환 구간 불연속 3 → 1, 은닉 0).
                a.enc.reset();
                let _ = a.enc.encode(&self.relay_prev, &mut a.payload);
            }
            a.encoding = want;
            if !want {
                continue;
            }
            match a.enc.encode(frame, &mut a.payload) {
                Ok(n) => a.len = Some(n),
                Err(e) => {
                    if self.encode_errors.is_multiple_of(1000) {
                        log::warn!("자동 전환 Opus 인코드 실패({}회째): {e}", self.encode_errors + 1);
                    }
                    self.encode_errors += 1;
                }
            }
        }
        if self.auto_enabled {
            self.relay_prev.copy_from_slice(frame);
        }
    }

    /// `frame_at`: 송신 큐 지연의 시작점(이 프레임을 꺼낸 워커 패스의 시작 시각, `pump_input`).
    fn on_capture_frame(&mut self, frame: &[f32], frame_at: Instant) {
        let alt = if self.encoder.is_some() { self.probe_frame() } else { None };
        self.update_auto_codec();
        self.auto_frames(frame, alt.as_ref().map(|(id, _)| id.as_str()));
        if let (Some(enc), Some(net)) = (self.encoder.as_mut(), self.p.net.as_ref()) {
            let alt = alt.as_ref().map(|(id, n)| (id.as_str(), &self.alt_payload[..*n]));
            let (auto, auto_flags, relay_on) = (&self.auto, self.auto_flags, self.relay_policy.active());
            // 이유는 송신 순간의 경로로 정한다. 릴레이 부하 대상인데 아직 인코드하지 않은 피어(None)는 net이
            // 표시해 두고 워커가 다음 프레임에서 바로 다시 본다(L1).
            let lookup = |p: &Peer, kind: PathKind| -> Option<AutoPayload<'_>> {
                let key = (p as *const Peer as usize, p.ssrc);
                let a = auto.iter().find(|a| a.key == key)?;
                let reason = auto_reason(relay_on, kind == PathKind::Relay, a.loss_on)?;
                Some(AutoPayload { payload: &a.payload[..a.len?], flags: auto_flags, reason })
            };
            // 자동 전환이 가능하면 대상 목록이 비어 있어도 넘긴다(첫 릴레이 피어도 L1 표시를 받게)
            let auto_fn = self.auto_enabled.then_some(&lookup as _);
            match enc.encode(frame, &mut self.payload) {
                Ok(n) => net.send_audio_timed(frame_at, &self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt, alt, auto_fn),
                Err(e) => {
                    // 레이트 제한 로그(리뷰 L10)
                    if self.encode_errors.is_multiple_of(1000) {
                        log::warn!("인코드 실패({}회째): {e}", self.encode_errors + 1);
                    }
                    self.encode_errors += 1;
                }
            }
        }
        self.ts = self.ts.wrapping_add(self.frame as u32);
    }

    fn timing(&mut self) -> FeedTiming {
        let wall_us = self.p.epoch.elapsed().as_micros() as u64;
        let out_burst = self.out_burst_dev() as usize;
        self.tracker.observe(&self.p.counters, wall_us, out_burst, self.p.info.out_rate)
    }

    /// 공유 메트로놈(P2-3): 다음 클릭 프레임을 올리고(시계 추정이 바뀌면 다시 맞춤), 상태를 0.1초마다 알린다.
    fn service_metronome(&mut self, t: &FeedTiming) {
        if t.cb_seq.is_none() {
            return; // 콜백 도중: 재생 카운터와 시각이 어긋날 수 있다
        }
        {
            let p = self.p.metronome.plan.lock();
            if p.0 != self.metro_plan.0 {
                self.metro_plan = p.clone();
            }
        }
        let (gen, plan) = (self.metro_plan.0, &self.metro_plan.1);
        let est = match (&plan.ref_peer_id, self.p.net.as_ref()) {
            (Some(id), Some(net)) if plan.running => net.peer_clock(id, t.wall_us),
            _ => None,
        };
        let offset = match &plan.ref_peer_id {
            None => Some(0.0),
            Some(_) => est.map(|e| e.offset_us),
        };
        let rate = t.out_rate as f64;
        let info = &self.p.info;
        let driver = hidden_out_latency_us(info.out_reported_latency_frames, t.out_burst as u64, rate);
        // 드라이버가 출력 지연을 알려 주지 않으면 루프백 보정값으로(PR #34 M2(a)). 입력 쪽 보고가 있으면 그만큼 뺀다.
        let loopback = (*self.p.metronome.loopback_offset_ms.lock()).map(|o| {
            let in_burst = match self.p.counters.in_cb_frames.load(Ordering::Relaxed) {
                0 => info.buffer_frames,
                n => n,
            };
            measured_hidden_out_us(o, hidden_out_latency_us(info.in_reported_latency_frames, in_burst as u64, info.in_rate as f64))
        });
        let hidden = choose_hidden_out(driver, loopback);
        let extra_out_us = hidden.map_or(0.0, |h| h.0);
        let clock = OutClock { dev_now_us: t.dev_now_us as f64, played: t.played, rate, burst: t.out_burst as u64, extra_out_us };
        let beat = self.beats.tick(gen, plan, offset, &clock, &self.p.metronome.click);
        if t.wall_us.saturating_sub(self.metro_stats_at) >= 100_000 || gen != self.metro_stats_gen {
            self.metro_stats_at = t.wall_us;
            self.metro_stats_gen = gen;
            let mut s = self.p.metronome.stats.lock();
            s.running = plan.running;
            s.synced = offset.is_some();
            s.bpm = plan.bpm;
            s.beats_per_bar = plan.beats_per_bar;
            s.start_us = plan.start_us;
            s.ref_peer_id.clone_from(&plan.ref_peer_id);
            s.clock_offset_ms = offset.map(|o| o / 1000.0);
            s.clock_rtt_ms = est.map(|e| e.rtt_us / 1000.0);
            s.drift_ppm = est.map(|e| e.drift_ppm);
            s.beat = beat;
            s.output_latency_ms = hidden.map(|h| h.0 / 1000.0);
            s.output_latency_source = hidden.map(|h| h.1);
            s.ref_clock_unsupported = match (&plan.ref_peer_id, self.p.net.as_ref()) {
                (Some(id), Some(net)) if plan.running && est.is_none() => net.peer_clock_unsupported(id),
                _ => false,
            };
            s.clock_extrapolation_ms = est.map(|e| e.extrapolation_us / 1000.0);
        }
    }

    /// 피어 목록과 소스 슬롯을 맞춘다(새 피어에 빈 슬롯 배정, 사라진 피어 슬롯 해제).
    fn sync_peer_slots(&mut self, net: &NetShared) {
        for f in &mut self.feeds {
            f.seen = false;
        }
        let out_rate = self.p.info.out_rate;
        net.for_each_peer(|peer| {
            let key = (peer as *const Peer as usize, peer.ssrc);
            if let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) {
                f.seen = true;
                return;
            }
            let used = |s: usize| self.feeds.iter().any(|f| f.slot == s);
            let free = (1..MAX_SOURCES)
                .find(|&s| !used(s) && self.p.bank.slots[s].state.load(Ordering::Acquire) == SLOT_FREE);
            let Some(slot) = free else {
                if !self.slot_full_logged {
                    log::warn!("출력 소스 슬롯이 부족합니다(피어 {})", peer.id);
                    self.slot_full_logged = true;
                }
                return;
            };
            let sh = &self.p.bank.slots[slot];
            match PeerFeed::new(slot, key, out_rate, sh) {
                Ok(f) => {
                    sh.playing.store(false, Ordering::Relaxed);
                    sh.occupant.store(peer.ssrc, Ordering::Relaxed); // 녹음 트랙 구분(P2-4), ACTIVE(Release) 전에
                    sh.state.store(SLOT_ACTIVE, Ordering::Release);
                    self.feeds.push(f);
                }
                Err(e) => log::error!("피어 출력 소스 생성 실패: {e}"),
            }
        });
        let bank = &self.p.bank;
        self.feeds.retain(|f| {
            if !f.seen {
                let sh = &bank.slots[f.slot];
                sh.playing.store(false, Ordering::Relaxed);
                sh.state.store(SLOT_DRAIN, Ordering::Release);
            }
            f.seen
        });
    }

    /// 반환: 은닉 마감이 가까운 피어가 있다(짧게 자야 함).
    fn service_peers(&mut self, t: &FeedTiming) -> bool {
        let Some(net) = self.p.net.clone() else {
            return false;
        };
        self.sync_peer_slots(&net);
        let counters = self.p.counters.clone();
        let cb_seq = t.cb_seq;
        let consistent = move || cb_seq.is_some_and(|s| counters.out_cb_begin.load(Ordering::SeqCst) == s);
        let (mut fills, mut skipped, mut urgent) = (0u64, 0u64, false);
        let probe = self.p.probe.clone();
        let mut guard = probe.lock();
        let mut tap = guard.as_mut().filter(|p| p.tap);
        let marks = self.p.rec_marks.clone();
        let marking = marks.active();
        let played = &self.p.counters.out_played_frames;
        net.for_each_peer(|peer| {
            let key = (peer as *const Peer as usize, peer.ssrc);
            let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) else {
                return;
            };
            let sh = &self.p.bank.slots[f.slot];
            let g = peer.mix.lr_gains();
            sh.set_gains(g, g);
            peer.mix.level.store(sh.level.load());
            let Some(prod) = self.p.prods.get_mut(f.slot) else {
                return;
            };
            let mut jb = peer.jitter.lock();
            let before = marking.then(|| jb.mark_counters());
            let ev = f.service(&mut jb, prod, sh, t, &consistent, &mut self.chunk_buf, &mut self.out_tmp, tap.as_deref_mut().filter(|p| p.peer_id == peer.id && p.peer_ssrc == peer.ssrc));
            if let Some((c0, d0)) = before {
                // 녹음 마커(P2-4a): 이번 반복에 은닉·버림이 늘었으면, 방금 링에 넣은 꼬리(= 다음 콜백 시작 + 선행량)의
                // 출력 타임라인 위치로 기록한다. 링 선행량 추정이라 대략 ±1 콜백으로 추정한다(위치 대조 측정은 아직 없음).
                let (c1, d1) = jb.mark_counters();
                let to_dev = |n48: u64| n48 * t.out_rate as u64 / INTERNAL_RATE as u64;
                let fs = jb.frame_samples() as u64;
                let tail = played.load(Ordering::Relaxed) + f.dev_ahead(prod) as u64;
                let mark = |kind, frames: u64, span: bool| {
                    let start = if span { tail.saturating_sub(frames) } else { tail };
                    marks.push(MarkEvent { ssrc: peer.ssrc, kind, start, frames });
                };
                if c1 > c0 {
                    mark(MarkKind::Conceal, to_dev((c1 - c0) * fs), true);
                }
                // `skipped_frames`는 이미 장치 프레임이다
                let dropped = d1.saturating_sub(d0) * fs;
                if dropped > 0 || ev.skipped_frames > 0 {
                    mark(MarkKind::Drop, to_dev(dropped) + ev.skipped_frames, false);
                }
            }
            drop(jb);
            fills += u64::from(ev.deadline_fill);
            skipped += ev.skipped_frames;
            urgent |= ev.urgent;
        });
        if fills > 0 {
            self.p.stats.deadline_fills.fetch_add(fills, Ordering::Relaxed);
        }
        if skipped > 0 {
            self.p.stats.skipped_frames.fetch_add(skipped, Ordering::Relaxed);
        }
        urgent
    }

    /// xrun이 나면 로컬 소스 여유를 청크 하나씩 늘리고(최대 장치 버퍼 1개), 10초 동안 없으면 줄인다.
    /// 콜백 간격이 들쭉날쭉하거나 워커 스케줄링이 늦는 환경(느린 CI 러너 포함)에 적응한다.
    fn adapt_safety(&mut self, chunk_dev: u64) {
        let xruns = self.p.counters.xruns.load(Ordering::Relaxed);
        let now = self.p.epoch.elapsed();
        if xruns > self.seen_xruns {
            self.seen_xruns = xruns;
            self.safety = (self.safety + chunk_dev).min(self.out_burst_dev());
            self.last_xrun = now;
        } else if self.safety > 0 && now.saturating_sub(self.last_xrun) > Duration::from_secs(10) {
            self.safety = self.safety.saturating_sub(chunk_dev);
            self.last_xrun = now;
        }
    }

    /// 로컬(모니터 + 탭) 소스. 로컬 FIFO가 입·출력 클럭 차를 흡수하고 자기 수위를 제어하므로, 소스 링은
    /// 콜백이 소비한 만큼 "장치 버퍼 1개 + 청크 + 적응 여유"로 다시 채운다(모니터 경로는 GOAL 지표 밖).
    fn service_local(&mut self) {
        let ctl = &self.p.controls;
        let mon = if ctl.monitor_enabled.load(Ordering::Relaxed) { ctl.monitor_gain.load() } else { 0.0 };
        let sh = &self.p.bank.slots[LOCAL_SOURCE];
        sh.set_gains((mon, mon), (1.0, 1.0));
        let chunk_dev = (OUT_CHUNK as u64 * self.p.info.out_rate as u64).div_ceil(INTERNAL_RATE as u64);
        self.adapt_safety(chunk_dev);
        let target = self.out_burst_dev() + chunk_dev + self.safety;
        let fill = |w: &Self| (w.p.prods[LOCAL_SOURCE].occupied_len() / 2 + w.local_rs.available()) as u64;
        if fill(self) >= target {
            return;
        }
        // 한 번에 너무 많이 만들지 않도록 상한(장치 버퍼 4개분)
        let max_chunks = ((self.out_burst_dev() * 4) / chunk_dev.max(1) + 2) as usize;
        let mut produced = 0;
        while fill(self) < target && produced < max_chunks {
            self.local.pull(&mut self.local_buf);
            self.local_rs.push(&self.local_buf);
            let prod = &mut self.p.prods[LOCAL_SOURCE];
            loop {
                let room = (prod.vacant_len() / 2).min(self.out_tmp.len() / 2);
                let n = if room == 0 { 0 } else { self.local_rs.pop(&mut self.out_tmp[..room * 2]) };
                if n == 0 {
                    break;
                }
                prod.push_slice(&self.out_tmp[..n * 2]);
            }
            produced += 1;
        }
        if produced > 0 {
            self.local.end_burst();
            self.p.bank.slots[LOCAL_SOURCE].playing.store(true, Ordering::Relaxed);
            self.p.stats.local_corrections.store(self.local.corrections, Ordering::Relaxed);
            self.p.stats.local_underruns.store(self.local.underruns, Ordering::Relaxed);
        }
    }

    /// 모니터 경로 점유량(시간) 이동 평균(리뷰 M12).
    fn track_occupancy(&mut self) {
        let in48 = (self.in_rs.available() + self.local.frames()) as f64;
        let local_dev = (self.p.prods[LOCAL_SOURCE].occupied_len() / 2 + self.local_rs.available()) as f64;
        let out_us = local_dev * 1e6 / self.p.info.out_rate.max(1) as f64;
        let mon_us = in48 * 1e6 / INTERNAL_RATE as f64 + out_us;
        self.monitor_ewma_us += (mon_us - self.monitor_ewma_us) * 0.01;
        self.p.stats.monitor_occupancy_us.store(self.monitor_ewma_us as u64, Ordering::Relaxed);
    }
}
