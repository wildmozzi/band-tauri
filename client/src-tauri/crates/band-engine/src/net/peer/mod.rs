//! 피어 하나의 net 상태(`Peer`): 키·연결 상태기계·에코·송신 경로 스냅샷·원격 보고 (설계 문서 3절).
//!
//! 하위 모듈(엔진 R-1 ⑥-7): `descriptor`(계약 DTO), `conn`(홀펀칭·경로 선택·타이밍), `ping`(Ping 평문·에코 상태).
//! 옛 경로(`net::peer::{LocalDescriptor, ConnState, ping_plain, …}`)는 아래 `pub use`로 그대로 쓴다.

mod conn;
pub mod descriptor;
pub mod ping;

pub use conn::*;
pub use descriptor::*;
pub use ping::*;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, MutexGuard};

use super::crypto::{KeyPair, PacketCipher, KEY_BYTES};
use band_dsp::media_link::{LossCell, MediaTx, PeerKey};
use band_dsp::types::{PathKind, PeerStateKind};

pub const MAX_PEER_ID_BYTES: usize = 64;


// ============================================================================
// 송신 경로 스냅샷(엔진 ③c, ADR D1): 워커가 `conn`·`echo` 락 없이 읽는 값
// ============================================================================
//
// 측정(engine-stage3-design.md 10절): 워커가 `conn`·`echo`를 잡으면, 그 락을 쥔 기본 우선순위 스레드(통계·커맨드·
// `band-net-ctrl`)가 선점될 때 워커가 수~수십 ms 멈춘다(`parking_lot`은 우선순위 상속이 없다). 그래서 두 상태를 바꾸는
// 쪽이 락을 쥔 채 원자값을 발행하고(가드의 `Drop` — 락 해제 **전**에 저장하므로 발행 순서가 락 순서와 같다), 워커는
// 원자값만 읽는다.

/// 경로 스냅샷 비트: 주소 IPv4 32 | 포트 16 | 종류 2 | 보낼 수 있음 1 | … | 유효 1(최상위).
const ROUTE_VALID: u64 = 1 << 63;
const ROUTE_SENDABLE: u64 = 1 << 50;

fn kind_bits(k: PathKind) -> u64 {
    match k {
        PathKind::Lan => 0,
        PathKind::Public => 1,
        PathKind::Relay => 2,
    }
}

/// `ConnState`의 송신 경로를 64비트 하나로. 경로가 IPv6이면(지금은 후보·릴레이·STUN 모두 IPv4만 받아 생기지 않는다)
/// 담을 수 없으므로 유효 비트 없이 낸다 — 그때 워커는 `try_lock`으로 직접 보고, 못 잡으면 그 프레임을 그 피어에게 건너뛴다.
pub(crate) fn encode_route(c: &ConnState) -> u64 {
    match (c.can_send_audio(), c.path) {
        (true, Some((SocketAddr::V4(a), kind))) => {
            ROUTE_VALID | ROUTE_SENDABLE | (kind_bits(kind) << 48) | ((a.port() as u64) << 32) | u32::from(*a.ip()) as u64
        }
        (true, Some((SocketAddr::V6(_), _))) => 0,
        _ => ROUTE_VALID,
    }
}

/// 스냅샷 해석: `Some(경로 또는 없음)` = 유효, `None` = 담을 수 없는 경로(락으로 직접 봐야 함).
pub(crate) fn decode_route(v: u64) -> Option<Option<(SocketAddr, PathKind)>> {
    if v & ROUTE_VALID == 0 {
        return None;
    }
    if v & ROUTE_SENDABLE == 0 {
        return Some(None);
    }
    let ip = std::net::Ipv4Addr::from(v as u32);
    let port = (v >> 32) as u16;
    let kind = match (v >> 48) & 3 {
        0 => PathKind::Lan,
        1 => PathKind::Public,
        _ => PathKind::Relay,
    };
    Some(Some((SocketAddr::from((ip, port)), kind)))
}

/// 반사 마감(피어 시계 기준 ns, 0 = 반사 안 함). `EchoState::reflecting(now)` = `enabled && now - t < ECHO_LAPSE`
/// = `now < t + ECHO_LAPSE`이므로 `enabled`일 때 `t + ECHO_LAPSE`를 내면 시각 비교 하나로 같은 답이 나온다
/// (`Instant` 차이는 ns 단위로 정확하다). 시간이 지나 요청이 끊기는 것도 틱 없이 반영된다.
pub(crate) fn echo_deadline_ns(e: &EchoState, born: Instant) -> u64 {
    match (e.enabled, e.remote_request_at) {
        (true, Some(t)) => ((t + ECHO_LAPSE).saturating_duration_since(born).as_nanos() as u64).max(1),
        _ => 0,
    }
}

/// `now`가 반사 마감 전인가(`echo_deadline_ns`의 짝).
pub(crate) fn before_deadline(deadline_ns: u64, now: Instant, born: Instant) -> bool {
    deadline_ns != 0 && (now.saturating_duration_since(born).as_nanos() as u64) < deadline_ns
}

/// `ConnState` 가드: 놓을 때(락 해제 전) 송신 경로 스냅샷을 발행한다. 상태를 바꾸는 모든 곳이 `Peer::conn()`을 거치므로
/// 빠뜨릴 수 없다.
pub(crate) struct ConnGuard<'a> {
    g: MutexGuard<'a, ConnState>,
    snap: &'a AtomicU64,
}

impl std::ops::Deref for ConnGuard<'_> {
    type Target = ConnState;
    fn deref(&self) -> &ConnState {
        &self.g
    }
}

impl std::ops::DerefMut for ConnGuard<'_> {
    fn deref_mut(&mut self) -> &mut ConnState {
        &mut self.g
    }
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        // 필드(`g`, 락)는 이 함수가 끝난 뒤에 놓인다 → 락을 쥔 채 저장.
        self.snap.store(encode_route(&self.g), Ordering::Release);
    }
}

/// `EchoState` 가드: 놓을 때(락 해제 전) 반사 마감을 발행한다.
pub(crate) struct EchoGuard<'a> {
    g: MutexGuard<'a, EchoState>,
    snap: &'a AtomicU64,
    born: Instant,
}

impl std::ops::Deref for EchoGuard<'_> {
    type Target = EchoState;
    fn deref(&self) -> &EchoState {
        &self.g
    }
}

impl std::ops::DerefMut for EchoGuard<'_> {
    fn deref_mut(&mut self) -> &mut EchoState {
        &mut self.g
    }
}

impl Drop for EchoGuard<'_> {
    fn drop(&mut self) {
        self.snap.store(echo_deadline_ns(&self.g, self.born), Ordering::Release);
    }
}

/// 원격 손실 보고 셀(엔진 ③c): 쓰는 쪽은 수신 스레드 하나(`set_remote_loss`), 읽는 쪽(워커 200ms, 통계)은 락을 잡지 않는다.
/// seqlock(#116과 같은 꼴): 홀수 버전 → fence(Release) → 필드 → 짝수 Release / 읽기 Acquire → 필드 → fence(Acquire) → 재확인.
#[derive(Default)]
struct RemoteLossCell {
    ver: AtomicU32,
    /// `loss_pct` 비트 << 32 | `conceal_pct` 비트.
    report: AtomicU64,
    /// 받은 시각(피어 시계 ns + 1, 0 = 없음).
    at: AtomicU64,
}

impl RemoteLossCell {
    fn write(&self, r: LossReport, at_ns: u64) {
        let v = self.ver.load(Ordering::Relaxed);
        self.ver.store(v.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.report.store(((r.loss_pct.to_bits() as u64) << 32) | r.conceal_pct.to_bits() as u64, Ordering::Relaxed);
        self.at.store(at_ns.saturating_add(1), Ordering::Relaxed);
        self.ver.store(v.wrapping_add(2), Ordering::Release);
    }

    /// 성공할 때까지 다시 읽는다. 쓰기 구간은 저장 두 번이고 쓰는 쪽(`band-net-recv`)은 MMCSS라 사실상 한두 번이다.
    fn read(&self) -> Option<(LossReport, u64)> {
        let mut tries = 0u32;
        loop {
            let v1 = self.ver.load(Ordering::Acquire);
            if v1 & 1 == 0 {
                let (r, at) = (self.report.load(Ordering::Relaxed), self.at.load(Ordering::Relaxed));
                std::sync::atomic::fence(Ordering::Acquire);
                if self.ver.load(Ordering::Relaxed) == v1 {
                    if at == 0 {
                        return None;
                    }
                    let rep = LossReport { loss_pct: f32::from_bits((r >> 32) as u32), conceal_pct: f32::from_bits(r as u32) };
                    return Some((rep, at - 1));
                }
            }
            tries += 1;
            if tries.is_multiple_of(64) {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

pub struct Peer {
    id: String,
    ssrc: u32,
    public_key: [u8; KEY_BYTES],
    /// 내 → 피어 송신 암호기.
    tx: PacketCipher,
    /// 피어 → 나 수신 암호기.
    rx: PacketCipher,
    candidates: Vec<(SocketAddr, CandidateKind)>,
    conn: Mutex<ConnState>,
    /// 송신 경로 스냅샷(`encode_route`, `ConnGuard`가 발행). 워커는 이것만 읽는다(엔진 ③c).
    route_snap: AtomicU64,
    /// 반사 마감(`echo_deadline_ns`, `EchoGuard`가 발행). 워커는 이것만 읽는다(엔진 ③c).
    echo_snap: AtomicU64,
    /// 스냅샷 시각의 기준(이 항목을 만든 시각).
    born: Instant,
    /// 피어별 수신 큐 생산자(엔진 ③-a3). `ringbuf` 생산자는 `&mut`가 필요하고 `Peer`는 `Arc`로 공유되므로 Mutex로
    /// 감싼다. 넣는 스레드는 `band-net-recv` 하나뿐이라(`push_audio`는 `recv_loop` 경로에서만) 경쟁자가 없다(설계 1.2절).
    media: Mutex<MediaTx>,
    /// `media.key()`의 복사본(락 없이 읽게).
    key: PeerKey,
    /// 워커가 발행하는 이 피어 스트림의 수신 손실 보고(Ping, P1-4e). `media` 락을 잡지 않고 읽게 따로 쥔다.
    rx_report: Arc<LossCell>,
    /// 이 피어로 보내는 Audio 패킷 seq(송신 키별). 에코로 되돌리는 패킷도 같은 카운터를 써서 nonce가 겹치지 않는다.
    /// 같은 키를 쓰는 살아 있는 항목(다른 peerId가 같은 descriptor를 보냄)끼리는 이 카운터를 같이 쓴다(`Net::add_peer`).
    tx_seq: Arc<AtomicU32>,
    /// 이 피어로 마지막에 보낸 Audio 타임스탬프(에코 시작 시 타임스탬프를 이어 붙이는 데 쓴다).
    tx_last_ts: AtomicU32,
    /// 이 피어로 마지막에 보낸 내 오디오의 코덱(`TX_CODEC_*` 비트, 0 = 아직 안 보냄). PeerStats.txCodec.
    tx_codec: AtomicU8,
    echo: Mutex<EchoState>,
    /// 이 피어 시계와 내 시계의 오프셋 추정(P2-3, Pong 평문의 응답자 시계로 갱신).
    clock: Mutex<band_dsp::clock::ClockSync>,
    /// 이 피어가 Ping으로 알려 온, 내 소리의 수신 손실 보고와 받은 시각(P1-4e). 손실 피드백 Opus 전환의 입력.
    remote_loss: RemoteLossCell,
    /// 이 피어가 Ping으로 알려 온 그쪽 입력 장치 버퍼(P2-8b): 0 = 없음, 아니면 `(받은 ms(born 기준) + 1) << 16 | µs`.
    /// 쓰는 쪽은 수신 스레드 하나, 읽는 쪽(통계)은 락 없이 한 번에 읽는다.
    remote_in_buf: AtomicU64,
    /// 이 피어에게서 마지막으로 받은 Audio 헤더 `frame_code`(0 = 아직 없음). 상대 송신 프레임(P2-8b).
    rx_frame_code: AtomicU8,
}

/// `Peer::tx_codec` 비트: 보낸 적 있음 / Opus / 릴레이 대역 때문에 자동 전환한 Opus(P1-4c) /
/// 수신 손실 피드백으로 자동 전환한 Opus(P1-4e). AUTO와 AUTO_LOSS가 둘 다 서면 이유는 릴레이 부하로 보인다.
pub const TX_CODEC_SENT: u8 = 0x80;
pub const TX_CODEC_OPUS: u8 = 0x01;
pub const TX_CODEC_AUTO: u8 = 0x02;
pub const TX_CODEC_AUTO_LOSS: u8 = 0x04;

/// `PeerStats`의 net 몫(상태·경로·RTT·에코·송신 코덱 비트·원격 손실 보고)을 값으로 담는다.
#[derive(Clone, Copy, Debug)]
pub struct PeerNetStats {
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<f32>,
    pub echo_requested: bool,
    pub echo_active: bool,
    pub peer_echoing: bool,
    /// `TX_CODEC_*` 비트.
    pub tx_codec: u8,
    pub remote_loss: Option<(LossReport, Instant)>,
    /// add_peer → 처음 Connected(ms, P1-20). 아직 연결 전이면 None.
    pub connect_ms: Option<f32>,
    /// 이 피어가 Ping으로 알려 온 그쪽 입력 장치 버퍼(µs)와 받은 시각(P2-8b). 옛 엔진이면 None.
    pub remote_in_buf: Option<(u16, Instant)>,
    /// 이 피어에게서 마지막으로 받은 Audio 헤더 `frame_code`(0 = 아직 없음, P2-8b).
    pub rx_frame_code: u8,
}

pub struct PeerSetup<'a> {
    pub keys: &'a KeyPair,
    pub my_ssrc: u32,
    pub allow_loopback: bool,
}

impl Peer {
    /// `media`: 엔진이 만든 이 피어의 수신 큐 생산자(`media_link`).
    pub fn new(id: String, desc: &LocalDescriptor, setup: &PeerSetup<'_>, media: MediaTx, now: Instant) -> Result<Self, String> {
        if id.is_empty() || id.len() > MAX_PEER_ID_BYTES {
            return Err("peerId는 1..64바이트여야 합니다".into());
        }
        let public_key = desc.decode_key()?;
        if public_key == setup.keys.public_bytes() {
            return Err("자기 자신의 descriptor입니다".into());
        }
        let candidates = desc.validated_candidates(setup.allow_loopback)?;
        let (tx, rx) = setup.keys.pair_ciphers(setup.my_ssrc, &public_key, desc.ssrc)?;
        Ok(Self {
            id,
            ssrc: desc.ssrc,
            public_key,
            tx,
            rx,
            candidates,
            route_snap: AtomicU64::new(encode_route(&ConnState::new(now))),
            echo_snap: AtomicU64::new(0),
            born: now,
            conn: Mutex::new(ConnState::new(now)),
            rx_report: media.loss_cell().clone(),
            key: media.key(),
            media: Mutex::new(media),
            tx_seq: Arc::new(AtomicU32::new(rand::random())),
            tx_last_ts: AtomicU32::new(0),
            tx_codec: AtomicU8::new(0),
            echo: Mutex::new(EchoState::default()),
            clock: Mutex::new(band_dsp::clock::ClockSync::new()),
            remote_loss: RemoteLossCell::default(),
            remote_in_buf: AtomicU64::new(0),
            rx_frame_code: AtomicU8::new(0),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn public_key(&self) -> &[u8; KEY_BYTES] {
        &self.public_key
    }

    /// 오디오를 보낼 수 있으면 지금 경로(주소, 종류). 락 없이 스냅샷에서 읽는다(엔진 ③c — 워커 송신 경로·자동 코덱).
    /// 스냅샷에 담을 수 없는 경로(IPv6, 지금은 생기지 않음)면 `try_lock`으로 직접 보고, 락이 바쁘면 기다리지 않고 None.
    pub fn route(&self) -> Option<(SocketAddr, PathKind)> {
        self.send_route().unwrap_or(None)
    }

    /// `route`와 같되, 담을 수 없는 경로인데 `conn` 락이 바빠 못 본 경우를 `Err`로 구분한다(송신 경로가 건너뛴 수를 센다).
    pub(super) fn send_route(&self) -> Result<Option<(SocketAddr, PathKind)>, ()> {
        match decode_route(self.route_snap.load(Ordering::Acquire)) {
            Some(r) => Ok(r),
            None => self.conn.try_lock().map(|c| if c.can_send_audio() { c.path } else { None }).ok_or(()),
        }
    }

    /// 지금 오디오를 릴레이로 보내는 중인가(자동 코덱·릴레이 부하 계산).
    pub fn sends_audio_via_relay(&self) -> bool {
        self.route().is_some_and(|(_, k)| k == PathKind::Relay)
    }

    /// 이 피어가 Ping으로 알려 온 내 소리의 수신 손실 보고와 받은 시각(락 없음, 엔진 ③c).
    pub fn remote_loss(&self) -> Option<(LossReport, Instant)> {
        self.remote_loss.read().map(|(r, ns)| (r, self.born + Duration::from_nanos(ns)))
    }

    /// 지금 이 피어의 오디오를 되돌려 보내는 중인가(에코 반사). 락 없이 반사 마감과 비교한다(엔진 ③c) —
    /// `EchoState::reflecting(now)`과 같은 답(`echo_deadline_ns`).
    pub fn reflecting(&self, now: Instant) -> bool {
        before_deadline(self.echo_snap.load(Ordering::Acquire), now, self.born)
    }

    /// `PeerStats`의 net 몫을 값으로.
    pub fn net_stats(&self, now: Instant) -> PeerNetStats {
        let (state, path, rtt_ms, connect_ms) = {
            let c = self.conn.lock();
            let connect_ms = c.connected_at.map(|t| t.saturating_duration_since(c.started).as_secs_f32() * 1000.0);
            (c.state, c.path.map(|(_, k)| k), c.rtt_ms, connect_ms)
        };
        let (echo_requested, echo_active, peer_echoing) = {
            let e = self.echo.lock();
            (e.remote_requesting(now) && !e.enabled, e.reflecting(now), e.remote_echoing(now))
        };
        PeerNetStats {
            state,
            path,
            rtt_ms,
            echo_requested,
            echo_active,
            peer_echoing,
            tx_codec: self.tx_codec.load(Ordering::Relaxed),
            remote_loss: self.remote_loss(),
            connect_ms,
            remote_in_buf: self.remote_input_buffer(),
            rx_frame_code: self.rx_frame_code.load(Ordering::Relaxed),
        }
    }

    /// 이 피어 미디어 상태의 키(엔진 등록부·워커와 짝짓는 값).
    pub fn key(&self) -> PeerKey {
        self.key
    }

    /// 수신 오디오를 피어별 수신 큐에 넣는다(가득 차면 버리고 센다). `recv_loop` 경로(`handle_packet`)에서만 부른다 —
    /// 다른 스레드가 부르면 SPSC가 Mutex로 직렬화될 뿐 깨지지는 않지만 무경쟁 가정이 깨진다(③-a3 리뷰 항목).
    pub(super) fn push_audio(&self, hdr: &band_dsp::packet::Header, payload: &[u8], arrival_us: u64) {
        // 프레임은 거의 바뀌지 않는다 — 같으면 쓰지 않아 패킷마다 캐시라인을 더럽히지 않는다(P2-8b 리뷰 Low-3)
        if self.rx_frame_code.load(Ordering::Relaxed) != hdr.frame_code {
            self.rx_frame_code.store(hdr.frame_code, Ordering::Relaxed);
        }
        self.media.lock().push(hdr, payload, arrival_us);
    }

    /// 이 피어 스트림의 수신 손실 보고(재생 중이 아니면 None). Ping에 싣는다(P1-4e).
    pub(super) fn loss_report(&self) -> Option<LossReport> {
        self.rx_report.get().map(|(loss_pct, conceal_pct)| LossReport { loss_pct, conceal_pct })
    }

    pub(super) fn tx(&self) -> &PacketCipher {
        &self.tx
    }

    pub(super) fn rx(&self) -> &PacketCipher {
        &self.rx
    }

    pub(super) fn candidates(&self) -> &[(SocketAddr, CandidateKind)] {
        &self.candidates
    }

    /// 상태를 바꾸는 곳은 모두 이 가드를 거친다 — 놓을 때 송신 경로 스냅샷을 발행한다(엔진 ③c).
    pub(super) fn conn(&self) -> ConnGuard<'_> {
        ConnGuard { g: self.conn.lock(), snap: &self.route_snap }
    }

    /// 상태를 바꾸는 곳은 모두 이 가드를 거친다 — 놓을 때 반사 마감을 발행한다(엔진 ③c).
    pub(super) fn echo(&self) -> EchoGuard<'_> {
        EchoGuard { g: self.echo.lock(), snap: &self.echo_snap, born: self.born }
    }

    pub(super) fn clock(&self) -> MutexGuard<'_, band_dsp::clock::ClockSync> {
        self.clock.lock()
    }

    /// 이 피어 시계 − 내 시계 추정(P2-3). `at_us` = 내 시계. 표본이 없으면 None. **워커 전용**(메트로놈 단계, 엔진 ②b-6b):
    /// 워커가 피어 표 복사본에서 찾은 피어에 부른다 — 피어 표 락을 거치지 않는다. 잡는 락은 `clock` 하나이고 그것을 나누는
    /// 상대는 `band-net-recv`뿐이다(둘 다 MMCSS). 기본 우선순위 스레드가 `clock`을 잡게 되면 이 규칙을 다시 열어야 한다
    /// (ADR D1). 가시성으로는 적을 수 없어(`net`의 메서드를 `engine::worker`에만 보이게 할 수 없다) `tests/architecture.rs`의
    /// `peer_clock_is_read_only_by_worker`가 부르는 파일을 워커 디렉터리로 고정한다. 기다렸으면 `w`·`site`에 적는다(진단).
    pub(crate) fn clock_estimate(&self, at_us: u64, w: &mut super::txlat::LockWait, site: &super::txlat::LockSite) -> Option<band_dsp::clock::ClockEstimate> {
        w.lock(&self.clock, site).estimate(at_us as f64)
    }

    /// 이 피어가 Pong에 시계를 싣지 않는 엔진(옛 버전)으로 보이는지(PR #34 L4, `ClockSync::unsupported`). 워커 전용(위와 같음).
    pub(crate) fn clock_unsupported(&self, w: &mut super::txlat::LockWait, site: &super::txlat::LockSite) -> bool {
        w.lock(&self.clock, site).unsupported()
    }

    /// 이 피어가 Ping으로 알려 온 그쪽 입력 장치 버퍼(µs)와 받은 시각(P2-8b, 락 없음).
    pub fn remote_input_buffer(&self) -> Option<(u16, Instant)> {
        match self.remote_in_buf.load(Ordering::Relaxed) {
            0 => None,
            v => Some((v as u16, self.born + Duration::from_millis((v >> 16) - 1))),
        }
    }

    pub(super) fn set_remote_input_buffer(&self, us: u16, at: Instant) {
        let ms = (at.saturating_duration_since(self.born).as_millis() as u64).min((1u64 << 47) - 2);
        self.remote_in_buf.store(((ms + 1) << 16) | us as u64, Ordering::Relaxed);
    }

    pub(super) fn set_remote_loss(&self, r: LossReport, at: Instant) {
        self.remote_loss.write(r, at.saturating_duration_since(self.born).as_nanos() as u64);
    }

    pub(super) fn tx_seq(&self) -> &AtomicU32 {
        &self.tx_seq
    }

    pub(super) fn tx_last_ts(&self) -> &AtomicU32 {
        &self.tx_last_ts
    }

    pub(super) fn tx_codec(&self) -> &AtomicU8 {
        &self.tx_codec
    }

    /// 같은 송신 키를 쓰는 살아 있는 항목과 seq 카운터를 같이 쓴다(`Net::add_peer`, 공유 전 단계).
    pub(super) fn share_tx_seq(&mut self, live: &Peer) {
        self.tx_seq = live.tx_seq.clone();
    }

    /// 테스트용: 경로를 강제로 세운다(핸드셰이크 없이).
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_path(&self, addr: SocketAddr, kind: PathKind) {
        let mut c = self.conn();
        c.state = PeerStateKind::Connected;
        c.path = Some((addr, kind));
        c.last_rx = Some(Instant::now());
    }

    /// 테스트용: 두 항목이 송신 seq 카운터를 같이 쓰는가.
    #[cfg(any(test, feature = "test-support"))]
    pub fn shares_tx_seq_with(&self, other: &Peer) -> bool {
        Arc::ptr_eq(&self.tx_seq, &other.tx_seq)
    }

    /// 직접 Hello를 보낼 주소들: 모든 후보 + (다르면) 현재 직접 경로 + 리바인딩 후보 주소. 최대 10개.
    /// 릴레이 주소는 빠진다(릴레이로는 RelayData로 따로 보낸다).
    pub fn hello_targets(&self, relay: Option<SocketAddr>) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self.candidates.iter().map(|(a, _)| *a).collect();
        let c = self.conn.lock();
        let direct_path = c.path.filter(|(_, k)| *k != PathKind::Relay).map(|(a, _)| a);
        for extra in [direct_path, c.rebind.map(|(a, _)| a)].into_iter().flatten() {
            if !v.contains(&extra) {
                v.push(extra);
            }
        }
        v.retain(|a| Some(*a) != relay);
        v.truncate(MAX_CANDIDATES + 2);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use band_dsp::packet::PacketType;

    fn cands() -> Vec<(SocketAddr, CandidateKind)> {
        vec![
            ("192.168.0.5:4000".parse().unwrap(), CandidateKind::Lan),
            ("203.0.113.9:5000".parse().unwrap(), CandidateKind::Public),
        ]
    }

    #[test]
    fn lan_preferred_over_public() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        c.on_handshake("203.0.113.9:5000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
        assert_eq!(c.state, PeerStateKind::Connected);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // LAN 확정 후 public 응답은 무시
        assert_eq!(c.on_handshake("203.0.113.9:5000".parse().unwrap(), false, &cands(), None, t0), HandshakeOutcome::Ignored);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // 후보에 없는 주소(peer-reflexive)는 public 취급
        let mut c = ConnState::new(t0);
        c.on_handshake("198.51.100.1:7".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
    }

    /// P1-20 `connectMs`: 처음 Connected가 된 시각만 남고, 끊김 → 재연결·경로 승격에도 바뀌지 않는다.
    #[test]
    fn connected_at_is_first_connect_only() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.connected_at, None);
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let t1 = t0 + Duration::from_millis(120);
        c.on_handshake(public, false, &cands(), None, t1);
        assert_eq!(c.connected_at, Some(t1));
        // LAN 승격
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t1 + Duration::from_millis(50));
        assert_eq!(c.connected_at, Some(t1));
        // 끊김 → 재연결
        c.tick(t1 + Duration::from_secs(6), false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t1 + Duration::from_secs(7));
        assert_eq!(c.state, PeerStateKind::Connected);
        assert_eq!(c.connected_at, Some(t1));
    }

    #[test]
    fn hello_from_other_address_does_not_refresh_liveness() {
        // 리뷰 M6: 경로가 아닌 주소의 Hello는 last_rx/상태를 갱신하지 않는다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let path: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let other: SocketAddr = "198.51.100.1:7".parse().unwrap();
        c.on_handshake(path, false, &cands(), None, t0);
        let t1 = t0 + Duration::from_secs(3);
        assert_eq!(c.on_handshake(other, false, &cands(), None, t1), HandshakeOutcome::Ignored);
        assert_eq!(c.last_rx, Some(t0), "last_rx 유지");
        // 5초가 지나면 끊김으로 판정되어야 한다(가짜 Hello로 살아 있는 척 불가)
        c.tick(t0 + Duration::from_secs(5), false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
    }

    #[test]
    fn nat_rebinding_switches_after_consecutive_hellos() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let old: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let new: SocketAddr = "203.0.113.9:6000".parse().unwrap();
        c.on_handshake(old, false, &cands(), None, t0);
        for i in 1..REBIND_HELLOS {
            assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored, "{i}");
        }
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Accepted);
        assert_eq!(c.path.unwrap().0, new);
        // 중간에 원래 경로에서 오면 카운트 초기화
        let mut c = ConnState::new(t0);
        c.on_handshake(old, false, &cands(), None, t0);
        for _ in 0..REBIND_HELLOS - 1 {
            c.on_handshake(new, false, &cands(), None, t0);
        }
        c.on_handshake(old, false, &cands(), None, t0);
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored);
    }

    #[test]
    fn ctrl_replay_rejected() {
        let mut c = ConnState::new(Instant::now());
        assert!(c.accept_ctrl_seq(PacketType::Bye, 10));
        assert!(!c.accept_ctrl_seq(PacketType::Bye, 10), "같은 seq 재전송");
        assert!(!c.accept_ctrl_seq(PacketType::Bye, 3), "옛 seq");
        assert!(c.accept_ctrl_seq(PacketType::Bye, 11));
        assert!(c.accept_ctrl_seq(PacketType::Ping, 3), "타입별 독립");
        // u32 랩
        let mut c = ConnState::new(Instant::now());
        assert!(c.accept_ctrl_seq(PacketType::Hello, u32::MAX));
        assert!(c.accept_ctrl_seq(PacketType::Hello, 0));
    }

    #[test]
    fn hello_ack_rate_limited() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let (a, b): (SocketAddr, SocketAddr) = ("192.168.0.5:4000".parse().unwrap(), "203.0.113.9:5000".parse().unwrap());
        assert!(c.ack_allowed(a, t0));
        assert!(!c.ack_allowed(a, t0 + Duration::from_millis(10)));
        // 다른 주소(두 번째 후보 경로)는 굶지 않는다(PR #5 L-4)
        assert!(c.ack_allowed(b, t0 + Duration::from_millis(10)));
        assert!(c.ack_allowed(a, t0 + Duration::from_millis(60)));
        // 주소 수가 많아도 기억 공간은 제한된다
        for i in 0..50u16 {
            c.ack_allowed(SocketAddr::from(([198, 51, 100, 1], 1000 + i)), t0);
        }
        assert!(c.last_acks.len() <= MAX_CANDIDATES + 2);
    }

    #[test]
    fn rebind_count_reset_by_path_traffic() {
        // PR #5 L-3: 경로에서 Ping/Audio가 오면 리바인딩 카운트가 지워져 "연속 5회"가 유지된다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let old: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let new: SocketAddr = "203.0.113.9:6000".parse().unwrap();
        c.on_handshake(old, false, &cands(), None, t0);
        for _ in 0..REBIND_HELLOS - 1 {
            c.on_handshake(new, false, &cands(), None, t0);
            c.on_rx_from_path(t0);
        }
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored, "누적이 아니라 연속이어야 함");
        assert_eq!(c.path.unwrap().0, old);
    }

    #[test]
    fn replay_window_inherited() {
        let t0 = Instant::now();
        let mut old = ConnState::new(t0);
        assert!(old.accept_ctrl_seq(PacketType::Bye, 100));
        let mut fresh = ConnState::new(t0);
        fresh.inherit_replay_window(&old);
        assert!(!fresh.accept_ctrl_seq(PacketType::Bye, 100), "재추가 전에 캡처한 Bye 재전송 거부(PR #5 L-2)");
        assert!(fresh.accept_ctrl_seq(PacketType::Bye, 101));
    }

    #[test]
    fn state_machine_timeouts() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.tick(t0, false), TickAction::HELLO);
        assert_eq!(c.tick(t0 + Duration::from_millis(50), false), TickAction::NONE);
        assert_eq!(c.tick(t0 + Duration::from_millis(100), false), TickAction::HELLO);
        assert_eq!(c.tick(t0 + Duration::from_secs(10), false), TickAction::NONE);
        assert_eq!(c.state, PeerStateKind::Failed);

        let mut c = ConnState::new(t0);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.tick(t0, false), TickAction::PING);
        assert_eq!(c.tick(t0 + Duration::from_millis(400), false), TickAction::NONE);
        assert_eq!(c.tick(t0 + Duration::from_millis(500), false), TickAction::PING);
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(c.tick(t5, false), TickAction::HELLO);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        assert!(c.can_send_audio());
        c.on_rx_from_path(t5 + Duration::from_secs(1));
        assert_eq!(c.state, PeerStateKind::Connected);
        let t_lost = t5 + Duration::from_secs(6);
        c.tick(t_lost, false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        c.tick(t5 + Duration::from_secs(16), false);
        assert_eq!(c.state, PeerStateKind::Failed);
        // Failed 뒤에도 2초마다 탐색(늦게 열린 릴레이/경로로 회복). Bye를 받았으면 하지 않는다.
        let tf = t5 + Duration::from_secs(16);
        assert_eq!(c.tick(tf + FAILED_PROBE_INTERVAL, true), TickAction { hello_direct: true, hello_relay: true, ping: false });
        c.bye = true;
        assert_eq!(c.tick(tf + FAILED_PROBE_INTERVAL * 3, true), TickAction::NONE);
    }

    const RELAY: &str = "198.51.100.200:3478";

    fn relay() -> Option<SocketAddr> {
        Some(RELAY.parse().unwrap())
    }

    #[test]
    fn relay_hello_after_fallback_delay_only_when_ready() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.tick(t0, true), TickAction::HELLO, "처음에는 직접만");
        let tf = t0 + RELAY_FALLBACK_AFTER;
        assert_eq!(c.tick(tf, false), TickAction::HELLO, "릴레이 미등록이면 직접만");
        let a = c.tick(tf + HELLO_INTERVAL, true);
        assert!(a.hello_direct && a.hello_relay, "{a:?}");
    }

    #[test]
    fn relay_path_established_and_direct_promoted_by_ack() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        assert_eq!(c.on_handshake(r, false, &cands(), relay(), t0), HandshakeOutcome::Accepted);
        assert_eq!(c.path, Some((r, PathKind::Relay)));
        assert!(c.relay_confirmed);
        // 릴레이 경로에서는 직접 경로를 1초마다 계속 찾는다
        let a = c.tick(t0, true);
        assert!(a.ping && a.hello_direct && !a.hello_relay, "{a:?}");
        assert!(!c.tick(t0 + Duration::from_millis(500), true).hello_direct);
        assert!(c.tick(t0 + DIRECT_PROBE_INTERVAL, true).hello_direct);
        // 직접 주소의 Hello만으로는 승격하지 않는다(왕복 미확인) — 힌트만
        assert_eq!(c.on_handshake(lan, false, &cands(), relay(), t0), HandshakeOutcome::Ignored);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert_eq!(c.direct_hint, Some(lan));
        // HelloAck(왕복 확인)가 1초 간격으로 3번, 2초 이상 이어져야 승격(M2 히스테리시스)
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0), HandshakeOutcome::Ignored);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + Duration::from_secs(1)), HandshakeOutcome::Ignored);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        let tp = t0 + Duration::from_secs(2);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), tp), HandshakeOutcome::Accepted);
        assert_eq!(c.path, Some((lan, PathKind::Lan)));
        // 비행 중인 릴레이 패킷은 계속 받는다(무끊김), 생존은 경로 기준
        assert!(c.accept_rx(r, &cands(), relay(), tp + Duration::from_millis(10)));
        assert!(!c.accept_rx("198.51.100.9:1".parse().unwrap(), &cands(), relay(), tp), "모르는 주소");
    }

    #[test]
    fn promotion_requires_unbroken_acks_and_acceptable_rtt() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let s = Duration::from_secs(1);
        // 중간에 2초 끊기면(손실) 처음부터 다시 센다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.on_handshake(lan, true, &cands(), relay(), t0);
        c.on_handshake(lan, true, &cands(), relay(), t0 + s);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + s * 3), HandshakeOutcome::Ignored, "간격 2초 → 리셋");
        c.on_handshake(lan, true, &cands(), relay(), t0 + s * 4);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + s * 5), HandshakeOutcome::Accepted);
        // 직접 RTT(80ms)가 릴레이 종단 RTT(40ms) + 10ms보다 크면 승격하지 않는다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.rtt_ms = Some(40.0);
        for i in 0..5 {
            assert_eq!(c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * i, Some(80.0)), HandshakeOutcome::Ignored);
        }
        // 직접이 더 빠르면 승격하고, RTT는 직접 경로 값으로 바뀐다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.rtt_ms = Some(40.0);
        for i in 0..2 {
            c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * i, Some(4.0));
        }
        assert_eq!(c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * 2, Some(4.0)), HandshakeOutcome::Accepted);
        assert_eq!(c.rtt_ms, Some(4.0));
    }

    /// 직접 경로 승격까지(1초 간격 HelloAck 3회). 승격 시각을 돌려준다.
    fn promote(c: &mut ConnState, addr: SocketAddr, from: Instant) -> Instant {
        let mut t = from;
        for _ in 0..PROMOTE_ACKS {
            c.on_handshake(addr, true, &cands(), relay(), t);
            t += Duration::from_secs(1);
        }
        t - Duration::from_secs(1)
    }

    #[test]
    fn demotion_holds_promotion_and_backs_off_on_flapping() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(lan, false, &cands(), relay(), t0);
        // 처음 강등(직접 → 릴레이): 5초 동안 승격 보류
        let td = t0 + Duration::from_secs(60);
        c.on_handshake(r, false, &cands(), relay(), td);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert_eq!(c.promote_hold_until, Some(td + RELAY_HOLD_MIN));
        let tp = promote(&mut c, lan, td + Duration::from_secs(1));
        assert_eq!(c.path_kind(), Some(PathKind::Relay), "유예 중에는 HelloAck가 이어져도 승격 안 함 ({tp:?})");
        let tp = promote(&mut c, lan, td + RELAY_HOLD_MIN);
        assert_eq!(c.path_kind(), Some(PathKind::Lan), "유예 뒤 승격");
        // 승격 후 30초 안에 다시 강등(플래핑) → 유예 10초, 다음은 20초 … 최대 60초
        let mut t = tp + Duration::from_secs(3);
        let mut holds = vec![];
        for _ in 0..5 {
            c.on_handshake(r, false, &cands(), relay(), t);
            holds.push(c.promote_hold_until.unwrap() - t);
            let hold_end = c.promote_hold_until.unwrap();
            let tp = promote(&mut c, lan, hold_end);
            assert_eq!(c.path_kind(), Some(PathKind::Lan));
            t = tp + Duration::from_secs(3);
        }
        let secs: Vec<u64> = holds.iter().map(|d| d.as_secs()).collect();
        assert_eq!(secs, vec![10, 20, 40, 60, 60]);
        // 승격이 30초 넘게 안정적이었으면 처음(5초)부터
        let t_late = t + FLAP_WINDOW;
        c.on_handshake(r, false, &cands(), relay(), t_late);
        assert_eq!(c.promote_hold_until, Some(t_late + RELAY_HOLD_MIN));
        assert_eq!(c.flaps, 0);
    }

    #[test]
    fn follows_peer_promotion_even_during_hold() {
        // 상대가 먼저 승격해 직접 경로로 보내면 유예 중이어도 따라간다(안 그러면 그 소리를 버린다)
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(lan, false, &cands(), relay(), t0);
        c.on_handshake(r, false, &cands(), relay(), t0 + Duration::from_secs(1));
        assert!(c.promote_hold_until.is_some());
        c.on_handshake(lan, false, &cands(), relay(), t0 + Duration::from_secs(2));
        assert!(c.accept_rx(lan, &cands(), relay(), t0 + Duration::from_secs(2)));
        assert_eq!(c.path_kind(), Some(PathKind::Lan));
    }

    #[test]
    fn relay_address_change_moves_relay_path() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let r2: SocketAddr = "198.51.100.201:3478".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.relay_moved(r, Some(r2), t0);
        assert_eq!(c.path, Some((r2, PathKind::Relay)), "새 릴레이로 바로 옮긴다(Reconnecting 없이)");
        assert_eq!(c.state, PeerStateKind::Connected);
        assert!(c.accept_rx(r2, &cands(), Some(r2), t0));
        assert!(c.accept_rx(r, &cands(), Some(r2), t0 + Duration::from_secs(1)), "옛 릴레이의 비행 중 패킷");
        // 해제: 경로를 비우고 직접 경로를 다시 찾는다
        c.relay_moved(r2, None, t0);
        assert_eq!(c.path, None);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        assert!(!c.relay_confirmed);
        assert!(c.tick(t0 + Duration::from_millis(200), false).hello_direct);
        // 직접 경로 피어는 그대로
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut d = ConnState::new(t0);
        d.on_handshake(lan, false, &cands(), relay(), t0);
        d.relay_moved(r, Some(r2), t0);
        assert_eq!(d.path, Some((lan, PathKind::Lan)));
    }

    #[test]
    fn follows_peer_promotion_via_direct_hint() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        c.on_handshake(r, true, &cands(), relay(), t0);
        assert!(!c.accept_rx(public, &cands(), relay(), t0), "힌트 없으면 직접 주소 패킷 폐기");
        c.on_handshake(public, false, &cands(), relay(), t0);
        // 상대가 승격해 그 주소로 Ping/Audio를 보내면 따라 승격
        assert!(c.accept_rx(public, &cands(), relay(), t0));
        assert_eq!(c.path, Some((public, PathKind::Public)));
        assert_eq!(c.prev_path.map(|(a, _)| a), Some(r));
    }

    #[test]
    fn falls_back_to_relay_when_direct_goes_quiet() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        c.on_handshake(lan, false, &cands(), relay(), t0);
        assert!(!c.tick(t0 + Duration::from_secs(1), true).hello_relay, "직접 경로가 살아 있으면 릴레이 안 씀");
        let tq = t0 + RELAY_PROBE_AFTER;
        assert!(c.tick(tq, true).hello_relay, "조용해지면 릴레이로도 Hello");
        assert!(!c.tick(tq, false).hello_relay);
        // 상대의 릴레이 Hello/HelloAck → 릴레이 경로로 전환, 직전 직접 경로는 잠시 수용
        assert_eq!(c.on_handshake(r, false, &cands(), relay(), tq), HandshakeOutcome::Accepted);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert!(c.accept_rx(lan, &cands(), relay(), tq + Duration::from_secs(1)));
        assert!(!c.accept_rx(lan, &cands(), relay(), tq + PATH_GRACE), "유예 이후 폐기");
    }

    #[test]
    fn relay_data_without_relay_handshake_rejected() {
        // 릴레이를 거친 Hello/HelloAck가 없었던 피어는 릴레이 주소에서 온 데이터를 받지 않는다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), relay(), t0);
        assert!(!c.accept_rx(RELAY.parse().unwrap(), &cands(), relay(), t0));
    }

    fn test_peer() -> Peer {
        let keys = KeyPair::generate();
        let other = KeyPair::generate();
        let desc = LocalDescriptor {
            ssrc: 9,
            key: LocalDescriptor::encode_key(&other.public_bytes()),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: "192.168.0.5:4000".into() }],
        };
        let setup = PeerSetup { keys: &keys, my_ssrc: 1, allow_loopback: false };
        let (media, _rx) = band_dsp::media_link::media_link(PeerKey::from_raw(1));
        Peer::new("p".into(), &desc, &setup, media, Instant::now()).unwrap()
    }

    /// 엔진 ③c: 경로 스냅샷 왕복 — IPv4 경로는 그대로, 보낼 수 없으면 없음, IPv6이면 유효 비트 없음.
    #[test]
    fn route_snapshot_roundtrip() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(decode_route(encode_route(&c)), Some(None), "처음엔 경로 없음(유효)");
        for (addr, kind) in [("192.168.0.5:4000", PathKind::Lan), ("203.0.113.9:65535", PathKind::Public), ("198.51.100.1:1", PathKind::Relay)] {
            c.state = PeerStateKind::Connected;
            c.path = Some((addr.parse().unwrap(), kind));
            assert_eq!(decode_route(encode_route(&c)), Some(Some((addr.parse().unwrap(), kind))));
            c.state = PeerStateKind::Reconnecting;
            assert_eq!(decode_route(encode_route(&c)), Some(Some((addr.parse().unwrap(), kind))), "재연결 중에도 보낸다");
            c.state = PeerStateKind::Failed;
            assert_eq!(decode_route(encode_route(&c)), Some(None), "실패면 안 보낸다");
        }
        c.state = PeerStateKind::Connected;
        c.path = Some(("[2001:db8::1]:4000".parse().unwrap(), PathKind::Public));
        assert_eq!(decode_route(encode_route(&c)), None, "IPv6은 담을 수 없다 → 락으로 직접");
    }

    /// 엔진 ③c: `conn()`·`echo()` 가드가 놓을 때 스냅샷을 발행한다 — 락 없는 `route()`·`reflecting()`이 바로 본다.
    /// 담을 수 없는 경로(IPv6)는 `try_lock`으로 보고, 락이 바쁘면 기다리지 않고 `Err`(건너뜀).
    #[test]
    fn guards_publish_snapshots() {
        let p = test_peer();
        assert_eq!(p.route(), None);
        let a: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        p.force_path(a, PathKind::Lan);
        assert_eq!(p.route(), Some((a, PathKind::Lan)));
        p.conn().state = PeerStateKind::Failed;
        assert_eq!(p.route(), None);
        // IPv6: 스냅샷 무효 → try_lock 경로
        let v6: SocketAddr = "[2001:db8::1]:4000".parse().unwrap();
        p.force_path(v6, PathKind::Public);
        assert_eq!(p.send_route(), Ok(Some((v6, PathKind::Public))));
        let held = p.conn.lock();
        assert_eq!(p.send_route(), Err(()), "락이 바쁘면 기다리지 않는다");
        assert_eq!(p.route(), None);
        drop(held);
        // 에코: 요청 + 동의 → 반사, 동의 거두면 즉시 아님
        let now = Instant::now();
        assert!(!p.reflecting(now));
        {
            let mut e = p.echo();
            e.remote_request_at = Some(now);
            e.enabled = true;
        }
        assert!(p.reflecting(now));
        assert!(p.reflecting(now + ECHO_LAPSE - Duration::from_nanos(1)));
        assert!(!p.reflecting(now + ECHO_LAPSE), "요청이 끊기면 틱 없이도 반사가 멈춘다");
        p.echo().stop_reflecting();
        assert!(!p.reflecting(now));
    }

    /// 엔진 ③c: 반사 마감 비교가 `EchoState::reflecting(now)`과 같은 답 — 동의 여부, 요청 시각(기준 전·후), 확인 시각을
    /// 경계(마감 −1ns, 마감, +1ns)와 그 사이에서 모두 비교한다.
    #[test]
    fn echo_deadline_matches_reflecting() {
        let born = Instant::now();
        let ns = Duration::from_nanos;
        for enabled in [false, true] {
            for req_off in [None, Some(0u64), Some(1), Some(1_500_000_000), Some(7_000_000_123)] {
                let e = EchoState { enabled, remote_request_at: req_off.map(|o| born + ns(o)), ..EchoState::default() };
                let dl = echo_deadline_ns(&e, born);
                let mut probes = vec![0u64, 1, 2_999_999_999, 3_000_000_000, 10_000_000_000];
                if let Some(o) = req_off {
                    let edge = o + ECHO_LAPSE.as_nanos() as u64;
                    probes.extend([o.saturating_sub(5), o, edge - 1, edge, edge + 1]);
                }
                for q in probes {
                    let now = born + ns(q);
                    assert_eq!(before_deadline(dl, now, born), e.reflecting(now), "enabled {enabled} req {req_off:?} now +{q}ns");
                }
            }
        }
    }

    /// 엔진 ③c: 원격 손실 보고 셀 — 없음 → 쓴 값·시각 그대로, 덮어쓰기.
    #[test]
    fn remote_loss_cell_roundtrip() {
        let p = test_peer();
        assert_eq!(p.remote_loss(), None);
        let at = p.born + Duration::from_millis(1234);
        p.set_remote_loss(LossReport { loss_pct: 2.5, conceal_pct: 1.25 }, at);
        assert_eq!(p.remote_loss(), Some((LossReport { loss_pct: 2.5, conceal_pct: 1.25 }, at)));
        let at2 = at + Duration::from_nanos(7);
        p.set_remote_loss(LossReport { loss_pct: 0.0, conceal_pct: 9.0 }, at2);
        assert_eq!(p.remote_loss(), Some((LossReport { loss_pct: 0.0, conceal_pct: 9.0 }, at2)));
    }

    #[test]
    fn hello_targets_exclude_relay() {
        let keys = KeyPair::generate();
        let other = KeyPair::generate();
        let desc = LocalDescriptor {
            ssrc: 9,
            key: LocalDescriptor::encode_key(&other.public_bytes()),
            candidates: vec![
                Candidate { kind: CandidateKind::Lan, addr: "192.168.0.5:4000".into() },
                Candidate { kind: CandidateKind::Public, addr: RELAY.into() },
            ],
        };
        let setup = PeerSetup { keys: &keys, my_ssrc: 1, allow_loopback: false };
        let (media, _rx) = band_dsp::media_link::media_link(PeerKey::from_raw(1));
        let p = Peer::new("p".into(), &desc, &setup, media, Instant::now()).unwrap();
        p.conn.lock().on_handshake(RELAY.parse().unwrap(), false, &p.candidates, relay(), Instant::now());
        assert_eq!(p.hello_targets(relay()), vec!["192.168.0.5:4000".parse::<SocketAddr>().unwrap()]);
    }

    #[test]
    fn rtt_ewma() {
        let mut c = ConnState::new(Instant::now());
        c.on_rtt_sample(10.0);
        assert_eq!(c.rtt_ms, Some(10.0));
        c.on_rtt_sample(18.0);
        assert_eq!(c.rtt_ms, Some(11.0));
        c.on_rtt_sample(-5.0);
        assert_eq!(c.rtt_ms, Some(11.0));
    }

    #[test]
    fn descriptor_roundtrip() {
        let key = [9u8; 32];
        let d = LocalDescriptor {
            ssrc: 42,
            key: LocalDescriptor::encode_key(&key),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: "10.0.0.2:5555".into() }],
        };
        let j = serde_json::to_string(&d).unwrap();
        assert!(j.contains("\"kind\":\"lan\""));
        let back: LocalDescriptor = serde_json::from_str(&j).unwrap();
        assert_eq!(back.decode_key().unwrap(), key);
        assert!(LocalDescriptor { key: "AAAA".into(), ..d }.decode_key().is_err());
    }

    #[test]
    fn candidate_validation() {
        let mk = |addrs: &[&str]| LocalDescriptor {
            ssrc: 1,
            key: LocalDescriptor::encode_key(&[1u8; 32]),
            candidates: addrs.iter().map(|a| Candidate { kind: CandidateKind::Public, addr: a.to_string() }).collect(),
        };
        assert!(mk(&["203.0.113.1:5000"]).validated_candidates(false).is_ok());
        for bad in ["0.0.0.0:5000", "224.0.0.1:5000", "255.255.255.255:5000", "203.0.113.1:0", "[::1]:5000", "nope"] {
            assert!(mk(&[bad]).validated_candidates(false).is_err(), "{bad}");
        }
        assert!(mk(&["127.0.0.1:5000"]).validated_candidates(false).is_err());
        assert!(mk(&["127.0.0.1:5000"]).validated_candidates(true).is_ok());
        let many: Vec<String> = (0..9).map(|i| format!("203.0.113.{i}:5000")).collect();
        let refs: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        assert!(mk(&refs).validated_candidates(false).is_err(), "후보 9개는 거부");
        // 중복 제거
        assert_eq!(mk(&["203.0.113.1:5000", "203.0.113.1:5000"]).validated_candidates(false).unwrap().len(), 1);
    }

    #[test]
    fn loss_report_roundtrip_and_old_engine_compat() {
        let now = Instant::now();
        let rep = LossReport { loss_pct: 2.37, conceal_pct: 2.5 };
        let mut buf = [0u8; PING_PLAIN_MAX];
        // 손실 보고만(에코 없음): 옛 엔진의 on_ping은 빈 평문(flags 0)과 같게 처리한다
        let n = ping_plain(None, Some(rep), &mut buf);
        assert_eq!(n, LOSS_REPORT_BYTES);
        assert_eq!(LossReport::parse_ping(&buf[..n]), Some(rep));
        let mut e = EchoState { enabled: true, remote_request_at: Some(now), ..Default::default() };
        e.on_ping(&buf[..n], now);
        assert!(!e.remote_requesting(now) && !e.enabled, "에코 플래그 0으로 해석");
        // 에코 블록 + 손실 보고: 에코는 그대로(옛 엔진은 뒤 바이트 무시), 손실 보고도 읽힌다
        let echo = [ECHO_TAG, ECHO_FLAG_REQUEST, 0, 0, 0, 7];
        let n = ping_plain(Some(echo), Some(rep), &mut buf);
        assert_eq!(n, ECHO_PAYLOAD_BYTES + LOSS_REPORT_BYTES);
        let mut e = EchoState::default();
        e.on_ping(&buf[..n], now);
        assert!(e.remote_requesting(now));
        assert_eq!(LossReport::parse_ping(&buf[..n]), Some(rep));
        // 옛 엔진 Ping(빈 평문·에코만)에는 보고가 없다
        assert_eq!(LossReport::parse_ping(&[]), None);
        assert_eq!(LossReport::parse_ping(&echo), None);
        assert_eq!(ping_plain(None, None, &mut buf), 0);
        // 잘린 블록·모르는 태그는 무시, 뒤에 붙는 확장 바이트는 허용
        assert_eq!(LossReport::parse_ping(&[LOSS_TAG, 0, 1, 0]), None);
        assert_eq!(LossReport::parse_ping(&[0x55, 0, 1, 0, 1]), None);
        assert!(LossReport::parse_ping(&[LOSS_TAG, 0, 100, 0, 100, 0xAA, 0xBB]).is_some_and(|r| r.loss_pct == 1.0));
        // 범위 밖 값: 보낼 때 0..100%로 자르고, 받을 때도 100%를 넘지 않는다
        let wild = LossReport { loss_pct: f32::NAN, conceal_pct: 400.0 }.encode();
        assert_eq!(LossReport::parse_ping(&wild), Some(LossReport { loss_pct: 0.0, conceal_pct: 100.0 }));
        assert_eq!(LossReport::parse_ping(&[LOSS_TAG, 0xFF, 0xFF, 0, 0]).unwrap().loss_pct, 100.0);
    }

    /// P2-8b: 입력 버퍼 블록. (1) 블록 없는 옛 Ping → None, (2) 옛 파서(에코·손실)는 새 Ping을 그대로 읽는다,
    /// (3) u16 µs 포화, 모르는 태그·잘린 블록 무시.
    #[test]
    fn input_buffer_block_roundtrip_and_old_engine_compat() {
        let now = Instant::now();
        let rep = LossReport { loss_pct: 2.37, conceal_pct: 2.5 };
        let echo = [ECHO_TAG, ECHO_FLAG_REQUEST, 0, 0, 0, 7];
        let mut buf = [0u8; PING_PLAIN_MAX];
        // (1) 옛 엔진 Ping: 빈 평문·에코만·손실만·에코+손실 — 입력 버퍼 없음
        assert_eq!(parse_ping_input_buffer(&[]), None);
        assert_eq!(parse_ping_input_buffer(&echo), None);
        let n = ping_plain(Some(echo), Some(rep), &mut buf);
        assert_eq!(parse_ping_input_buffer(&buf[..n]), None);
        let n = ping_plain(None, Some(rep), &mut buf);
        assert_eq!(parse_ping_input_buffer(&buf[..n]), None);
        // 모든 조합에서 새 블록이 읽히고, 옛 파서(에코 플래그·손실 보고)는 같은 값을 본다
        for (e, l) in [(None, None), (Some(echo), None), (None, Some(rep)), (Some(echo), Some(rep))] {
            let n0 = ping_plain(e, l, &mut buf);
            let n = append_input_buffer(&mut buf, n0, 2667);
            assert_eq!(n, n0 + INBUF_BYTES);
            assert!(n <= PING_PLAIN_MAX);
            assert_eq!(parse_ping_input_buffer(&buf[..n]), Some(2667), "{e:?} {l:?}");
            // (2) 옛 손실 파서: 블록 뒤 바이트 무시, 손실 블록 없으면 None
            assert_eq!(LossReport::parse_ping(&buf[..n]), l);
            // (2) 옛 에코 파서: 에코 블록이 없으면 첫 바이트가 INBUF_TAG라 빈 평문(flags 0)과 같다
            let mut st = EchoState { enabled: true, remote_request_at: Some(now), ..Default::default() };
            st.on_ping(&buf[..n], now);
            assert_eq!(st.remote_requesting(now), e.is_some(), "{e:?} {l:?}");
        }
        // 0 = 모름 → 싣지 않는다
        let n0 = ping_plain(None, Some(rep), &mut buf);
        assert_eq!(append_input_buffer(&mut buf, n0, 0), n0);
        // (3) 포화: 65535µs 넘으면 65535, 받을 때 0은 None
        let n = append_input_buffer(&mut buf, 0, 200_000);
        assert_eq!(parse_ping_input_buffer(&buf[..n]), Some(u16::MAX));
        let n = append_input_buffer(&mut buf, 0, u32::MAX);
        assert_eq!(parse_ping_input_buffer(&buf[..n]), Some(u16::MAX));
        assert_eq!(parse_ping_input_buffer(&[INBUF_TAG, 0, 0]), None);
        // 잘린 블록·모르는 태그·확장 바이트
        assert_eq!(parse_ping_input_buffer(&[INBUF_TAG, 1]), None);
        assert_eq!(parse_ping_input_buffer(&[0x55, 1, 2]), None);
        assert_eq!(parse_ping_input_buffer(&[INBUF_TAG, 0x0A, 0x6B, 0xAA, 0xBB]), Some(2667));
        // 잘린 손실 블록 뒤라면 손실 블록으로 보지 않고 태그를 그대로 본다(INBUF가 아니므로 None)
        assert_eq!(parse_ping_input_buffer(&[LOSS_TAG, 0, 1]), None);
        // 가득 찬 평문에는 더 붙이지 않는다
        let n = ping_plain(Some(echo), Some(rep), &mut buf);
        let n = append_input_buffer(&mut buf, n, 1000);
        assert_eq!(n, PING_PLAIN_MAX);
        assert_eq!(append_input_buffer(&mut buf, n, 1000), n);
        // 태그가 다른 블록·예약 태그와 겹치지 않는다(에코 0xEC, 손실 0x4C, Pong 시계 0xC1)
        assert!(![ECHO_TAG, LOSS_TAG, 0xC1].contains(&INBUF_TAG));
    }

    #[test]
    fn remote_input_buffer_and_frame_code_cells() {
        let p = test_peer();
        let born = p.born;
        let ns = p.net_stats(born);
        assert_eq!((ns.remote_in_buf, ns.rx_frame_code), (None, 0));
        let at = born + Duration::from_millis(1234);
        p.set_remote_input_buffer(2667, at);
        assert_eq!(p.remote_input_buffer(), Some((2667, at)));
        p.set_remote_input_buffer(u16::MAX, born);
        assert_eq!(p.remote_input_buffer(), Some((u16::MAX, born)));
    }
}
