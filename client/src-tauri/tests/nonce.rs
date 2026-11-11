//! Audio nonce 유일성 회귀 테스트(PR #13 H1).
//!
//! nonce = `sender_ssrc ‖ type ‖ 0 ‖ seq`이고 송신 키는 (내 키쌍·ssrc, 상대 공개키, 상대 ssrc)로 정해진다.
//! 이 Net의 키쌍·ssrc는 고정이므로 (상대 공개키, 상대 ssrc, seq)가 세션 동안 반복되면 nonce 재사용이다.
//! 원시 UDP 소켓이 상대 피어 역할을 하며 Net이 보낸 Audio 헤더를 모두 모은다.

use std::collections::HashSet;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use band_lib::engine::jitter::JitterSettings;
use band_lib::engine::stats::{PathKind, PeerStateKind};
use band_lib::net::crypto::KeyPair;
use band_lib::net::packet::{Header, PacketType};
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor, ECHO_FLAG_REQUEST, ECHO_TAG};
use band_lib::net::relay::DATA_PREFIX_BYTES;
use band_lib::net::{Net, NetOptions, AUDIO_SEQ_HANDOVER_GAP};

const PEER: &str = "bob";

fn net() -> Net {
    let opts = NetOptions {
        port: 0,
        stun_servers: vec![],
        gather_lan: false,
        extra_candidates: vec![],
        allow_loopback_candidates: true,
        cancel: None,
    };
    Net::start("alice".into(), &opts, JitterSettings { min_ms: 0.0, max_ms: 80.0 }).unwrap()
}

/// 상대 역할: 키쌍, ssrc, 소켓.
struct Fake {
    keys: KeyPair,
    ssrc: u32,
    sock: UdpSocket,
    last: std::cell::Cell<Option<u32>>,
}

impl Fake {
    fn new(ssrc: u32) -> Fake {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
        Fake { keys: KeyPair::generate(), ssrc, sock, last: std::cell::Cell::new(None) }
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

    /// 지금까지 받은 Net의 Audio 패킷 seq를 `seen`에 넣는다. 반환: 새로 본 개수, 중복 개수.
    /// 키마다 seq가 이전 최대값 뒤에서 이어지는지(`last`)도 본다: 재추가가 새 난수에서 시작하면 실패한다.
    fn collect(&self, relay: bool, seen: &mut HashSet<([u8; 32], u32, u32)>) -> (usize, usize) {
        let (mut new, mut dup) = (0, 0);
        let mut last = self.last.get();
        let mut buf = [0u8; 2048];
        while let Ok(n) = self.sock.recv(&mut buf) {
            let body = if relay && n > DATA_PREFIX_BYTES && buf[0] != 0x42 { &buf[DATA_PREFIX_BYTES..n] } else { &buf[..n] };
            let Some(h) = Header::parse(body) else { continue };
            if h.ptype != PacketType::Audio {
                continue;
            }
            if let Some(l) = last {
                let ahead = h.seq.wrapping_sub(l);
                assert!((1..=AUDIO_SEQ_HANDOVER_GAP + 1000).contains(&ahead), "seq {} 가 이전 {} 뒤에서 이어지지 않음", h.seq, l);
            }
            last = Some(h.seq);
            if seen.insert((self.keys.public_bytes(), self.ssrc, h.seq)) {
                new += 1;
            } else {
                dup += 1;
            }
        }
        self.last.set(last);
        (new, dup)
    }
}

/// 핸드셰이크 없이 경로를 세운다(테스트: 송신 경로만 본다).
fn force_path(net: &Net, addr: SocketAddr, kind: PathKind) {
    let p = net.peers().into_iter().find(|p| p.id == PEER).unwrap();
    let mut c = p.conn.lock();
    c.state = PeerStateKind::Connected;
    c.path = Some((addr, kind));
    c.last_rx = Some(Instant::now());
}

fn send(net: &Net, n: usize) {
    let mut buf = vec![0u8; 1500];
    for i in 0..n {
        net.shared().send_audio(&[i as u8; 40], 0, 1, (i * 120) as u32, &mut buf, None);
    }
}

#[test]
fn audio_nonce_never_repeats_across_remove_add_replace_relay_and_echo() {
    let net = net();
    let net_addr: SocketAddr = format!("127.0.0.1:{}", net.local_port()).parse().unwrap();
    let net_desc = net.descriptor.clone();
    let mut seen = HashSet::new();
    let mut total = 0;
    let fake = Fake::new(0x1111_2222);

    // 1) 제거 → 같은 descriptor 재추가 반복(WS 순단 뒤 같은 descriptor 재수신)
    for _ in 0..6 {
        net.add_peer(PEER.into(), &fake.desc()).unwrap();
        force_path(&net, fake.addr(), PathKind::Lan);
        send(&net, 40);
        std::thread::sleep(Duration::from_millis(30));
        let (n, d) = fake.collect(false, &mut seen);
        assert_eq!(d, 0, "제거 후 재추가에서 nonce 중복");
        total += n;
        assert!(net.remove_peer(PEER));
    }
    // 2) 교체(같은 id로 다시 add)와 3) 릴레이 경로
    net.add_peer(PEER.into(), &fake.desc()).unwrap();
    force_path(&net, fake.addr(), PathKind::Lan);
    send(&net, 40);
    net.add_peer(PEER.into(), &fake.desc()).unwrap();
    force_path(&net, fake.addr(), PathKind::Relay);
    send(&net, 40);
    std::thread::sleep(Duration::from_millis(30));
    let (n, d) = fake.collect(true, &mut seen);
    assert_eq!(d, 0, "교체·릴레이 경로에서 nonce 중복");
    total += n;

    // 4) 같은 키를 다른 id로 추가해도 거부하지 않고 송신 카운터를 같이 쓴다(P1-1b, 자세한 검사는 tests/peer_key.rs).
    net.add_peer("bob-2".into(), &fake.desc()).unwrap();
    assert!(net.remove_peer("bob-2"));

    // 5) 에코 반사: 상대가 요청 Ping과 Audio를 보내면 수신 스레드가 같은 카운터로 되돌린다
    force_path(&net, fake.addr(), PathKind::Lan);
    let net_pk = net_desc.decode_key().unwrap();
    let (tx, _rx) = fake.keys.pair_ciphers(fake.ssrc, &net_pk, net_desc.ssrc).unwrap();
    let seal_send = |t: PacketType, seq: u32, ts: u32, plain: &[u8]| {
        let h = Header { ptype: t, ssrc: fake.ssrc, seq, timestamp: ts, flags: 0, frame_code: 1 };
        let mut out = [0u8; 1500];
        let n = tx.seal(&h, plain, &mut out).unwrap();
        fake.sock.send_to(&out[..n], net_addr).unwrap();
    };
    seal_send(PacketType::Ping, 1, 0, &[ECHO_TAG, ECHO_FLAG_REQUEST, 0, 0, 0, 0]);
    let t0 = Instant::now();
    while net.shared().set_echo_mode(PEER, true).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(2), "요청 Ping이 처리되지 않음");
        std::thread::sleep(Duration::from_millis(5));
    }
    // 에코하는 동안 같은 상대에게 측정 요청은 거부(양방향 동시 에코 방지, L1)
    assert!(net.shared().set_echo_request(PEER, true).is_err());
    for i in 0..30u32 {
        seal_send(PacketType::Audio, 1000 + i, i * 120, &[7u8; 40]);
        send(&net, 1); // 반사 중인 피어에게는 내 오디오를 보내지 않는다(카운터를 쓰지 않음)
    }
    std::thread::sleep(Duration::from_millis(50));
    let (n, d) = fake.collect(false, &mut seen);
    assert_eq!(d, 0, "에코 반사에서 nonce 중복");
    assert!(n >= 25, "반사 패킷 {n}");
    total += n;
    // 반사 중 제거 → 재추가
    assert!(net.remove_peer(PEER));
    net.add_peer(PEER.into(), &fake.desc()).unwrap();
    force_path(&net, fake.addr(), PathKind::Lan);
    send(&net, 40);
    std::thread::sleep(Duration::from_millis(30));
    let (n, d) = fake.collect(false, &mut seen);
    assert_eq!(d, 0, "반사 뒤 재추가에서 nonce 중복");
    total += n;

    // 6) ssrc 재생성(상대 엔진 재시작 = 새 키): 다른 키이므로 독립, 그 안에서도 중복 없음
    assert!(net.remove_peer(PEER));
    let fake2 = Fake::new(0x3333_4444);
    for _ in 0..3 {
        net.add_peer(PEER.into(), &fake2.desc()).unwrap();
        force_path(&net, fake2.addr(), PathKind::Lan);
        send(&net, 40);
        std::thread::sleep(Duration::from_millis(30));
        let (n, d) = fake2.collect(false, &mut seen);
        assert_eq!(d, 0, "새 키에서 nonce 중복");
        total += n;
        assert!(net.remove_peer(PEER));
    }
    assert!(total >= 440, "수집한 패킷 {total}");
}
