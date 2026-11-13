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
//! ## 구조(엔진 ②b, `docs/overhaul/engine-stage2b-design.md`)
//! `Worker`는 **조정자**다: 단계 구조체를 정해진 순서로 부르고 단계 사이 값(48k 블록, `FeedTiming`, `urgent`,
//! 입력 쪽 대기량)을 인자·반환값으로 넘긴다. 단계는 서로의 필드를 읽지 않고, 읽기 전용 문맥(`WorkerCtx`) 전체가 아니라
//! 쓰는 값만 인자로 받는다.
//!
//! ## 파일
//! - `mod.rs`: `Worker`(조정자) 조립과 루프, 정지. 송신 쪽(입력 펌프·인코드·송신·자동 코덱)은 아직 여기 있다(②b-2에서 단계로)
//! - `clock.rs`: 출력 콜백 시각 추정(`DeviceClock`, `CallbackTracker`, `FeedTiming`) — 시각 단계
//! - `peer_sources.rs`: 피어 소스 단계(`PeerSources`: 미디어 표·슬롯 배정·피어별 공급·녹음 마커)
//! - `peer_feed.rs`: 피어 하나의 소스 공급(`PeerFeed`, 콜백 구동 믹스 규칙)
//! - `local_path.rs`: 로컬 경로 단계(`LocalPath`: 모니터·탭 공급, 여유 적응, 점유 통계)
//! - `local.rs`: 로컬 입력 FIFO(`LocalFifo`)
//! - `metronome_stage.rs`: 메트로놈 단계(`MetronomeStage`: 박 예약 입력·상태 보고)
//! - `device_watch.rs`: 장치 감시 단계(`DeviceWatch`)
//! - `tests.rs`: 결정적 단위 시뮬레이션

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::traits::Consumer;
use ringbuf::{HeapCons, HeapProd};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, CodecKind, EngineConfig, INTERNAL_RATE, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use super::device::StreamInfo;

use super::measure::EchoProbe;
use super::metronome::MetronomeCtl;
use super::mixer::MixControls;
use super::record::MarkSink;
use super::relay_codec::{auto_opus_perc, auto_reason, pcm_relay_load, LossCodecPolicy, PolicyInputs, RelayCodecPolicy, DEFAULT_OPUS_LOSS_PERC};
use super::resample::{StreamResampler, CHUNK as RESAMPLE_CHUNK};
use super::rt::{SourceBank, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE};
use super::stats::AudioCounters;
use crate::net::types::PathKind;
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use super::peer_media::{MediaPortRt, MediaSettings};
use crate::net::media_link::PeerKey;
use crate::net::peer::Peer;
use crate::net::{AutoPayload, NetShared, MAX_PEERS};

mod clock;
mod device_watch;
mod local;
mod local_path;
mod metronome_stage;
mod peer_feed;
mod peer_sources;
#[cfg(test)]
mod tests;

pub use clock::{CallbackTracker, DeviceClock, FeedTiming};
pub use local::LocalFifo;
pub use peer_feed::{FeedEvents, PeerFeed, DEADLINE_GUARD_US};

use device_watch::DeviceWatch;
use local_path::LocalPath;
use metronome_stage::MetronomeStage;
pub(crate) use peer_sources::{PeerSources, PeerSourcesParts};

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
    pub thread_prio: crate::sys::prio::PrioSlot,
    /// 워커의 피어 미디어 표 길이(명령을 비운 직후, 엔진 ③). 진단·테스트: 표에 죽은 항목이 남는지 직접 본다.
    pub media_peers: AtomicU64,
    /// 워커 반복 본문 시간(수면 제외)과 반복 시작 간격 분포(엔진 ③c 측정, 진단 전용 — 측정 시험이 비운다).
    pub iter_body: crate::net::txlat::SendLatency,
    pub iter_period: crate::net::txlat::SendLatency,
    /// 반복마다 남은 락(에코 프로브·메트로놈)에서 기다린 합과, 본문에서 그것을 뺀 값(락 밖 비용, #122 리뷰 M3).
    pub iter_lock_wait: crate::net::txlat::SendLatency,
    pub iter_unlocked: crate::net::txlat::SendLatency,
    /// 남은 락을 자리별로(엔진 ②b 측정, 설계 4.3절): 기다린 횟수·합·최대. ②b-5·②b-6b가 없앨 락들이다.
    pub lock_sites: WorkerLockSites,
}

/// 워커가 기본 우선순위 스레드와 나누는 남은 락의 자리별 대기 기록(진단 전용, `Engine::take_worker_timing`).
#[derive(Debug, Default)]
pub struct WorkerLockSites {
    /// 에코 프로브 락 — 캡처 프레임마다(클릭 프레임 생성).
    pub probe_send: crate::net::txlat::LockSite,
    /// 에코 프로브 락 — 반복마다, 전 피어 공급 동안 쥔다(재생 시각 탭).
    pub probe_feed: crate::net::txlat::LockSite,
    /// 메트로놈 계획 락 — 반복마다.
    pub metro_plan: crate::net::txlat::LockSite,
    /// 메트로놈 루프백 보정 락 — 반복마다.
    pub metro_offset: crate::net::txlat::LockSite,
    /// 메트로놈 상태 보고 락 — 100ms마다.
    pub metro_stats: crate::net::txlat::LockSite,
    /// 기준 피어 시계 조회의 피어 표 읽기 락 — 기준 피어를 따르는 동안 반복마다.
    pub peer_clock: crate::net::txlat::LockSite,
}

/// 워커 반복 시간 분포 기록(엔진 ③c 측정, #122 리뷰 Low): 측정 빌드(`test-support`)에서만. 출시 빌드는 시계를 더 읽지 않는다.
const DIAG_TIMING: bool = cfg!(feature = "test-support");


// ============================================================================
// 워커
// ============================================================================

/// `Worker::new`의 입력. 소유권이 넘어가는 끝(`in_cons`·`prods`·`media_port`)과 공유 읽기 전용 값이 섞여 있으므로 단계에
/// 그대로 주지 않는다 — `Worker::new`가 `WorkerCtx`(조정자 묶음)와 단계들에 나눠 준다.
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
    /// 피어 미디어 명령 큐의 워커 쪽 끝(추가·제거, 엔진 ③-a2).
    pub media_port: MediaPortRt,
    /// 지터 프로필(원자값, 워커가 반복마다 비교해 적용).
    pub media_settings: Arc<MediaSettings>,
}

/// 조정자(`Worker::run`)의 읽기 전용 묶음: 설정·장치 정보·카운터·컨트롤·소스 뱅크·통계·epoch·stop(모두 `Arc`·`Copy`·불변).
/// **단계 메서드는 이것을 받지 않는다** — 조정자가 필드를 꺼내 쓰는 값만 인자로 넘긴다(설계 2.2절). `net`은 여기에 없다.
struct WorkerCtx {
    cfg: EngineConfig,
    info: StreamInfo,
    counters: Arc<AudioCounters>,
    controls: Arc<MixControls>,
    bank: Arc<SourceBank>,
    stats: Arc<WorkerStats>,
    /// 지터 버퍼 수신 시각과 같은 기준(µs)의 시계.
    epoch: Instant,
    stop: Arc<AtomicBool>,
}

impl WorkerCtx {
    fn wall_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }
}

// 버스트 계산: 단계가 문맥 없이 부를 수 있게 자유 함수다. **부를 때마다 원자값을 읽는다** — 반복당 한 번 읽어 넘기는
// 것으로 바꾸지 않는다(콜백 크기가 바뀌는 순간의 동작이 달라진다, 설계 2.2절).

fn to48(frames: u32, rate: u32) -> usize {
    (frames as u64 * INTERNAL_RATE as u64).div_ceil(rate.max(1) as u64) as usize
}

/// 입력 콜백 크기(48k 환산). 아직 콜백이 안 돌았으면 장치 버퍼 크기.
fn in_burst48(info: &StreamInfo, counters: &AudioCounters) -> usize {
    let n = match counters.in_cb_frames.load(Ordering::Relaxed) {
        0 => info.buffer_frames,
        n => n,
    };
    to48(n, info.in_rate)
}

/// 출력 콜백 크기(장치 프레임). 아직 콜백이 안 돌았으면 장치 주기, 그것도 모르면 장치 버퍼 크기.
fn out_burst_dev(info: &StreamInfo, counters: &AudioCounters) -> u64 {
    match counters.out_cb_frames.load(Ordering::Relaxed) {
        0 => match info.out_period_frames {
            0 => info.buffer_frames as u64,
            n => n as u64,
        },
        n => n as u64,
    }
}

/// 출력 생산 단위 `OUT_CHUNK`(48k)의 장치 프레임 수.
fn chunk_dev(info: &StreamInfo) -> u64 {
    (OUT_CHUNK as u64 * info.out_rate as u64).div_ceil(INTERNAL_RATE as u64)
}

/// 조정자. 필드 = 문맥 + 단계들 + 조정자가 갖고 단계에 빌려 주는 것(`peer_copy`·`lock_wait`) + 아직 단계로 옮기지 않은
/// 송신 쪽(②b-2에서 `Capture`·`SendPath`·`AutoCodec`).
pub struct Worker {
    ctx: WorkerCtx,
    /// 시각 단계(출력 콜백 시각 추정).
    clock: CallbackTracker,
    metro: MetronomeStage,
    peers: PeerSources,
    local: LocalPath,
    watch: DeviceWatch,
    /// 피어 표 복사본(엔진 ③c): 송신·자동 코덱이 락 없이 본다. 세대가 바뀐 반복에서만 다시 복사된다.
    peer_copy: crate::net::PeerTableCopy,
    /// 이번 반복에 남은 락(에코 프로브·메트로놈)에서 기다린 시간(엔진 ③c 측정 — 본문에서 빼서 락 밖 비용을 본다).
    /// 락을 잡는 단계 메서드에 빌려 준다.
    lock_wait: crate::net::txlat::LockWait,

    // ---- 송신 쪽(②b-2에서 단계로 옮긴다) ----
    net: Option<Arc<NetShared>>,
    in_cons: HeapCons<f32>,
    /// 에코 측정 상태(클릭 프레임 생성). 재생 시각 탭 쪽 복제는 `PeerSources`가 갖는다.
    probe: Arc<parking_lot::Mutex<Option<EchoProbe>>>,
    frame: usize,
    ich: usize,
    in_rs: StreamResampler,
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
    in_tmp: Vec<f32>,
    /// 48k 입력 블록(리샘플 직후, 내부 채널).
    in48_buf: Vec<f32>,
    /// 인코더용 프레임 누적.
    cap_acc: VecDeque<f32>,
    frame_buf: Vec<f32>,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    encode_errors: u64,
}


/// 피어 하나의 Opus 자동 전환 송신 상태(PCM16 설정, P1-4c·P1-4e). 키는 `PeerFeed`와 같은 `PeerKey`.
struct AutoTx {
    key: PeerKey,
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
    fn new(key: PeerKey, id: String, mut enc: Encoder) -> Self {
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
        let WorkerParams { cfg, info, in_cons, mut prods, bank, counters, controls, net, stop, stats, epoch, probe, metronome, rec_marks, media_port, media_settings } = p;
        let frame = cfg.frame_samples();
        let ich = cfg.channels as usize;
        for (what, r) in [("입력", info.in_rate), ("출력", info.out_rate)] {
            if !(MIN_DEVICE_RATE..=MAX_DEVICE_RATE).contains(&r) {
                return Err(format!("지원하지 않는 {what} 장치 레이트: {r}Hz"));
            }
        }
        if prods.len() != MAX_SOURCES {
            return Err("소스 링 수가 맞지 않습니다".into());
        }
        // 소스 링 생산자: 로컬 것(슬롯 0)은 `LocalPath`, 나머지(슬롯 1..)는 `PeerSources`.
        const _: () = assert!(LOCAL_SOURCE == 0);
        let peer_prods = prods.split_off(LOCAL_SOURCE + 1);
        let local_prod = prods.pop().ok_or("소스 링 수가 맞지 않습니다")?;
        let in_rs = StreamResampler::new(info.in_rate, INTERNAL_RATE, ich)?;
        let local = LocalPath::new(ich, info.out_rate, local_prod)?;
        let encoder = if net.is_some() {
            Some(Encoder::new(cfg.codec, ich, cfg.opus_bitrate())?)
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
        let auto_enabled = net.is_some() && cfg.codec == CodecKind::Pcm16;
        let mut auto_pool = Vec::with_capacity(MAX_PEERS);
        if auto_enabled {
            for _ in 0..MAX_PEERS {
                auto_pool.push(Encoder::new(CodecKind::Opus, ich, cfg.opus_bitrate())?);
            }
        }
        let auto_flags = FLAG_OPUS | if ich == 2 { FLAG_STEREO } else { 0 };
        // 로컬 소스는 항상 켜져 있다(모니터 꺼짐 = 버스 게인 0, 탭에는 계속 나감).
        let local_slot = &bank.slots[LOCAL_SOURCE];
        local_slot.set_gains((0.0, 0.0), (1.0, 1.0));
        local_slot.state.store(SLOT_ACTIVE, Ordering::Release);
        Ok(Self {
            clock: CallbackTracker::new(info.out_rate),
            metro: MetronomeStage::new(metronome, net.clone()),
            peers: PeerSources::new(PeerSourcesParts { prods: peer_prods, port: media_port, settings: media_settings, rec_marks, probe: probe.clone() }),
            local,
            watch: DeviceWatch::new(),
            peer_copy: crate::net::PeerTableCopy::for_worker(),
            lock_wait: Default::default(),
            net,
            in_cons,
            probe,
            frame,
            ich,
            in_rs,
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
            in_tmp: vec![0.0; 4096],
            in48_buf: vec![0.0; 2048 * ich],
            cap_acc: VecDeque::with_capacity((frame + 4096) * ich),
            frame_buf: vec![0.0; frame * ich],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            encode_errors: 0,
            ctx: WorkerCtx { cfg, info, counters, controls, bank, stats, epoch, stop },
        })
    }

    /// 워커 루프. **호출 순서와 원자값·시계를 읽는 시점이 계약이다**(설계 5절) — 읽기 좋게 재배열하지 말 것:
    /// - `clock.observe`(`FeedTiming`)는 캡처·인코드·송신 **뒤**, 메트로놈·피어 공급 **앞**에서 한 번(J5). 앞으로 옮기면 실시간
    ///   은닉 마감 판단이 송신 시간만큼 이르게 된다.
    /// - 로컬 FIFO는 `configure` → `push`(블록마다, `pump_input` 안) → `service` 순.
    pub fn run(mut self) {
        let _prio = crate::sys::prio::promote_engine_thread(&self.ctx.stats.thread_prio);
        log::info!("워커 시작: frame {} ch {}", self.frame, self.ich);
        let mut last_start = Instant::now();
        while !self.ctx.stop.load(Ordering::Relaxed) {
            let start = Instant::now();
            if DIAG_TIMING {
                self.ctx.stats.iter_period.record(start - last_start);
            }
            last_start = start;
            // 피어 표 복사본은 송신과 무관하게 반복마다 맞춘다: 캡처가 멈춰도(장치 분리) 빠진 피어가 쌓이지 않게(#126 리뷰 M-1).
            // 세대가 같으면 원자 load 하나.
            if let Some(net) = self.net.as_ref() {
                net.refresh_copy(&mut self.peer_copy);
            }
            // 모니터/탭은 프레임 누적 전에 분기하므로 입력 방출 단위는 리샘플러 청크(패스스루면 1)다.
            let granularity = if self.in_rs.is_passthrough() { 1 } else { RESAMPLE_CHUNK };
            self.local.configure(&self.ctx.info, &self.ctx.counters, granularity);
            self.pump_input();
            let c = &self.ctx;
            // 인자 평가 순서 = 읽는 순서: 벽시계 → 출력 버스트 → (observe 안) 콜백 카운터.
            let timing = self.clock.observe(&c.counters, c.wall_us(), out_burst_dev(&c.info, &c.counters) as usize, c.info.out_rate);
            self.metro.service(&c.info, &c.counters, &c.stats, &timing, &mut self.lock_wait);
            let round = self.peers.service(&c.bank, &c.counters, &c.stats, c.info.out_rate, &timing, &mut self.lock_wait);
            self.local.service(&c.info, &c.counters, &c.controls, &c.bank.slots[LOCAL_SOURCE], &c.stats, c.epoch);
            self.local.track_occupancy(c.info.out_rate, &c.stats, self.in_rs.available());
            self.watch.check(&c.counters, &c.stats, c.epoch);
            let waited = std::time::Duration::from_nanos(std::mem::take(&mut self.lock_wait).ns);
            if DIAG_TIMING {
                let body = start.elapsed();
                let s = &c.stats;
                s.iter_body.record(body);
                s.iter_lock_wait.record(waited);
                s.iter_unlocked.record(body.saturating_sub(waited));
            }
            // 은닉 마감이 가까우면 짧게 잔다(Windows 고해상도 타이머라도 0.5ms 폴링은 마감을 넘길 수 있다).
            std::thread::sleep(if round.urgent { FINE_POLL } else { POLL });
        }
        self.peers.close(&self.ctx.bank);
        log::info!("워커 종료");
    }

    fn pump_input(&mut self) {
        // 송신 큐 지연의 시작점(P1-3, 리뷰 M1): 이 패스에서 링을 비우기 직전 시각. 이번 패스에서 완성되는 프레임은
        // 모두 이 시각을 쓴다 — 같은 패스의 뒤 프레임은 앞 프레임의 인코딩·송신을 기다린 시간까지 포함된다.
        // (cap_acc에 남는 것은 프레임 미만이므로, 여기서 나가는 프레임의 마지막 샘플은 모두 이번 패스에 들어왔다.)
        let pass_at = Instant::now();
        loop {
            let n = self.in_cons.pop_slice(&mut self.in_tmp);
            if n == 0 {
                break;
            }
            let n = n - n % self.ich;
            self.in_rs.push(&self.in_tmp[..n]);
        }
        let muted = self.ctx.controls.input_muted.load(Ordering::Relaxed);
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
            self.local.push(block);
            self.cap_acc.extend(block.iter().copied());
            self.in48_buf = buf;
        }
        let mut fb = std::mem::take(&mut self.frame_buf);
        let mut acc = std::mem::take(&mut self.cap_acc);
        drain_frames(&mut acc, &mut fb, pass_at, |f, at| self.on_capture_frame(f, at));
        self.cap_acc = acc;
        self.frame_buf = fb;
    }

    /// 에코 측정 대상 피어용 프레임(무음 + 클릭)을 만들어 인코드한다. 반환: (피어 id, 페이로드 길이).
    fn probe_frame(&mut self) -> Option<(String, usize)> {
        let now_us = self.ctx.epoch.elapsed().as_secs_f64() * 1e6;
        let mut g = self.lock_wait.lock(&self.probe, &self.ctx.stats.lock_sites.probe_send);
        let Some(pr) = g.as_mut() else {
            self.alt_encoder = None;
            return None;
        };
        if self.alt_encoder.is_none() {
            match Encoder::new(self.ctx.cfg.codec, self.ich, self.ctx.cfg.opus_bitrate()) {
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
        let Some(net) = self.net.clone() else {
            return;
        };
        let now_d = self.ctx.epoch.elapsed();
        // 직전 프레임에 릴레이 부하 전환 대상인데 PCM으로 나간 피어가 있으면(새 피어, 직접→릴레이) 바로 다시 본다(L1)
        let refresh = net.take_auto_refresh();
        if now_d < self.auto_check_at && !refresh {
            return;
        }
        self.auto_check_at = now_d + AUTO_CODEC_CHECK;
        let now = Instant::now();
        let force = net.force_auto_opus();
        let (relay_peers, measured_over) = net.relay_audio_load(&self.peer_copy);
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
        let (ich, bitrate) = (self.ich, self.ctx.cfg.opus_bitrate());
        let (auto, pool) = (&mut self.auto, &mut self.auto_pool);
        for peer in self.peer_copy.peers() {
            let key = peer.key();
            let idx = match auto.iter().position(|a| a.key == key) {
                Some(i) => i,
                None => {
                    let enc = match pool.pop() {
                        Some(e) => e,
                        None => match Encoder::new(CodecKind::Opus, ich, bitrate) {
                            Ok(e) => e,
                            Err(e) => {
                                log::warn!("{}: 자동 전환용 Opus 인코더 생성 실패: {e}", peer.id());
                                continue;
                            }
                        },
                    };
                    auto.push(AutoTx::new(key, peer.id().to_string(), enc));
                    auto.len() - 1
                }
            };
            let a = &mut auto[idx];
            a.seen = true;
            a.on_relay = peer.sends_audio_via_relay();
            let report = peer.remote_loss();
            let loss = report.map(|(r, at)| (r.loss_pct, at));
            match a.loss.update(now, true, loss) {
                Some(true) => log::info!(
                    "{}: 수신 손실 {:.1}%(상대 보고) — 이 피어에게 Opus로 전환(손실 피드백)",
                    peer.id(),
                    loss.map_or(0.0, |l| l.0)
                ),
                Some(false) => log::info!(
                    "{}: 수신 손실 회복(상대 보고 {}) — PCM16으로 복귀",
                    peer.id(),
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
        }
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
        if let (Some(enc), Some(net)) = (self.encoder.as_mut(), self.net.as_ref()) {
            let alt = alt.as_ref().map(|(id, n)| (id.as_str(), &self.alt_payload[..*n]));
            let (auto, auto_flags, relay_on) = (&self.auto, self.auto_flags, self.relay_policy.active());
            // 이유는 송신 순간의 경로로 정한다. 릴레이 부하 대상인데 아직 인코드하지 않은 피어(None)는 net이
            // 표시해 두고 워커가 다음 프레임에서 바로 다시 본다(L1).
            let lookup = |p: &Peer, kind: PathKind| -> Option<AutoPayload<'_>> {
                let key = p.key();
                let a = auto.iter().find(|a| a.key == key)?;
                let reason = auto_reason(relay_on, kind == PathKind::Relay, a.loss_on)?;
                Some(AutoPayload { payload: &a.payload[..a.len?], flags: auto_flags, reason })
            };
            // 자동 전환이 가능하면 대상 목록이 비어 있어도 넘긴다(첫 릴레이 피어도 L1 표시를 받게)
            let auto_fn = self.auto_enabled.then_some(&lookup as _);
            match enc.encode(frame, &mut self.payload) {
                Ok(n) => net.send_audio_timed(&mut self.peer_copy, frame_at, &self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt, alt, auto_fn),
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
}
