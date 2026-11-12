//! 같은 키(공개키·ssrc) 피어 항목과 피어 제거 경쟁(P1-1b, PR #13 리뷰 Low).
//!
//! - 선점에 의한 가용성 방해: 방의 다른 참가자가 남의 descriptor를 그대로 복사해 자기 id로 먼저 보내도
//!   진짜 주인의 추가·연결을 막지 못한다(예전에는 "같은 키" 거부로 진짜 주인이 막혔다).
//! - 같은 키의 살아 있는 항목들은 송신 카운터 하나를 같이 쓴다 → (키, seq) nonce가 겹치지 않는다.
//! - 제거의 수위 기록은 목록 write 락 안에서 한다 → 제거·추가가 동시에 와도 seq를 다시 쓰지 않는다.
//!
//! 결정적: 판정은 상태·받은 패킷으로 하고, 벽시계는 행 감지 상한(`HANG`)뿐이다.

use std::collections::HashSet;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::net::media_link::{media_link, PeerKey};
use band_lib::engine::mixer::MixControls;
use band_lib::net::types::{PathKind, PeerStateKind};
use band_lib::engine::Engine;
use band_lib::net::crypto::KeyPair;
use band_lib::net::packet::{Header, PacketType};
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::{Net, NetOptions};
use parking_lot::Mutex;


/// 엔진 없이 Net만 쓸 때의 피어 추가(엔진 ③). 수신 큐 소비자(`MediaRx`)는 버린다 — 이 테스트는 수신 오디오를 보지
/// 않는다. 오디오가 오면 큐가 찰 때까지 쌓이다 버려진다.
fn add(net: &Net, id: String, desc: &LocalDescriptor) -> Result<Option<PeerKey>, String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let key = PeerKey::from_raw(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    net.add_peer(id, desc, media_link(key).0)
}

const HANG: Duration = Duration::from_secs(20);

static RT_LOCK: Mutex<()> = Mutex::new(());

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

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ============================================================================
// 엔진 수준: 선점자가 진짜 주인을 막지 못한다
// ============================================================================

fn engine(id: &str, backend: FakeBackend) -> Engine {
    let cfg = EngineConfig { buffer_size: 128, frame_ms: 5.0, codec: CodecKind::Opus, channels: 1, input_channels: vec![0], ..Default::default() };
    Engine::start(cfg, Arc::new(backend), Arc::new(MixControls::default()), Some((id.into(), test_net()))).unwrap()
}

fn desc_at(e: &Engine) -> LocalDescriptor {
    let addr = format!("127.0.0.1:{}", e.local_port().unwrap());
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr }], ..e.descriptor().unwrap().clone() }
}

fn state(e: &Engine, id: &str) -> Option<PeerStateKind> {
    e.stats().peers.iter().find(|p| p.peer_id == id).map(|p| p.state)
}

/// 지터 버퍼가 실제 패킷으로 만든 프레임 수(은닉 제외).
fn real_frames(e: &Engine, id: &str) -> u64 {
    e.peer_jitter_stats().iter().find(|(p, _)| p == id).map(|(_, js)| js.produced.saturating_sub(js.concealed)).unwrap_or(0)
}

fn sine() -> FakeBackend {
    FakeBackend::new(
        48_000,
        Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
            for (i, f) in buf.chunks_exact_mut(ch).enumerate() {
                f.fill(((start + i as u64) as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.5);
            }
        }),
    )
}

/// mallory가 alice의 descriptor(키·ssrc·후보)를 그대로 복사해 bob에게 alice보다 **먼저**(또는 나중에) 보낸다.
/// 진짜 alice는 추가·연결되고 소리도 alice 항목으로 들어온다. 개인키가 없는 mallory 항목은 연결되지 않는다.
fn squat(squat_first: bool) {
    let _g = RT_LOCK.lock();
    let a = engine("alice", sine());
    let b = engine("bob", FakeBackend::silence(48_000));
    let da = desc_at(&a);
    if squat_first {
        b.add_peer("mallory".into(), &da).unwrap();
    }
    b.add_peer("alice".into(), &da).expect("선점된 키라도 진짜 주인의 추가는 받아야 한다");
    a.add_peer("bob".into(), &desc_at(&b)).unwrap();
    assert!(wait_until(HANG, || state(&b, "alice") == Some(PeerStateKind::Connected)), "alice가 연결되지 않음: {:?}", b.stats().peers);
    assert!(wait_until(HANG, || state(&a, "bob") == Some(PeerStateKind::Connected)), "A 쪽 bob이 연결되지 않음");
    if !squat_first {
        b.add_peer("mallory".into(), &da).unwrap();
    }
    // A의 소리는 alice 항목으로 계속 들어온다(진행량: 프레임 200개 = 1초분)
    let f0 = real_frames(&b, "alice");
    assert!(wait_until(HANG, || real_frames(&b, "alice") >= f0 + 200), "alice 오디오가 흐르지 않음");
    assert_eq!(state(&b, "alice"), Some(PeerStateKind::Connected));
    assert_ne!(state(&b, "mallory"), Some(PeerStateKind::Connected), "개인키 없는 선점 항목은 연결되면 안 된다");
    assert_eq!(real_frames(&b, "mallory"), 0, "alice의 소리가 mallory 항목으로 새면 안 된다");
}

#[test]
fn squatted_key_does_not_block_real_owner() {
    squat(true);
}

#[test]
fn squat_after_connect_does_not_hijack() {
    squat(false);
}

// ============================================================================
// Net 수준: 송신 카운터 공유와 제거 경쟁
// ============================================================================

fn net() -> Net {
    Net::start("alice".into(), &test_net()).unwrap()
}

/// 상대 역할(원시 소켓): Net이 보낸 Audio 헤더의 seq를 모은다.
struct Fake {
    keys: KeyPair,
    ssrc: u32,
    sock: UdpSocket,
}

impl Fake {
    fn new(ssrc: u32) -> Fake {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
        Fake { keys: KeyPair::generate(), ssrc, sock }
    }

    fn addr(&self) -> SocketAddr {
        self.sock.local_addr().unwrap()
    }

    fn desc(&self) -> LocalDescriptor {
        LocalDescriptor {
            ssrc: self.ssrc,
            key: LocalDescriptor::encode_key(&self.keys.public_bytes()),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: self.addr().to_string() }],
        }
    }

    /// Audio 패킷을 `want`개 받을 때까지 seq를 `seen`에 넣는다. 반환: 중복 개수.
    fn collect(&self, seen: &mut HashSet<u32>, want: usize) -> usize {
        let (mut got, mut dup) = (0, 0);
        let mut buf = [0u8; 2048];
        let t0 = Instant::now();
        while got < want {
            assert!(t0.elapsed() < HANG, "Audio {got}/{want}개만 받음");
            let Ok(n) = self.sock.recv(&mut buf) else { continue };
            let Some(h) = Header::parse(&buf[..n]) else { continue };
            if h.ptype != PacketType::Audio {
                continue;
            }
            got += 1;
            if !seen.insert(h.seq) {
                dup += 1;
            }
        }
        dup
    }
}

/// 핸드셰이크 없이 경로를 세운다(송신 경로만 본다).
fn force_path(net: &Net, id: &str, addr: SocketAddr) {
    if let Some(p) = net.peers().into_iter().find(|p| p.id() == id) {
        p.force_path(addr, PathKind::Lan);
    }
}

fn send(net: &Net, n: usize) {
    let mut buf = vec![0u8; 1500];
    for i in 0..n {
        net.shared().send_audio(&[i as u8; 40], 0, 1, (i * 120) as u32, &mut buf, None, None);
    }
}

/// 같은 키를 다른 id로 넣어도(선점) 송신 카운터는 하나다: 두 항목이 모두 보내도 (키, seq)가 겹치지 않는다.
/// 한쪽을 지우고 다시 넣으면 살아 있는 쪽 카운터를 이어 쓰고, 모두 지운 뒤 다시 넣으면 기록 + 여유에서 이어 간다.
#[test]
fn same_key_entries_share_one_tx_counter() {
    let net = net();
    let fake = Fake::new(0x5555_6666);
    let mut seen = HashSet::new();
    add(&net, "bob".into(), &fake.desc()).unwrap();
    add(&net, "bob-copy".into(), &fake.desc()).expect("같은 키라도 거부하지 않는다");
    let peer = |id: &str| net.peers().into_iter().find(|p| p.id() == id).unwrap();
    assert!(peer("bob").shares_tx_seq_with(&peer("bob-copy")), "같은 키의 항목은 카운터를 같이 써야 한다");
    for id in ["bob", "bob-copy"] {
        force_path(&net, id, fake.addr());
    }
    send(&net, 40); // 항목마다 40개 = 80개
    assert_eq!(fake.collect(&mut seen, 80), 0, "같은 키 두 항목 사이 nonce 중복");

    // 한쪽 제거 → 재추가: 살아 있는 bob-copy의 카운터를 이어 쓴다
    assert!(net.remove_peer("bob").is_some());
    add(&net, "bob".into(), &fake.desc()).unwrap();
    assert!(peer("bob").shares_tx_seq_with(&peer("bob-copy")));
    force_path(&net, "bob", fake.addr());
    send(&net, 40);
    assert_eq!(fake.collect(&mut seen, 80), 0, "재추가 뒤 nonce 중복");

    // 같은 id 교체도 마찬가지
    add(&net, "bob-copy".into(), &fake.desc()).unwrap();
    assert!(peer("bob").shares_tx_seq_with(&peer("bob-copy")));
    force_path(&net, "bob-copy", fake.addr());
    send(&net, 40);
    assert_eq!(fake.collect(&mut seen, 80), 0, "교체 뒤 nonce 중복");

    // 모두 제거 → 재추가: 기록(수위) + 여유에서 이어 간다
    assert!(net.remove_peer("bob").is_some() && net.remove_peer("bob-copy").is_some());
    add(&net, "bob".into(), &fake.desc()).unwrap();
    force_path(&net, "bob", fake.addr());
    send(&net, 40);
    assert_eq!(fake.collect(&mut seen, 40), 0, "모두 제거 뒤 재추가에서 nonce 중복");
    assert_eq!(seen.len(), 80 + 80 + 80 + 40);
}

/// 같은 id를 두 스레드가 동시에 추가·송신·제거한다(WS 순단·재수신이 겹치는 경우). 제거의 수위 기록이 목록 락
/// 밖에 있으면, 그 사이 추가된 항목이 옛 수위에서 시작해 방금 지운 항목이 쓴 seq를 다시 쓸 수 있다.
/// 올바른 구현에서는 항상 통과한다(경쟁이 없을 때 실패하지 않는다). 받는 쪽은 별도 스레드로 계속 비운다.
#[test]
fn concurrent_remove_and_add_never_reuse_seq() {
    let net = Arc::new(net());
    let fake = Arc::new(Fake::new(0x7777_8888));
    let stop = Arc::new(AtomicBool::new(false));
    let collector = {
        let (fake, stop) = (fake.clone(), stop.clone());
        std::thread::spawn(move || {
            let (mut seen, mut dup, mut total) = (HashSet::new(), 0usize, 0usize);
            let mut buf = [0u8; 2048];
            let mut idle = 0;
            while idle < 5 {
                match fake.sock.recv(&mut buf) {
                    Ok(n) => {
                        idle = 0;
                        if let Some(h) = Header::parse(&buf[..n]).filter(|h| h.ptype == PacketType::Audio) {
                            total += 1;
                            if !seen.insert(h.seq) {
                                dup += 1;
                            }
                        }
                    }
                    Err(_) if stop.load(Ordering::Relaxed) => idle += 1,
                    Err(_) => {}
                }
            }
            (total, dup)
        })
    };
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let (net, fake) = (net.clone(), fake.clone());
            std::thread::spawn(move || {
                for _ in 0..300 {
                    add(&net, "bob".into(), &fake.desc()).unwrap();
                    force_path(&net, "bob", fake.addr());
                    send(&net, 3);
                    net.remove_peer("bob");
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let (total, dup) = collector.join().unwrap();
    println!("동시 제거·추가: Audio {total}개 수신, 중복 {dup}");
    assert!(total > 0, "패킷을 하나도 받지 못함");
    assert_eq!(dup, 0, "제거·추가 경쟁에서 nonce 중복");
}

/// 녹음 트랙 이름(P2-4, PR #47 M3): 피어가 빠진 뒤에도 그 ssrc는 같은 peerId로 풀린다.
#[test]
fn removed_peer_ssrc_still_resolves() {
    let net = net();
    let fake = Fake::new(0x7777_8888);
    add(&net, "carol".into(), &fake.desc()).unwrap();
    let ssrc = fake.ssrc;
    assert_eq!(net.shared().peer_id_for_ssrc(ssrc).as_deref(), Some("carol"));
    assert!(net.remove_peer("carol").is_some());
    assert_eq!(net.shared().peer_id_for_ssrc(ssrc).as_deref(), Some("carol"), "빠진 피어도 기록에서 찾는다");
    assert_eq!(net.shared().peer_id_for_ssrc(ssrc ^ 1), None);
}

/// 엔진 ③-a2: 추가·제거·교체·프로필 변경·통계 읽기가 여러 커맨드 스레드에서 겹쳐도 등록부·net·워커 표가 어긋나지 않는다.
/// 끝나면 남은 피어는 alice뿐이고(등록부·`stats`·`peer_jitter_stats`가 같은 집합), alice 소리는 계속 흐른다
/// (워커 표에 죽은 항목이 남아 슬롯을 잡거나, alice 항목이 사라지면 실패).
#[test]
fn add_remove_replace_profile_race_keeps_tables_consistent() {
    let _g = RT_LOCK.lock();
    let a = engine("alice", sine());
    let b = Arc::new(engine("bob", FakeBackend::silence(48_000)));
    b.add_peer("alice".into(), &desc_at(&a)).unwrap();
    a.add_peer("bob".into(), &desc_at(&b)).unwrap();
    assert!(wait_until(HANG, || state(&b, "alice") == Some(PeerStateKind::Connected)), "alice가 연결되지 않음");

    let fakes: Arc<Vec<Fake>> = Arc::new((0..3).map(|i| Fake::new(0x9000 + i)).collect());
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    // 추가·제거 반복(x0, x1)
    for i in 0..2 {
        let (b, fakes, stop) = (b.clone(), fakes.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            let id = format!("x{i}");
            while !stop.load(Ordering::Relaxed) {
                b.add_peer(id.clone(), &fakes[i].desc()).unwrap();
                b.remove_peer(&id).unwrap();
            }
        }));
    }
    // 같은 id 교체 반복(x2)
    {
        let (b, fakes, stop) = (b.clone(), fakes.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                b.add_peer("x2".into(), &fakes[2].desc()).unwrap();
            }
        }));
    }
    // 프로필 변경 + 통계 읽기
    {
        let (b, stop) = (b.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            let mut p = 0.2f32;
            while !stop.load(Ordering::Relaxed) {
                b.set_jitter_profile(p).unwrap();
                p = if p < 0.5 { 0.8 } else { 0.2 };
                let _ = b.stats();
                let _ = b.peer_jitter_stats();
            }
        }));
    }
    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    b.remove_peer("x2").unwrap();

    let mut ids: Vec<String> = b.peer_jitter_stats().into_iter().map(|(id, _)| id).collect();
    ids.sort();
    assert_eq!(ids, vec!["alice".to_string()], "등록부에 남은 피어");
    let net_ids: Vec<String> = b.stats().peers.into_iter().map(|p| p.peer_id).collect();
    assert_eq!(net_ids, vec!["alice".to_string()], "net 피어 표와 등록부가 맞아야 한다");
    // 워커 표도 직접 본다(#117 리뷰 L4): 죽은 항목이 남으면 1이 되지 않는다
    assert!(wait_until(HANG, || b.worker_media_peers() == 1), "워커 표에 남은 항목: {}", b.worker_media_peers());
    let f0 = real_frames(&b, "alice");
    assert!(wait_until(HANG, || real_frames(&b, "alice") >= f0 + 200), "경합 뒤 alice 오디오가 흐르지 않음");
}
