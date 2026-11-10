//! 릴레이 폴백 통합 테스트(P1-4b, relay-design.md).
//!
//! 실제 `band-relay` 크레이트를 같은 프로세스의 tokio 런타임으로 로컬 UDP에 띄운다. 두 엔진의 직접 후보는
//! 열고 닫을 수 있는 드롭 프록시를 가리킨다(닫히면 모든 패킷 폐기 = 대칭형 NAT 흉내).
//!
//! - 직접 경로 차단 → 릴레이로 연결되고 오디오가 흐른다(연결 시간, 버퍼 지연, 은닉률, 연속성).
//! - 직접 경로가 나중에 열리면 직접(LAN)으로 승격되고 그 사이 소리가 끊기지 않는다.
//! - 릴레이 재시작 → 쿠키 절차로 재등록하고 소리가 돌아온다.
//! - 토큰 만료 전 갱신 → 같은 주소 갱신으로 등록이 이어진다.

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
use band_lib::net::NetOptions;
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
        let relay = Relay::new(SECRET.to_vec(), Limits::default(), self.stats.clone());
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

// ---------------------------------------------------------------------------- 드롭 프록시(직접 경로)

/// A ↔ pa ... pb ↔ B. `open`이 false면 모든 패킷을 버린다(직접 경로 차단).
struct GateProxy {
    pa: SocketAddr,
    pb: SocketAddr,
    open: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    forwarded: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl GateProxy {
    fn start(a: SocketAddr, b: SocketAddr) -> GateProxy {
        let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
        // 비블로킹 + 짧은 sleep(Windows에서 1ms 읽기 타임아웃은 ~15ms로 늘어나 지터가 된다)
        sa.set_nonblocking(true).unwrap();
        sb.set_nonblocking(true).unwrap();
        let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
        let open = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let forwarded = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (op, st, fw, dr) = (open.clone(), stop.clone(), forwarded.clone(), dropped.clone());
        let thread = std::thread::spawn(move || {
            let _prio = band_lib::engine::prio::promote_current_thread();
            let mut buf = [0u8; 2048];
            while !st.load(Ordering::Relaxed) {
                for (sock, out, to) in [(&sa, &sb, b), (&sb, &sa, a)] {
                    while let Ok((n, _)) = sock.recv_from(&mut buf) {
                        if op.load(Ordering::Relaxed) {
                            let _ = out.send_to(&buf[..n], to);
                            fw.fetch_add(1, Ordering::Relaxed);
                        } else {
                            dr.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        GateProxy { pa, pb, open, stop, forwarded, dropped, thread: Some(thread) }
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

struct Pair {
    a: Engine,
    b: Engine,
    proxy: GateProxy,
    sink_b: Arc<Mutex<Vec<f32>>>,
    /// add_peer 시각
    t_added: Instant,
}

/// A(사인) → B. 직접 후보는 닫힌 프록시, 릴레이는 `relay`. 토큰 수명 `ttl_secs`.
fn relay_pair(relay: &RelayServer, ttl_secs: u64) -> Pair {
    let backend_b = FakeBackend::silence(48_000);
    let sink_b = backend_b.sink.clone();
    let a = Engine::start(config(), Arc::new(sine_backend()), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(config(), Arc::new(backend_b), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let proxy = GateProxy::start(local_addr(&a), local_addr(&b));
    let cand = |d: &LocalDescriptor, addr: SocketAddr| LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    };
    let t_added = Instant::now();
    a.add_peer("bob".into(), &cand(b.descriptor().unwrap(), proxy.pa)).unwrap();
    b.add_peer("alice".into(), &cand(a.descriptor().unwrap(), proxy.pb)).unwrap();
    // 서버의 relay-token은 native-descriptor 직후에 온다: add_peer와 거의 동시
    a.set_relay(Some((relay.addr, &relay_token(&a, "user-alice", ttl_secs)))).unwrap();
    b.set_relay(Some((relay.addr, &relay_token(&b, "user-bob", ttl_secs)))).unwrap();
    Pair { a, b, proxy, sink_b, t_added }
}

fn peer_path(e: &Engine) -> Option<(PeerStateKind, Option<PathKind>)> {
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

#[derive(Debug, Default)]
struct Continuity {
    discontinuities: usize,
    silent_runs: usize,
    /// 5ms 창 중 소리 있는 비율
    sound_ratio: f64,
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
    let (mut windows, mut sound) = (0, 0);
    for w in seg.as_chunks::<480>().0 {
        windows += 1;
        if (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt() > 0.15 {
            sound += 1;
        }
    }
    c.sound_ratio = sound as f64 / windows.max(1) as f64;
    c
}

fn relay_state(e: &Engine) -> Option<RelayStateKind> {
    e.stats().relay.map(|r| r.state)
}

// ============================================================================

#[test]
fn direct_blocked_connects_via_relay_then_promotes_to_direct() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(&relay, 600);

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
    let relay_rtt = stats_a.relay.as_ref().and_then(|r| r.rtt_ms);
    let seg = p.sink_b.lock()[start + 48_000 * 2..].to_vec(); // 첫 1초(수렴) 제외
    let cont_relay = analyze(&seg);
    let xruns_b = p.b.stats().xruns;

    // 직접 경로 개방 → LAN으로 승격(무끊김)
    let before_open = p.sink_b.lock().len();
    let t_open = Instant::now();
    p.proxy.open.store(true, Ordering::Relaxed);
    let promote = wait_path(&p, PathKind::Lan, Duration::from_secs(6)).expect("직접 경로로 승격되지 않음");
    std::thread::sleep(Duration::from_secs(3));
    let seg = p.sink_b.lock()[before_open..].to_vec();
    let cont_switch = analyze(&seg);
    let js_end = p.b.peer_jitter_stats()[0].1;
    let xruns_b2 = p.b.stats().xruns;
    let forwarded = relay.stats.forwarded_packets.load(Ordering::Relaxed);

    let conceal_relay = js_relay.concealed as f64 / js_relay.produced.max(1) as f64;
    println!("=== 릴레이 폴백 (직접 경로 차단, 로컬 UDP, debug 빌드) ===");
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
    println!("A→B 종단 RTT(릴레이 경유) {rtt_relay:?}ms, A↔릴레이 RTT {relay_rtt:?}ms, 릴레이 전달 {forwarded}패킷");
    println!(
        "승격: 개방 후 {promote:?}(총 {:?}), 전환 구간 연속성 {:.2}%, 무음 {} 불연속 {}, xrun {}, 끝 버퍼 {:.1}ms",
        t_open.elapsed(),
        cont_switch.sound_ratio * 100.0,
        cont_switch.silent_runs,
        cont_switch.discontinuities,
        xruns_b2 - xruns_b,
        js_end.buffer_ms
    );

    // GOAL 3절: 입장 후 3초 이내 소리 (폴백 1.5초 + 릴레이 왕복 + 지터 버퍼)
    assert!(first_sound_ms < 3000, "첫 소리까지 {first_sound_ms}ms");
    assert!(js_end.buffer_ms < 30.0, "승격 후 버퍼 지연 {}", js_end.buffer_ms);
    assert!(cont_relay.sound_ratio > 0.97, "릴레이 경로 연속성 {:?}", cont_relay);
    assert!(cont_relay.silent_runs as u64 <= 3 * xruns_b, "릴레이 경로 무음 {:?} (xrun {xruns_b})", cont_relay);
    assert!(conceal_relay < 0.02, "릴레이 경로 은닉 {conceal_relay}");
    assert!(js_relay.buffer_ms < 30.0, "릴레이 경로 버퍼 {}", js_relay.buffer_ms);
    assert!(rtt_relay.is_some() && relay_rtt.is_some(), "RTT 측정");
    // 경로 전환은 지터 버퍼를 유지하므로 끊김이 없어야 한다(장치 xrun으로 설명되는 것만 허용)
    assert!(
        (cont_switch.silent_runs + cont_switch.discontinuities) as u64 <= 3 * (xruns_b2 - xruns_b),
        "전환 구간 끊김 {:?}",
        cont_switch
    );
    assert!(cont_switch.sound_ratio > 0.97, "전환 구간 연속성 {:?}", cont_switch);
    assert!(p.proxy.forwarded.load(Ordering::Relaxed) > 0);
}

#[test]
fn relay_restart_reregisters_and_audio_resumes() {
    let _g = RT_LOCK.lock();
    let mut relay = RelayServer::start();
    let p = relay_pair(&relay, 600);
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

#[test]
fn token_refresh_before_expiry_keeps_relay_registration() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    // 수명 5초 토큰(만료 시각은 초 단위라 실제 4~5초)
    let p = relay_pair(&relay, 5);
    wait_path(&p, PathKind::Relay, Duration::from_secs(4)).expect("릴레이 경로로 연결되지 않음");
    let ok_before = relay.stats.bind_ok.load(Ordering::Relaxed);
    // 프론트가 expiresAt 전에 relay-token-request로 받은 새 토큰을 넘긴다
    p.a.set_relay(Some((relay.addr, &relay_token(&p.a, "user-alice", 600)))).unwrap();
    p.b.set_relay(Some((relay.addr, &relay_token(&p.b, "user-bob", 600)))).unwrap();
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
fn expired_token_without_refresh_stops_binding() {
    let _g = RT_LOCK.lock();
    let relay = RelayServer::start();
    let p = relay_pair(&relay, 3);
    let t0 = Instant::now();
    while relay_state(&p.a) != Some(RelayStateKind::Bound) && t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Bound));
    std::thread::sleep(Duration::from_secs(4));
    println!("=== 토큰 만료(갱신 없음) === A 릴레이 {:?}, 피어 {:?}", p.a.stats().relay, peer_path(&p.a));
    assert_eq!(relay_state(&p.a), Some(RelayStateKind::Expired), "만료되면 Bind를 멈추고 Expired로 보고");
    // 만료 토큰은 받지 않는다
    let expired = {
        let ssrc = p.a.descriptor().unwrap().ssrc;
        let t = token::sign(SECRET, &Claims { expires_at: unix_now() - 1, ssrc, room: ROOM, client: "user-alice" });
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(t)
    };
    assert!(p.a.set_relay(Some((relay.addr, &expired))).is_err());
    // 남의 ssrc 토큰도 받지 않는다
    assert!(p.a.set_relay(Some((relay.addr, &relay_token(&p.b, "user-bob", 600)))).is_err());
}
