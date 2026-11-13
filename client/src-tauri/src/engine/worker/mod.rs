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
//! - `mod.rs`: `Worker`(조정자) 조립과 루프, 정지
//! - `capture.rs`: 캡처 단계(`Capture`: 입력 링 → 리샘플 → 48k 블록·프레임)
//! - `send.rs`: 송신 단계(`SendPath`: 프레임 → 인코드 → 송신, 에코 클릭 스트림. 네트워크가 있을 때만 존재)
//! - `auto_codec.rs`: `SendPath`의 부속 `AutoCodec`(PCM16 설정의 피어별 Opus 자동 전환 송신 상태)
//! - `clock.rs`: 출력 콜백 시각 추정(`DeviceClock`, `CallbackTracker`, `FeedTiming`) — 시각 단계
//! - `peer_sources.rs`: 피어 소스 단계(`PeerSources`: 미디어 표·슬롯 배정·피어별 공급·녹음 마커)
//! - `peer_feed.rs`: 피어 하나의 소스 공급(`PeerFeed`, 콜백 구동 믹스 규칙)
//! - `local_path.rs`: 로컬 경로 단계(`LocalPath`: 모니터·탭 공급, 여유 적응, 점유 통계)
//! - `local.rs`: 로컬 입력 FIFO(`LocalFifo`)
//! - `metronome_stage.rs`: 메트로놈 단계(`MetronomeStage`: 박 예약 입력·상태 보고)
//! - `device_watch.rs`: 장치 감시 단계(`DeviceWatch`)
//! - `tests.rs`: 결정적 단위 시뮬레이션

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::{HeapCons, HeapProd};

use super::config::{frame_code_from_samples, EngineConfig, INTERNAL_RATE};
use super::device::StreamInfo;

use super::measure::EchoProbe;
use super::metronome::MetronomeCtl;
use super::mixer::MixControls;
use super::record::MarkSink;
use super::rt::{SourceBank, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE};
use super::stats::AudioCounters;
use super::peer_media::{MediaPortRt, MediaSettings};
use crate::net::txlat::LockWait;
use crate::net::{NetShared, PeerTableCopy};

mod auto_codec;
mod capture;
mod clock;
mod device_watch;
mod local;
mod local_path;
mod metronome_stage;
mod peer_feed;
mod peer_sources;
mod send;
#[cfg(test)]
mod tests;

pub use clock::{CallbackTracker, DeviceClock, FeedTiming};
pub use local::LocalFifo;
pub use peer_feed::{FeedEvents, PeerFeed, DEADLINE_GUARD_US};

use capture::Capture;
#[cfg(test)]
use capture::drain_frames;
use device_watch::DeviceWatch;
use local_path::LocalPath;
use metronome_stage::MetronomeStage;
pub(crate) use peer_sources::{PeerSources, PeerSourcesParts};
use send::SendPath;

/// 워커 폴링 주기. Windows에서도 std::thread::sleep은 고해상도 타이머를 쓴다.
pub(super) const POLL: Duration = Duration::from_micros(500);
/// 피어 은닉 마감이 가까울 때의 폴링 주기.
pub(super) const FINE_POLL: Duration = Duration::from_micros(150);
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


/// 조정자가 갖는 피어 표 복사본(엔진 ③c)과 그것을 맞추는 net. 네트워크가 있을 때만 있다.
/// 단계에는 이 묶음이 아니라 안의 복사본만 빌려 준다(`SendPath`: 송신·자동 코덱이 락 없이 본다).
struct PeerTable {
    net: Arc<NetShared>,
    copy: PeerTableCopy,
}

impl PeerTable {
    fn new(net: Arc<NetShared>) -> Self {
        Self { net, copy: PeerTableCopy::for_worker() }
    }

    /// 반복마다. 세대가 같으면 원자 load 하나, 바뀐 반복에서만 `try_read`로 다시 복사한다(기다리지 않는다).
    fn refresh(&mut self) {
        self.net.refresh_copy(&mut self.copy);
    }
}

/// 조정자. 필드 = 문맥 + 단계들 + 조정자가 갖고 단계에 빌려 주는 것(`peer_table`·`lock_wait`).
pub struct Worker {
    ctx: WorkerCtx,
    capture: Capture,
    /// 송신 단계. 네트워크가 있을 때만(없으면 캡처 프레임은 버려진다).
    send: Option<SendPath>,
    /// 시각 단계(출력 콜백 시각 추정).
    clock: CallbackTracker,
    metro: MetronomeStage,
    peers: PeerSources,
    local: LocalPath,
    watch: DeviceWatch,
    peer_table: Option<PeerTable>,
    /// 이번 반복에 남은 락(에코 프로브·메트로놈)에서 기다린 시간(엔진 ③c 측정 — 본문에서 빼서 락 밖 비용을 본다).
    /// 락을 잡는 단계 메서드에 빌려 준다.
    lock_wait: LockWait,
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
        // 실패 순서는 전과 같다: 입력 리샘플러 → 로컬 리샘플러 → 설정 코덱 인코더 → 프레임 크기 → 자동 전환 인코더 풀.
        let capture = Capture::new(in_cons, info.in_rate, ich, frame)?;
        let local = LocalPath::new(ich, info.out_rate, local_prod)?;
        // 프레임 크기 검증은 네트워크가 없어도 한다(전과 같다). 인코더가 먼저 실패하면 그 에러가 먼저 나가게 `SendPath::new` 안에서
        // 인코더를 만든 뒤 코드를 쓴다 — 그래서 코드 계산은 여기서 하되 에러는 인코더 뒤에 낸다.
        let frame_code = frame_code_from_samples(frame);
        let send = match &net {
            Some(net) => Some(SendPath::new(net.clone(), &cfg, epoch, frame_code, probe.clone())?),
            None => {
                frame_code.ok_or("잘못된 프레임 크기")?;
                None
            }
        };
        // 로컬 소스는 항상 켜져 있다(모니터 꺼짐 = 버스 게인 0, 탭에는 계속 나감).
        let local_slot = &bank.slots[LOCAL_SOURCE];
        local_slot.set_gains((0.0, 0.0), (1.0, 1.0));
        local_slot.state.store(SLOT_ACTIVE, Ordering::Release);
        Ok(Self {
            capture,
            send,
            clock: CallbackTracker::new(info.out_rate),
            metro: MetronomeStage::new(metronome, net.clone()),
            peers: PeerSources::new(PeerSourcesParts { prods: peer_prods, port: media_port, settings: media_settings, rec_marks, probe }),
            local,
            watch: DeviceWatch::new(),
            peer_table: net.map(PeerTable::new),
            lock_wait: Default::default(),
            ctx: WorkerCtx { cfg, info, counters, controls, bank, stats, epoch, stop },
        })
    }

    /// 워커 루프. **호출 순서와 원자값·시계를 읽는 시점이 계약이다**(설계 5절) — 읽기 좋게 재배열하지 말 것:
    /// - `pass_at`(송신 큐 지연의 시작점)은 입력 링을 비우기 **직전**에 한 번. 한 패스의 프레임은 모두 이 값을 쓴다(C7).
    /// - 48k 블록을 **모두** 로컬 경로에 넣은 뒤에 프레임을 보낸다(블록마다 `local.push`).
    /// - `clock.observe`(`FeedTiming`)는 캡처·인코드·송신 **뒤**, 메트로놈·피어 공급 **앞**에서 한 번(J5). 앞으로 옮기면 실시간
    ///   은닉 마감 판단이 송신 시간만큼 이르게 된다.
    /// - 로컬 FIFO는 `configure` → `push`(블록마다) → `service` 순.
    pub fn run(mut self) {
        let _prio = crate::sys::prio::promote_engine_thread(&self.ctx.stats.thread_prio);
        log::info!("워커 시작: frame {} ch {}", self.ctx.cfg.frame_samples(), self.ctx.cfg.channels);
        let c = &self.ctx;
        let mut last_start = Instant::now();
        while !c.stop.load(Ordering::Relaxed) {
            let start = Instant::now();
            if DIAG_TIMING {
                c.stats.iter_period.record(start - last_start);
            }
            last_start = start;
            // 피어 표 복사본은 송신과 무관하게 반복마다 맞춘다: 캡처가 멈춰도(장치 분리) 빠진 피어가 쌓이지 않게(#126 리뷰 M-1).
            if let Some(t) = self.peer_table.as_mut() {
                t.refresh();
            }
            self.local.configure(&c.info, &c.counters, self.capture.granularity());
            // 송신 큐 지연의 시작점(P1-3, 리뷰 M1): 이 패스에서 링을 비우기 직전 시각. 이번 패스에서 완성되는 프레임은
            // 모두 이 시각을 쓴다 — 같은 패스의 뒤 프레임은 앞 프레임의 인코딩·송신을 기다린 시간까지 포함된다.
            // (캡처에 남는 것은 프레임 미만이므로, 여기서 나가는 프레임의 마지막 샘플은 모두 이번 패스에 들어왔다.)
            let pass_at = Instant::now();
            self.capture.fill();
            let muted = c.controls.input_muted.load(Ordering::Relaxed);
            while let Some(block) = self.capture.next_block(muted) {
                self.local.push(block);
            }
            while let Some(frame) = self.capture.next_frame() {
                // 네트워크가 없으면 프레임은 버려진다(전에는 `ts`만 올렸다 — 송신에서만 쓰는 값이라 관찰되지 않는다).
                if let (Some(tx), Some(table)) = (self.send.as_mut(), self.peer_table.as_mut()) {
                    tx.send_frame(frame, pass_at, &mut table.copy, &c.stats, &mut self.lock_wait);
                }
            }
            // 인자 평가 순서 = 읽는 순서: 벽시계 → 출력 버스트 → (observe 안) 콜백 카운터.
            let timing = self.clock.observe(&c.counters, c.wall_us(), out_burst_dev(&c.info, &c.counters) as usize, c.info.out_rate);
            self.metro.service(&c.info, &c.counters, &c.stats, &timing, &mut self.lock_wait);
            let round = self.peers.service(&c.bank, &c.counters, &c.stats, c.info.out_rate, &timing, &mut self.lock_wait);
            self.local.service(&c.info, &c.counters, &c.controls, &c.bank.slots[LOCAL_SOURCE], &c.stats, c.epoch);
            self.local.track_occupancy(c.info.out_rate, &c.stats, self.capture.pending48());
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
        self.peers.close(&c.bank);
        log::info!("워커 종료");
    }
}
