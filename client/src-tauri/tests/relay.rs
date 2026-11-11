//! 릴레이 폴백 통합 테스트(P1-4b, relay-design.md).
//!
//! 실제 `band-relay` 크레이트를 같은 프로세스의 tokio 런타임으로 로컬 UDP에 띄운다. 모든 경로는 프록시
//! (`GateProxy`)를 거친다: 방향별로 열고 닫을 수 있고(닫히면 폐기 = 대칭형 NAT 흉내) 지연을 넣을 수 있다.
//! - 직접 경로: A ↔ 프록시 ↔ B
//! - 릴레이 경로: A ↔ 링크 프록시 ↔ 릴레이 ↔ 링크 프록시 ↔ B (엔진이 아는 릴레이 주소 = 링크 프록시)
//!
//! - 직접 경로 차단 → 릴레이로 연결되고 오디오가 흐른다(연결 시간, 버퍼 지연, 은닉률, 연속성).
//! - 직접 경로가 나중에 열리면 직접(LAN)으로 승격되고 그 사이 소리가 끊기지 않는다(경로 지연 차이 있음).
//! - 직접으로 연결된 뒤 차단 → 릴레이로 강등(무음 길이).
//! - 단방향 차단 → 양쪽 모두 릴레이에 안정적으로 머문다.
//! - 직접 경로 플래핑(1초 열림/2초 닫힘) → 히스테리시스로 경로가 흔들리지 않는다.
//! - 릴레이 재시작 → 쿠키 절차로 재등록하고 소리가 돌아온다.
//! - 릴레이 주소 변경 → 릴레이 경로 피어가 새 주소로 바로 옮긴다.
//! - 토큰 만료 전 갱신 → 같은 주소 갱신으로 등록이 이어진다. 만료(받은 시각 + 남은 수명) → Expired.
//! - 릴레이 주소가 아닌 곳에서 온 RelayBindAck(type 9)는 무시한다.
//! - 릴레이는 Bind MAC을 요구한다(2.1.1). 엔진은 서버가 준 bindKey로 MAC을 붙여 등록한다.
//! - 직접 Hello의 출발 주소를 릴레이로 위장해도 경로 태그(AAD)로 걸러 릴레이 경로가 서지 않는다(4.1).
//!
//! ## 두 단계(P3-0a, tests/loopback.rs와 같은 규칙)
//! - **결정적**(기본 `cargo test`): `relay_fallback_then_promotion_flow`(폴백·승격·강등 흐름),
//!   `relay_ack_from_other_address_is_ignored`. 대기는 상태 조건·받은 패킷 수, 벽시계는 행 감지 상한뿐이다.
//! - **실시간**(`#[ignore = "realtime: ..."]`): 연결·승격·강등·재등록 시간, 은닉률·연속성, 토큰 수명(초 단위 벽시계)을
//!   재는 나머지. CI `realtime` 잡이 하나씩 3회까지 돌린다. 경로·등록 상태 기계는 ignore 사유의 net::peer /
//!   net::relay 단위 테스트(가상 시간 주입)가 결정적 짝이다. 로컬: `cargo test --test relay -- --ignored --test-threads 1`.

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::stats::{PathKind, PeerStateKind};
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::relay::RelayStateKind;
use band_lib::net::{NetOptions, RelaySetting};
use band_relay::token::{self, Claims};
use band_relay::{server, Limits, Relay, Stats};
use base64::Engine as _;
use parking_lot::Mutex;

static RT_LOCK: Mutex<()> = Mutex::new(());

const SECRET: &[u8] = b"engine-relay-integration-secret-0123456789";
const ROOM: &str = "room-relay-test";

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn sine_backend() -> FakeBackend {
    FakeBackend::new(
        48_000,
        Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as u64) as f64 / 48_000.0;
                frame.fill((t * 440.0 * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5);
            }
        }),
    )
}

fn test_net() -> NetOptions {
    NetOptions {
        port: 0,
        stun_servers: vec![],
        gather_lan: false,
        extra_candidates: vec![],
        allow_loopback_candidates: true,
        cancel: None,
    }
}

fn config() -> EngineConfig {
    EngineConfig { buffer_size: 128, frame_ms: 5.0, codec: CodecKind::Opus, channels: 1, input_channels: vec![0], ..Default::default() }
}

fn local_addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn relay_token(e: &Engine, member: &str, ttl_secs: u64) -> String {
    let ssrc = e.descriptor().unwrap().ssrc;
    let t = token::sign(SECRET, &Claims { expires_at: unix_now() + ttl_secs, ssrc, room: ROOM, client: member });
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(t)
}

/// 프론트가 하듯 `engine_set_relay({ addr, token, expiresInSec })`. host 문자열은 주소 그대로.
fn set_relay(e: &Engine, addr: SocketAddr, token: &str, expires_in: Option<u64>) -> Result<(), String> {
    let host = addr.to_string();
    let key = bind_key_for(token);
    e.set_relay(Some(RelaySetting { host: &host, addr, token, expires_in: expires_in.map(Duration::from_secs), bind_key: Some(&key) }))
}

/// 시그널링 서버가 `relay-token.bindKey`로 내려주는 토큰별 Bind MAC 키(relay-design 2.1.1)
fn bind_key_for(token_b64: &str) -> String {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token_b64).unwrap();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token::bind_key(SECRET, &raw))
}

// ---------------------------------------------------------------------------- 릴레이 서버

struct RelayServer {
    addr: SocketAddr,
    rt: tokio::runtime::Runtime,
    task: Option<tokio::task::JoinHandle<()>>,
    stats: Arc<Stats>,
}

impl RelayServer {
    fn start() -> RelayServer {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let mut s = RelayServer { addr: "127.0.0.1:0".parse().unwrap(), rt, task: None, stats: Arc::new(Stats::default()) };
        s.spawn();
        s
    }

    fn spawn(&mut self) {
        let addr = self.addr;
        let sock = self.rt.block_on(tokio::net::UdpSocket::bind(addr)).expect("릴레이 바인드");
        self.addr = sock.local_addr().unwrap();
        self.stats = Arc::new(Stats::default());
        // Bind MAC 필수(relay-design 2.1.1): 엔진이 서버가 준 bindKey로 MAC을 붙이는지 모든 테스트에서 확인한다
        let relay = Relay::new(SECRET.to_vec(), Limits { require_bind_mac: true, ..Limits::default() }, self.stats.clone());
        self.task = Some(self.rt.spawn(server::run_udp(sock, relay)));
    }

    /// 같은 포트로 새 릴레이를 띄운다(등록 상태는 모두 사라진다 — 프로세스 재시작과 같다).
    fn restart(&mut self, down_for: Duration) {
        self.stop();
        std::thread::sleep(down_for);
        self.spawn();
    }

    fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
            let _ = self.rt.block_on(t);
        }
    }
}

impl Drop for RelayServer {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------- 프록시

/// a ↔ pa ... pb ↔ b. a가 pa로 보낸 패킷은 pb 소켓에서 b로, b가 pb로 보낸 패킷은 pa 소켓에서 a로 나간다.
/// `ab`/`ba`로 방향별 개방, `delay`로 방향마다 같은 편도 지연을 넣는다.
struct GateProxy {
    pa: SocketAddr,
    pb: SocketAddr,
    ab: Arc<AtomicBool>,
    ba: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    forwarded: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl GateProxy {
    fn start(a: SocketAddr, b: SocketAddr, open: bool, delay: Duration) -> GateProxy {
        let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
        // 비블로킹 + 짧은 sleep(Windows에서 1ms 읽기 타임아웃은 ~15ms로 늘어나 지터가 된다)
        sa.set_nonblocking(true).unwrap();
        sb.set_nonblocking(true).unwrap();
        let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
        let ab = Arc::new(AtomicBool::new(open));
        let ba = Arc::new(AtomicBool::new(open));
        let stop = Arc::new(AtomicBool::new(false));
        let forwarded = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (gab, gba, st, fw, dr) = (ab.clone(), ba.clone(), stop.clone(), forwarded.clone(), dropped.clone());
        let thread = std::thread::spawn(move || {
            let _prio = band_lib::engine::prio::promote_current_thread();
            let mut buf = [0u8; 2048];
            // (보낼 시각, 내용, true면 sb에서 b로 / false면 sa에서 a로)
            let mut queue: VecDeque<(Instant, Vec<u8>, bool)> = VecDeque::new();
            while !st.load(Ordering::Relaxed) {
                for (sock, to_b, gate) in [(&sa, true, &gab), (&sb, false, &gba)] {
                    while let Ok((n, _)) = sock.recv_from(&mut buf) {
                        if gate.load(Ordering::Relaxed) {
                            queue.push_back((Instant::now() + delay, buf[..n].to_vec(), to_b));
                        } else {
                            dr.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                let now = Instant::now();
                while queue.front().is_some_and(|(t, _, _)| *t <= now) {
                    let (_, pkt, to_b) = queue.pop_front().unwrap();
                    let _ = if to_b { sb.send_to(&pkt, b) } else { sa.send_to(&pkt, a) };
                    fw.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        GateProxy { pa, pb, ab, ba, stop, forwarded, dropped, thread: Some(thread) }
    }

    fn set_open(&self, open: bool) {
        self.ab.store(open, Ordering::Relaxed);
        self.ba.store(open, Ordering::Relaxed);
    }
}

impl Drop for GateProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------- 엔진 쌍

#[derive(Clone, Copy)]
struct PairOpts {
    /// 직접 경로가 처음부터 열려 있다.
    direct_open: bool,
    /// 직접 경로 편도 지연.
    direct_delay: Duration,
    /// 엔진 ↔ 릴레이 링크 편도 지연(릴레이 경로 편도 = 두 링크의 합).
    relay_link_delay: Duration,
    ttl_secs: u64,
}

impl Default for PairOpts {
    fn default() -> Self {
        Self { direct_open: false, direct_delay: Duration::ZERO, relay_link_delay: Duration::ZERO, ttl_secs: 600 }
    }
}

struct Pair {
    a: Engine,
    b: Engine,
    proxy: GateProxy,
    /// 엔진이 아는 릴레이 주소 = 각 링크 프록시의 pa
    link_a: GateProxy,
    link_b: GateProxy,
    sink_b: Arc<Mutex<Vec<f32>>>,
    /// add_peer 시각
    t_added: Instant,
}

/// A(사인) → B. 직접 후보는 프록시, 릴레이는 링크 프록시를 거친 `relay`.
fn relay_pair(relay: &RelayServer, o: PairOpts) -> Pair {
    let backend_b = FakeBackend::silence(48_000);
    let sink_b = backend_b.sink.clone();
    let a = Engine::start(config(), Arc::new(sine_backend()), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(config(), Arc::new(backend_b), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let proxy = GateProxy::start(local_addr(&a), local_addr(&b), o.direct_open, o.direct_delay);
    let link_a = GateProxy::start(local_addr(&a), relay.addr, true, o.relay_link_delay);
    let link_b = GateProxy::start(local_addr(&b), relay.addr, true, o.relay_link_delay);
    let cand = |d: &LocalDescriptor, addr: SocketAddr| LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    };
    let t_added = Instant::now();
    a.add_peer("bob".into(), &cand(b.descriptor().unwrap(), proxy.pa)).unwrap();
    b.add_peer("alice".into(), &cand(a.descriptor().unwrap(), proxy.pb)).unwrap();
    // 서버의 relay-token은 native-descriptor 직후에 온다: add_peer와 거의 동시
    set_relay(&a, link_a.pa, &relay_token(&a, "user-alice", o.ttl_secs), Some(o.ttl_secs)).unwrap();
    set_relay(&b, link_b.pa, &relay_token(&b, "user-bob", o.ttl_secs), Some(o.ttl_secs)).unwrap();
    Pair { a, b, proxy, link_a, link_b, sink_b, t_added }
}

type PathState = Option<(PeerStateKind, Option<PathKind>)>;

fn peer_path(e: &Engine) -> PathState {
    e.stats().peers.first().map(|p| (p.state, p.path))
}

fn wait_path(p: &Pair, kind: PathKind, timeout: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        let ok = |e: &Engine| peer_path(e) == Some((PeerStateKind::Connected, Some(kind)));
        if ok(&p.a) && ok(&p.b) {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// B sink에서 (샘플 시작 인덱스) 이후 첫 소리(|x|>0.1) 위치까지의 시간.
fn wait_sound(sink: &Arc<Mutex<Vec<f32>>>, from: usize, timeout: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if sink.lock()[from..].iter().any(|x| x.abs() > 0.1) {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// `dur` 동안 10ms마다 양쪽 경로 상태를 보고 바뀐 횟수를 센다. `each(경과)`는 매 표본마다(게이트 조작용).
fn watch_paths(p: &Pair, dur: Duration, mut each: impl FnMut(Duration)) -> (usize, Vec<PathState>) {
    let t0 = Instant::now();
    let (mut last_a, mut last_b) = (peer_path(&p.a), peer_path(&p.b));
    let mut changes = 0;
    let mut seen = vec![last_a];
    while t0.elapsed() < dur {
        each(t0.elapsed());
        std::thread::sleep(Duration::from_millis(10));
        let (a, b) = (peer_path(&p.a), peer_path(&p.b));
        if a != last_a {
            changes += 1;
            seen.push(a);
        }
        if b != last_b {
            changes += 1;
        }
        (last_a, last_b) = (a, b);
    }
    (changes, seen)
}

#[derive(Debug, Default)]
struct Continuity {
    discontinuities: usize,
    silent_runs: usize,
    /// 5ms 창 중 소리 있는 비율
    sound_ratio: f64,
    /// 가장 긴 연속 무음(5ms 창 RMS < 0.05) 길이(ms)
    max_quiet_ms: f64,
}

/// 스테레오 인터리브 구간 검사: 불연속 = 인접 샘플 차 > 0.08, 무음 = 정확히 0인 샘플 120개 이상 연속.
fn analyze(seg: &[f32]) -> Continuity {
    let mut c = Continuity::default();
    let mut prev: Option<f32> = None;
    let mut zero_run = 0;
    for f in seg.as_chunks::<2>().0 {
        let x = f[0];
        if prev.is_some_and(|p| (x - p).abs() > 0.08) {
            c.discontinuities += 1;
        }
        prev = Some(x);
        if x == 0.0 {
            zero_run += 1;
            if zero_run == 120 {
                c.silent_runs += 1;
            }
        } else {
            zero_run = 0;
        }
    }
    let (mut windows, mut sound, mut quiet_run, mut max_quiet) = (0, 0, 0usize, 0usize);
    for w in seg.as_chunks::<480>().0 {
        windows += 1;
        let rms = (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt();
        if rms > 0.15 {
            sound += 1;
        }
        if rms < 0.05 {
            quiet_run += 1;
            max_quiet = max_quiet.max(quiet_run);
        } else {
            quiet_run = 0;
        }
    }
    c.sound_ratio = sound as f64 / windows.max(1) as f64;
    c.max_quiet_ms = max_quiet as f64 * 5.0;
    c
}

fn relay_state(e: &Engine) -> Option<RelayStateKind> {
    e.stats().relay.map(|r| r.state)
}

fn wait_relay_state(e: &Engine, want: RelayStateKind, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if relay_state(e) == Some(want) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn conceal_ratio(e: &Engine) -> (u64, u64) {
    let js = e.peer_jitter_stats()[0].1;
    (js.concealed, js.produced)
}

/// 결정적 테스트의 대기 상한. 판정은 상태 조건으로 하고, 벽시계는 행(hang) 감지에만 쓴다.
const HANG: Duration = Duration::from_secs(20);

// ============================================================================

/// 결정적(P3-0a): 직접 차단 → 릴레이로 연결·소리 → 직접 개방 → LAN 승격·소리 → 직접 차단 → 릴레이 강등·소리.
/// 경로 전환 흐름만 본다(연결·승격·강등 시간, 은닉·연속성 임계는 아래 실시간 테스트). 대기는 상태 조건이고
/// 벽시계는 행 감지 상한(`HANG`)뿐이라 러너 속도와 무관하다. 경로 상태 기계 자체는 net::peer 단위 테스트(가상 시간)가 본다.
#[test]
fn relay_fallback_then_promotion_flow() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(
        &relay,
        PairOpts { direct_delay: Duration::from_millis(2), relay_link_delay: Duration::from_millis(10), ..Default::default() },
    );
    assert!(wait_path(&p, PathKind::Relay, HANG).is_some(), "릴레이 경로로 연결되지 않음: A {:?} B {:?}", peer_path(&p.a), peer_path(&p.b));
    assert!(wait_sound(&p.sink_b, 0, HANG).is_some(), "릴레이 경유 소리 없음");
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Bound));
    assert!(p.proxy.dropped.load(Ordering::Relaxed) > 0, "직접 경로 Hello는 프록시에서 버려져야 한다");
    assert_eq!(p.proxy.forwarded.load(Ordering::Relaxed), 0);
    assert!(relay.stats.forwarded_packets.load(Ordering::Relaxed) > 0, "소리는 릴레이를 거쳐야 한다");

    // 직접 경로 개방 → LAN 승격
    p.proxy.set_open(true);
    assert!(wait_path(&p, PathKind::Lan, HANG).is_some(), "직접 경로로 승격되지 않음: A {:?} B {:?}", peer_path(&p.a), peer_path(&p.b));
    let from = p.sink_b.lock().len();
    assert!(wait_sound(&p.sink_b, from, HANG).is_some(), "승격 뒤 소리 없음");
    assert!(p.proxy.forwarded.load(Ordering::Relaxed) > 0);

    // 직접 경로 차단 → 릴레이 강등
    p.proxy.set_open(false);
    assert!(wait_path(&p, PathKind::Relay, HANG).is_some(), "릴레이로 강등되지 않음: A {:?} B {:?}", peer_path(&p.a), peer_path(&p.b));
    let from = p.sink_b.lock().len();
    assert!(wait_sound(&p.sink_b, from, HANG).is_some(), "강등 뒤 소리 없음");
}

/// 릴레이 경로 편도 20ms(링크 10ms × 2), 직접 경로 편도 2ms: 실망처럼 경로 지연이 다를 때의 전환.
#[test]
#[ignore = "realtime: 결정적 짝 net::peer::tests::{relay_hello_after_fallback_delay_only_when_ready, relay_path_established_and_direct_promoted_by_ack, promotion_requires_unbroken_acks_and_acceptable_rtt}, net::relay::tests::bind_cookie_ok_flow, relay_fallback_then_promotion_flow(흐름)"]
fn direct_blocked_connects_via_relay_then_promotes_to_direct() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(
        &relay,
        PairOpts { direct_delay: Duration::from_millis(2), relay_link_delay: Duration::from_millis(10), ..Default::default() },
    );

    let t_relay = wait_path(&p, PathKind::Relay, Duration::from_secs(8)).expect("릴레이 경로로 연결되지 않음");
    let connect_ms = p.t_added.elapsed().as_millis();
    let sound_after = wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("릴레이 경유 소리 없음");
    let first_sound_ms = p.t_added.elapsed().as_millis();
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Bound));
    assert!(p.proxy.dropped.load(Ordering::Relaxed) > 0, "직접 경로 Hello는 프록시에서 버려져야 한다");
    assert_eq!(p.proxy.forwarded.load(Ordering::Relaxed), 0);

    // 릴레이 경로로 6초 연주
    let start = p.sink_b.lock().len();
    std::thread::sleep(Duration::from_secs(6));
    let js_relay = p.b.peer_jitter_stats()[0].1;
    let stats_a = p.a.stats();
    let rtt_relay = stats_a.peers[0].rtt_ms;
    let relay_stats = stats_a.relay.clone().expect("릴레이 상태");
    let seg = p.sink_b.lock()[start + 48_000 * 2..].to_vec(); // 첫 1초(수렴) 제외
    let cont_relay = analyze(&seg);
    let xruns_b = p.b.stats().xruns;

    // 직접 경로 개방 → LAN으로 승격(연속 HelloAck 3회·2초 안정 뒤, 무끊김)
    let before_open = p.sink_b.lock().len();
    let (conc0, prod0) = conceal_ratio(&p.b);
    let t_open = Instant::now();
    p.proxy.set_open(true);
    let promote = wait_path(&p, PathKind::Lan, Duration::from_secs(8)).expect("직접 경로로 승격되지 않음");
    std::thread::sleep(Duration::from_secs(3));
    let seg = p.sink_b.lock()[before_open..].to_vec();
    let cont_switch = analyze(&seg);
    let js_end = p.b.peer_jitter_stats()[0].1;
    let (conc1, prod1) = conceal_ratio(&p.b);
    let rtt_direct = p.a.stats().peers[0].rtt_ms;
    let xruns_b2 = p.b.stats().xruns;
    let forwarded = relay.stats.forwarded_packets.load(Ordering::Relaxed);

    let conceal_relay = js_relay.concealed as f64 / js_relay.produced.max(1) as f64;
    let conceal_switch = (conc1 - conc0) as f64 / (prod1 - prod0).max(1) as f64;
    println!("=== 릴레이 폴백 (직접 차단, 릴레이 편도 20ms / 직접 편도 2ms, 로컬 UDP, debug 빌드) ===");
    println!("연결: add_peer → 양쪽 릴레이 Connected {connect_ms}ms (대기 {t_relay:?}), 첫 소리 {first_sound_ms}ms ({sound_after:?})");
    println!("릴레이 경로 B←A 지터버퍼: {js_relay:?}");
    println!(
        "릴레이 경로: 버퍼 {:.1}ms(목표 {:.1}ms, 벽시계 {:.1}ms), 은닉 {:.2}%, 연속성 {:.2}%, 무음 {} 불연속 {}, xrun {xruns_b}",
        js_relay.buffer_ms,
        js_relay.target_ms,
        js_relay.wall_buffer_ms,
        conceal_relay * 100.0,
        cont_relay.sound_ratio * 100.0,
        cont_relay.silent_runs,
        cont_relay.discontinuities
    );
    println!(
        "종단 RTT 릴레이 {rtt_relay:?}ms → 직접 {rtt_direct:?}ms, A↔릴레이 RTT {:?}ms, A 릴레이 송신 {:.0}kbps(초과 {}), 릴레이 전달 {forwarded}패킷",
        relay_stats.rtt_ms, relay_stats.tx_kbps, relay_stats.over_budget
    );
    println!(
        "승격: 개방 후 {promote:?}(총 {:?}), 전환 구간 연속성 {:.2}%, 은닉 {:.2}%, 최장 무음 {:.0}ms, 무음 {} 불연속 {}, xrun {}, 끝 버퍼 {:.1}ms(벽시계 {:.1}ms)",
        t_open.elapsed(),
        cont_switch.sound_ratio * 100.0,
        conceal_switch * 100.0,
        cont_switch.max_quiet_ms,
        cont_switch.silent_runs,
        cont_switch.discontinuities,
        xruns_b2 - xruns_b,
        js_end.buffer_ms,
        js_end.wall_buffer_ms
    );

    // GOAL 3절: 입장 후 3초 이내 소리 (폴백 1.5초 + 릴레이 왕복 + 지터 버퍼)
    assert!(first_sound_ms < 3000, "첫 소리까지 {first_sound_ms}ms");
    assert!(cont_relay.sound_ratio > 0.97, "릴레이 경로 연속성 {:?}", cont_relay);
    assert!(cont_relay.silent_runs as u64 <= 3 * xruns_b, "릴레이 경로 무음 {:?} (xrun {xruns_b})", cont_relay);
    assert!(conceal_relay < 0.02, "릴레이 경로 은닉 {conceal_relay}");
    assert!(js_relay.buffer_ms < 30.0, "릴레이 경로 버퍼 {}", js_relay.buffer_ms);
    assert!(rtt_relay.is_some_and(|r| r >= 40.0), "릴레이 경유 RTT는 링크 지연(왕복 40ms) 이상: {rtt_relay:?}");
    assert!(rtt_direct.is_some_and(|r| r < 20.0), "승격 후 RTT는 직접 경로 값: {rtt_direct:?}");
    assert!(relay_stats.rtt_ms.is_some());
    assert!(relay_stats.tx_kbps > 50.0 && !relay_stats.over_budget, "Opus 1피어는 상한에 한참 못 미침: {relay_stats:?}");
    // 승격 조건: 연속 HelloAck 3회(1초 간격) → 2초 이상
    assert!(promote >= Duration::from_secs(2) && promote < Duration::from_secs(5), "승격 시간 {promote:?}");
    assert!(js_end.buffer_ms < 30.0, "승격 후 버퍼 지연 {}", js_end.buffer_ms);
    // 경로 전환은 지터 버퍼를 유지하므로 끊김이 없어야 한다(직접이 18ms 빨라 버퍼가 잠시 늘 뿐)
    assert!(cont_switch.max_quiet_ms < 50.0, "전환 구간 무음 {:?}", cont_switch);
    assert!(cont_switch.sound_ratio > 0.97, "전환 구간 연속성 {:?}", cont_switch);
    assert!(conceal_switch < 0.02, "전환 구간 은닉 {conceal_switch}");
    assert!(p.proxy.forwarded.load(Ordering::Relaxed) > 0);
}

/// 직접으로 연결된 뒤 직접 경로가 막히면 릴레이로 강등된다. 무음은 감지(1.5초) + 릴레이 왕복 정도.
#[test]
#[ignore = "realtime: 결정적 짝 net::peer::tests::falls_back_to_relay_when_direct_goes_quiet, relay_fallback_then_promotion_flow(흐름)"]
fn direct_blocked_after_connect_demotes_to_relay() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(
        &relay,
        PairOpts { direct_open: true, direct_delay: Duration::from_millis(2), relay_link_delay: Duration::from_millis(10), ..Default::default() },
    );
    wait_path(&p, PathKind::Lan, Duration::from_secs(3)).expect("직접 경로로 연결되지 않음");
    assert!(wait_relay_state(&p.a, RelayStateKind::Bound, Duration::from_secs(2)) && wait_relay_state(&p.b, RelayStateKind::Bound, Duration::from_secs(2)));
    wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("소리 없음");
    std::thread::sleep(Duration::from_secs(2));

    let before = p.sink_b.lock().len();
    let (conc0, prod0) = conceal_ratio(&p.b);
    let t_block = Instant::now();
    p.proxy.set_open(false);
    let demote = wait_path(&p, PathKind::Relay, Duration::from_secs(6)).expect("릴레이로 강등되지 않음");
    std::thread::sleep(Duration::from_secs(3));
    let cont = analyze(&p.sink_b.lock()[before..]);
    let (conc1, prod1) = conceal_ratio(&p.b);
    let hold = p.a.stats().peers[0].path;
    println!(
        "=== 강등 (직접 → 릴레이) === 차단 후 {demote:?}, 최장 무음 {:.0}ms, 연속성 {:.2}%, 은닉 {:.2}%, 이후 경로 {hold:?} ({:?})",
        cont.max_quiet_ms,
        cont.sound_ratio * 100.0,
        (conc1 - conc0) as f64 / (prod1 - prod0).max(1) as f64 * 100.0,
        t_block.elapsed()
    );
    assert!(demote < Duration::from_millis(2500), "강등이 느림: {demote:?}");
    assert!(cont.max_quiet_ms < 2000.0, "강등 무음 {:?}", cont);
    assert_eq!(peer_path(&p.a), Some((PeerStateKind::Connected, Some(PathKind::Relay))));
}

/// 한쪽만 대칭형 NAT(B→A 직접 차단, A→B는 통과): 직접 Hello는 닿지만 HelloAck가 못 돌아오므로 양쪽 모두
/// 릴레이에 안정적으로 머문다.
#[test]
#[ignore = "realtime: 결정적 짝 net::peer::tests::promotion_requires_unbroken_acks_and_acceptable_rtt(HelloAck 없으면 승격 없음)"]
fn one_way_blocked_direct_stays_on_relay() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(&relay, PairOpts::default());
    p.proxy.ab.store(true, Ordering::Relaxed); // A → B만 열림
    wait_path(&p, PathKind::Relay, Duration::from_secs(8)).expect("릴레이 경로로 연결되지 않음");
    wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("소리 없음");
    let start = p.sink_b.lock().len();
    let (changes, seen) = watch_paths(&p, Duration::from_secs(8), |_| {});
    let cont = analyze(&p.sink_b.lock()[start..]);
    println!(
        "=== 단방향 차단 (A→B만 통과) === 8초간 경로 변화 {changes} {seen:?}, 연속성 {:.2}%, 최장 무음 {:.0}ms, 프록시 전달 {} 폐기 {}",
        cont.sound_ratio * 100.0,
        cont.max_quiet_ms,
        p.proxy.forwarded.load(Ordering::Relaxed),
        p.proxy.dropped.load(Ordering::Relaxed)
    );
    assert!(p.proxy.forwarded.load(Ordering::Relaxed) > 0, "A→B 직접 Hello는 통과해야 한다");
    assert_eq!(changes, 0, "경로가 흔들리면 안 된다: {seen:?}");
    assert_eq!(peer_path(&p.a), Some((PeerStateKind::Connected, Some(PathKind::Relay))));
    assert_eq!(peer_path(&p.b), Some((PeerStateKind::Connected, Some(PathKind::Relay))));
    assert!(cont.sound_ratio > 0.97 && cont.max_quiet_ms < 50.0, "{cont:?}");
}

/// 직접 경로가 1초 열림/2초 닫힘을 반복(순단하는 Wi-Fi/LTE): 승격 조건(2초 안정)을 못 채우므로 릴레이에 머물고
/// 소리가 끊기지 않는다.
#[test]
#[ignore = "realtime: 결정적 짝 net::peer::tests::demotion_holds_promotion_and_backs_off_on_flapping"]
fn flapping_direct_path_does_not_flap_engine_path() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(&relay, PairOpts { direct_delay: Duration::from_millis(2), relay_link_delay: Duration::from_millis(10), ..Default::default() });
    wait_path(&p, PathKind::Relay, Duration::from_secs(8)).expect("릴레이 경로로 연결되지 않음");
    wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("소리 없음");
    std::thread::sleep(Duration::from_secs(1));
    let start = p.sink_b.lock().len();
    let (conc0, prod0) = conceal_ratio(&p.b);
    let cycles = 8;
    let (changes, seen) = watch_paths(&p, Duration::from_secs(3 * cycles), |t| {
        p.proxy.set_open(t.as_millis() % 3000 < 1000);
    });
    p.proxy.set_open(false);
    let cont = analyze(&p.sink_b.lock()[start..]);
    let (conc1, prod1) = conceal_ratio(&p.b);
    let conceal = (conc1 - conc0) as f64 / (prod1 - prod0).max(1) as f64;
    println!(
        "=== 플래핑 (1초 열림/2초 닫힘 × {cycles}) === 경로 변화 {changes} {seen:?}, 연속성 {:.2}%, 은닉 {:.2}%, 최장 무음 {:.0}ms, 직접 전달 {}",
        cont.sound_ratio * 100.0,
        conceal * 100.0,
        cont.max_quiet_ms,
        p.proxy.forwarded.load(Ordering::Relaxed)
    );
    assert!(p.proxy.forwarded.load(Ordering::Relaxed) > 0, "열린 동안 직접 Hello가 오갔어야 한다");
    assert!(changes <= 2, "경로가 플래핑함: {changes} {seen:?}");
    assert!(cont.sound_ratio > 0.97 && cont.max_quiet_ms < 100.0, "{cont:?}");
    assert!(conceal < 0.02, "은닉 {conceal}");
}

#[test]
#[ignore = "realtime: 결정적 짝 net::relay::tests::{bind_cookie_ok_flow, cookie_resend_rate_limited_for_stale_or_duplicate_requests}"]
fn relay_restart_reregisters_and_audio_resumes() {
    let _g = RT_LOCK.lock();
    let mut relay = RelayServer::start();
    let p = relay_pair(&relay, PairOpts::default());
    wait_path(&p, PathKind::Relay, Duration::from_secs(8)).expect("릴레이 경로로 연결되지 않음");
    wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("소리 없음");
    std::thread::sleep(Duration::from_secs(1));

    let t_down = Instant::now();
    relay.restart(Duration::from_millis(500));
    let restarted_at = p.sink_b.lock().len();
    // 재등록: 릴레이 경로 피어가 1초 조용하면 keepalive를 당겨 보내고 CookieRequired → 쿠키 Bind → Ok
    let t0 = Instant::now();
    let mut resumed = None;
    while t0.elapsed() < Duration::from_secs(8) {
        let tail_start = p.sink_b.lock().len().saturating_sub(240 * 2);
        if tail_start > restarted_at && analyze(&p.sink_b.lock()[tail_start..]).sound_ratio > 0.99 && relay.stats.forwarded_packets.load(Ordering::Relaxed) > 100 {
            resumed = Some(t_down.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let resumed = resumed.expect("릴레이 재시작 후 소리가 돌아오지 않음");
    std::thread::sleep(Duration::from_secs(2));
    let (state_a, state_b) = (peer_path(&p.a), peer_path(&p.b));
    let cookies = relay.stats.cookie_sent.load(Ordering::Relaxed);
    let binds = relay.stats.bind_ok.load(Ordering::Relaxed);
    println!(
        "=== 릴레이 재시작 === 중단 500ms, 정지→소리 복귀 {resumed:?}, 새 릴레이 쿠키 {cookies} Bind Ok {binds}, A {state_a:?} B {state_b:?}"
    );
    assert!(cookies >= 2 && binds >= 2, "양쪽 모두 쿠키 절차로 재등록해야 한다");
    assert_eq!(state_a, Some((PeerStateKind::Connected, Some(PathKind::Relay))));
    assert_eq!(state_b, Some((PeerStateKind::Connected, Some(PathKind::Relay))));
    assert!(resumed < Duration::from_secs(4), "재등록이 느림: {resumed:?}");
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Bound));
}

/// 릴레이 주소가 바뀌면(DNS 변경·릴레이 이전) 릴레이 경로 피어를 새 주소로 바로 옮긴다(Reconnecting 5초 없이, M3).
#[test]
#[ignore = "realtime: 결정적 짝 net::peer::tests::relay_address_change_moves_relay_path"]
fn relay_address_change_moves_peers_without_reconnecting() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let relay2 = RelayServer::start();
    let p = relay_pair(&relay, PairOpts::default());
    wait_path(&p, PathKind::Relay, Duration::from_secs(8)).expect("릴레이 경로로 연결되지 않음");
    wait_sound(&p.sink_b, 0, Duration::from_secs(3)).expect("소리 없음");
    std::thread::sleep(Duration::from_secs(1));
    // 같은 host 문자열이면 처음 해석한 주소를 그대로 쓴다(토큰 갱신마다 DNS를 다시 풀지 않음)
    assert_eq!(p.a.relay_addr_for_host(&p.link_a.pa.to_string()), Some(p.link_a.pa));
    assert_eq!(p.a.relay_addr_for_host("other.example:3478"), None);

    let before = p.sink_b.lock().len();
    let t_move = Instant::now();
    set_relay(&p.a, relay2.addr, &relay_token(&p.a, "user-alice", 600), Some(600)).unwrap();
    set_relay(&p.b, relay2.addr, &relay_token(&p.b, "user-bob", 600), Some(600)).unwrap();
    let (changes, seen) = watch_paths(&p, Duration::from_secs(3), |_| {});
    let cont = analyze(&p.sink_b.lock()[before..]);
    let relay_a = p.a.stats().relay.unwrap();
    println!(
        "=== 릴레이 주소 변경 === 경로 변화 {changes} {seen:?}, 최장 무음 {:.0}ms, 연속성 {:.2}%, 새 릴레이 전달 {}, A 릴레이 {relay_a:?} ({:?})",
        cont.max_quiet_ms,
        cont.sound_ratio * 100.0,
        relay2.stats.forwarded_packets.load(Ordering::Relaxed),
        t_move.elapsed()
    );
    assert_eq!(relay_a.addr, relay2.addr.to_string());
    assert_eq!(relay_a.state, RelayStateKind::Bound);
    assert_eq!(changes, 0, "Reconnecting을 거치지 않아야 한다: {seen:?}");
    assert!(relay2.stats.forwarded_packets.load(Ordering::Relaxed) > 100, "새 릴레이로 소리가 흘러야 한다");
    assert!(cont.max_quiet_ms < 300.0, "주소 변경 무음 {cont:?}");
}

#[test]
#[ignore = "realtime: 결정적 짝 net::relay::tests::{unreachable_rejected_expired_and_refresh, deadline_ignores_local_wall_clock_when_server_gives_expires_in}"]
fn token_refresh_before_expiry_keeps_relay_registration() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    // 수명 5초 토큰(만료 시각은 초 단위라 실제 4~5초)
    let p = relay_pair(&relay, PairOpts { ttl_secs: 5, ..Default::default() });
    wait_path(&p, PathKind::Relay, Duration::from_secs(4)).expect("릴레이 경로로 연결되지 않음");
    let ok_before = relay.stats.bind_ok.load(Ordering::Relaxed);
    // 프론트가 만료 전에 relay-token-request로 받은 새 토큰을 넘긴다
    set_relay(&p.a, p.link_a.pa, &relay_token(&p.a, "user-alice", 600), Some(600)).unwrap();
    set_relay(&p.b, p.link_b.pa, &relay_token(&p.b, "user-bob", 600), Some(600)).unwrap();
    // 원래 토큰 만료 시각을 충분히 넘긴다
    std::thread::sleep(Duration::from_secs(7));
    let start = p.sink_b.lock().len().saturating_sub(48_000 * 2 * 2);
    let cont = analyze(&p.sink_b.lock()[start..]);
    let ok_after = relay.stats.bind_ok.load(Ordering::Relaxed);
    let cookies = relay.stats.cookie_sent.load(Ordering::Relaxed);
    println!(
        "=== 토큰 갱신 === Bind Ok {ok_before}→{ok_after}, 쿠키 {cookies}, 최근 2초 연속성 {:.2}%, A {:?} 릴레이 {:?}",
        cont.sound_ratio * 100.0,
        peer_path(&p.a),
        p.a.stats().relay
    );
    assert!(ok_after >= ok_before + 2, "새 토큰으로 같은 주소 갱신(Ok)");
    assert_eq!(cookies, 2, "갱신에는 쿠키 왕복이 없어야 한다(처음 등록 2회만)");
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Bound));
    assert_eq!(peer_path(&p.b), Some((PeerStateKind::Connected, Some(PathKind::Relay))));
    assert!(cont.sound_ratio > 0.97, "원래 토큰 만료 후에도 소리가 이어져야 한다: {cont:?}");
}

#[test]
#[ignore = "realtime: 결정적 짝 net::relay::tests::{unreachable_rejected_expired_and_refresh, deadline_ignores_local_wall_clock_when_server_gives_expires_in}"]
fn expired_token_without_refresh_stops_binding() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(&relay, PairOpts { ttl_secs: 3, ..Default::default() });
    assert!(wait_relay_state(&p.a, RelayStateKind::Bound, Duration::from_secs(2)));
    // 만료 = 받은 시각 + 남은 수명(3초)
    assert!(wait_relay_state(&p.a, RelayStateKind::Expired, Duration::from_secs(5)), "만료되면 Bind를 멈추고 Expired로 보고");
    println!("=== 토큰 만료(갱신 없음) === A 릴레이 {:?}, 피어 {:?}", p.a.stats().relay, peer_path(&p.a));
    // 이미 만료된 토큰도 엔진은 로컬 시계로 거부하지 않는다(시계가 빠른 PC, M1) — 릴레이가 버리므로 unreachable
    let expired = {
        let ssrc = p.a.descriptor().unwrap().ssrc;
        let t = token::sign(SECRET, &Claims { expires_at: unix_now() - 1, ssrc, room: ROOM, client: "user-alice" });
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(t)
    };
    set_relay(&p.a, p.link_a.pa, &expired, None).unwrap();
    assert!(wait_relay_state(&p.a, RelayStateKind::Unreachable, Duration::from_secs(4)), "릴레이가 만료 토큰을 버림: {:?}", p.a.stats().relay);
    // 남의 ssrc 토큰은 받지 않는다
    assert!(set_relay(&p.a, p.link_a.pa, &relay_token(&p.b, "user-bob", 600), Some(600)).is_err());
}

/// 경로 종류 위장(relay-design 4.1): 경로상 공격자가 B의 직접 Hello를 잡아 출발 주소를 A의 릴레이 주소로 바꿔
/// 넣어도, Hello 헤더(AAD)의 경로 태그가 '직접'이라 A는 릴레이 경로를 세우지 않는다. 같은 Hello를 직접 주소에서
/// 보내면 받는다(대조군).
#[test]
fn hello_with_spoofed_relay_source_is_rejected() {
    let _g = RT_LOCK.lock();
    let start = |id: &str| Engine::start(config(), Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), Some((id.into(), test_net()))).unwrap();
    let (a, b) = (start("alice"), start("bob"));
    let fake_relay = UdpSocket::bind("127.0.0.1:0").unwrap(); // A가 아는 릴레이 주소(공격자가 출발지로 위장)
    let tap = UdpSocket::bind("127.0.0.1:0").unwrap(); // B가 A로 보내는 직접 Hello가 여기로 온다(경로상 관찰)
    let hole = UdpSocket::bind("127.0.0.1:0").unwrap(); // A가 아는 B 후보: 아무것도 돌려주지 않는다
    tap.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    set_relay(&a, fake_relay.local_addr().unwrap(), &relay_token(&a, "user-alice", 600), Some(600)).unwrap();
    let cand = |d: &LocalDescriptor, addr: SocketAddr| LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    };
    a.add_peer("bob".into(), &cand(b.descriptor().unwrap(), hole.local_addr().unwrap())).unwrap();
    b.add_peer("alice".into(), &cand(a.descriptor().unwrap(), tap.local_addr().unwrap())).unwrap();
    let mut buf = [0u8; 2048];
    let mut capture = || loop {
        let (n, _) = tap.recv_from(&mut buf).expect("B의 직접 Hello");
        if buf[3] == 2 {
            assert_eq!(buf[16], 0x80, "직접 Hello에는 '직접' 경로 태그");
            return buf[..n].to_vec();
        }
    };
    let mut hello = Vec::new();
    for _ in 0..5 {
        hello = capture();
        fake_relay.send_to(&hello, local_addr(&a)).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(peer_path(&a).and_then(|s| s.1), None, "위장한 릴레이 출발 Hello로 경로가 서면 안 된다: {:?}", peer_path(&a));
    // 위장본을 **먼저** 넣은 바로 그 Hello(같은 seq)를 직접 주소(tap)에서 늦게 넣어도 받아야 한다:
    // 태그 불일치 Hello가 제어 seq를 소모하면 진짜 Hello가 재전송으로 버려진다(PR #40 리뷰 M1).
    tap.send_to(&hello, local_addr(&a)).unwrap();
    let t0 = Instant::now();
    while peer_path(&a).and_then(|s| s.1).is_none() && t0.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let path = peer_path(&a).and_then(|s| s.1);
    assert!(matches!(path, Some(PathKind::Lan | PathKind::Public)), "직접 Hello는 받는다: {path:?}");
}

/// 릴레이 주소가 아닌 곳에서 온 RelayBindAck(type 9)는 무시한다(수신 주소 규칙의 NetShared 수준 회귀 테스트).
#[test]
fn relay_ack_from_other_address_is_ignored() {
    let _g = RT_LOCK.lock();
    let e = Engine::start(config(), Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), Some(("solo".into(), test_net()))).unwrap();
    let fake_relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    let forger = UdpSocket::bind("127.0.0.1:0").unwrap();
    fake_relay.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let relay_addr = fake_relay.local_addr().unwrap();
    set_relay(&e, relay_addr, &relay_token(&e, "user-solo", 600), Some(600)).unwrap();
    let mut buf = [0u8; 512];
    let (n, engine_addr) = fake_relay.recv_from(&mut buf).expect("RelayBind");
    assert!(n >= 18 && buf[3] == 7, "RelayBind");
    // 그 Bind에 대한 올바른 Ok Ack(status 0)
    let mut ack = [0u8; 50];
    ack[..18].copy_from_slice(&buf[..18]);
    ack[3] = 9;
    ack[16] = 0;
    ack[18..20].copy_from_slice(&30u16.to_be_bytes());
    forger.send_to(&ack, engine_addr).unwrap();
    // 고정 sleep 대신 진행량(P3-0a): 위조 Ack 뒤로 엔진이 Bind를 두 번 더 재전송할 때까지 기다린다(BIND_RETRY 간격).
    // 위조 Ack로 등록됐다면 재전송이 멈춰 상한에서 실패하고, 재전송이 이어지면 아직 미등록이다.
    let mut latest = [0u8; 18];
    let mut retries = 0;
    let t0 = Instant::now();
    while retries < 2 {
        assert!(t0.elapsed() < HANG, "Bind 재전송이 멈춤(위조 Ack로 등록됐을 수 있음): {:?}", relay_state(&e));
        if let Ok((n, _)) = fake_relay.recv_from(&mut buf) {
            if n >= 18 && buf[3] == 7 {
                latest.copy_from_slice(&buf[..18]);
                retries += 1;
            }
        }
    }
    assert_ne!(relay_state(&e), Some(RelayStateKind::Bound), "위조 출발 주소의 Ack로 등록되면 안 된다");
    // 마지막 Bind에 대한 Ok Ack를 릴레이 주소에서 보낸다
    ack[..18].copy_from_slice(&latest);
    ack[3] = 9;
    ack[16] = 0;
    fake_relay.send_to(&ack, engine_addr).unwrap();
    assert!(wait_relay_state(&e, RelayStateKind::Bound, HANG), "릴레이 주소에서 온 Ack는 받는다");
}
