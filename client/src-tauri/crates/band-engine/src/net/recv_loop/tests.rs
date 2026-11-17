//! 네트워크 입력 퍼징(P2 발굴 — 네트워크 파서). Windows에서 cargo-fuzz(libFuzzer)를 쓰기 어려워 시드 고정 무작위 입력으로 대신한다.
//!
//! 기본 `cargo test`는 짧게 돈다. 긴 판: `BAND_FUZZ_LONG=1 cargo test -p band-engine --release --lib fuzz -- --ignored`
//! (반복 수는 `BAND_FUZZ_ITERS`로 바꾼다).
//!
//! - `handle_packet` 끝에서 끝: 키를 아는 상대(피어)로서 새 패킷·그대로 다시 보낸 패킷·한 바이트라도 바꾼 패킷·무작위 바이트를
//!   섞어 넣고, 실제로 받아들인 것(돌아온 Pong, 수신 큐에 들어간 Audio, Bye)이 "인증되고 새로운 것"과 정확히 같은지 본다.
//! - 개별 파서: 암호 프레임 `open`, STUN 응답, Ping 평문 블록, Pong 시계, 릴레이 BindAck(`on_ack`), 릴레이 토큰.
//!
//! 판정: panic 없음, 끝남, 인증 실패 패킷 수락 0, 제어 패킷 재전송 수락 0.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_dsp::config::{HEADER_BYTES, MAX_PACKET_BYTES, TAG_BYTES};
use band_dsp::media_link::{media_link, MediaRx, PeerKey};
use band_dsp::packet::{path_tag, Header, PacketType, MAGIC, VERSION};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::net::crypto::{KeyPair, PacketCipher};
use crate::net::peer::{self, Candidate, CandidateKind, EchoState, LocalDescriptor, LossReport, Peer};
use crate::net::relay::{AckAction, RelayClient, RelayToken};
use crate::net::{parse_clock_payload, stun, Net, NetOptions, NetShared};

/// 긴 판은 `BAND_FUZZ_LONG=1`일 때만 돈다. CI realtime 잡(`--run-ignored only`, debug·직렬)이 `#[ignore]`를 모두 돌리므로
/// 가드가 없으면 수십 분이 더해진다(#190 리뷰 H1). 가드에 걸리면 아무것도 하지 않고 통과한다.
fn long_enabled(name: &str) -> bool {
    let on = std::env::var_os("BAND_FUZZ_LONG").is_some_and(|v| v == "1");
    if !on {
        eprintln!("{name}: BAND_FUZZ_LONG=1이 아니라 건너뜀");
    }
    on
}

fn iters(default: usize) -> usize {
    std::env::var("BAND_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn random_bytes(rng: &mut StdRng, max: usize) -> Vec<u8> {
    (0..rng.gen_range(0..=max)).map(|_| rng.gen()).collect()
}

/// 유효한 원본을 1~4번 바꾼다(비트 뒤집기·바이트 덮기·자르기·덧붙이기·구간 복제). 원본과 같아지면 마지막 바이트를 뒤집는다.
fn mutate(rng: &mut StdRng, orig: &[u8]) -> Vec<u8> {
    let mut b = orig.to_vec();
    for _ in 0..rng.gen_range(1..=4) {
        match rng.gen_range(0..6) {
            0 | 1 if !b.is_empty() => {
                let i = rng.gen_range(0..b.len());
                b[i] ^= 1 << rng.gen_range(0..8);
            }
            2 if !b.is_empty() => {
                let i = rng.gen_range(0..b.len());
                b[i] = rng.gen();
            }
            3 => {
                let n = rng.gen_range(0..=b.len());
                b.truncate(n);
            }
            4 => {
                for _ in 0..rng.gen_range(1..40) {
                    b.push(rng.gen());
                }
            }
            _ if b.len() > 2 => {
                let a = rng.gen_range(0..b.len() - 1);
                let e = rng.gen_range(a + 1..b.len());
                let chunk = b[a..e].to_vec();
                let at = rng.gen_range(0..=b.len());
                b.splice(at..at, chunk);
            }
            _ => b.push(rng.gen()),
        }
    }
    if b == orig {
        match b.last_mut() {
            Some(x) => *x ^= 0x80,
            None => b.push(0),
        }
    }
    b
}

/// 엔진 하나 + 키를 아는 상대 피어 하나. 스레드는 멈춰 두고 `handle_packet`을 직접 부른다.
/// 엔진이 상대에게 보내는 패킷(HelloAck·Pong)은 상대 소켓(`sock`, 피어 후보 주소)으로 실제로 도착한다.
struct Rig {
    _net: Net,
    s: Arc<NetShared>,
    peer: Arc<Peer>,
    sock: UdpSocket,
    from: SocketAddr,
    tx: PacketCipher,
    rx: PacketCipher,
    media: MediaRx,
    ssrc: u32,
    seq: [u32; 7],
}

impl Rig {
    fn new() -> Self {
        let opts = NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
        let mut net = Net::start("me".into(), &opts).unwrap();
        net.stop();
        let s = net.shared().clone();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_nonblocking(true).unwrap();
        let from = sock.local_addr().unwrap();
        let remote = KeyPair::generate();
        let ssrc = 0x5EED_0001;
        let desc = LocalDescriptor {
            ssrc,
            key: LocalDescriptor::encode_key(&remote.public_bytes()),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: from.to_string() }],
        };
        let (mtx, media) = media_link(PeerKey::from_raw(1));
        net.add_peer("peer".into(), &desc, mtx).unwrap();
        let peer = net.peers().into_iter().next().unwrap();
        let (tx, rx) = remote.pair_ciphers(ssrc, &s.keys.public_bytes(), s.ssrc).unwrap();
        let mut rig = Self { _net: net, s, peer, sock, from, tx, rx, media, ssrc, seq: [1000; 7] };
        // 핸드셰이크: 상대 id를 실은 Hello → 경로 확정(그 뒤 Audio·Ping을 받는다)
        let hello = rig.seal(PacketType::Hello, 1, path_tag(false), 0, b"peer");
        rig.feed(&hello, rig.from);
        assert!(rig.peer.conn().path.is_some(), "핸드셰이크 실패 — 시험 설정 오류");
        rig.drain_replies();
        rig
    }

    fn seal(&mut self, t: PacketType, timestamp: u32, flags: u8, frame_code: u8, plain: &[u8]) -> Vec<u8> {
        let i = t as usize;
        self.seq[i] = self.seq[i].wrapping_add(1);
        let h = Header { ptype: t, ssrc: self.ssrc, seq: self.seq[i], timestamp, flags, frame_code };
        let mut out = [0u8; MAX_PACKET_BYTES];
        let n = self.tx.seal(&h, plain, &mut out).unwrap();
        out[..n].to_vec()
    }

    /// `recv_loop`와 같은 길이 거름(`n <= MAX_PACKET_BYTES`)을 거쳐 넣는다.
    fn feed(&self, pkt: &[u8], from: SocketAddr) {
        if pkt.len() <= MAX_PACKET_BYTES {
            let mut b = pkt.to_vec();
            self.s.handle_packet(&mut b, from);
        }
    }

    /// 엔진이 상대에게 보낸 패킷을 모두 읽어 Pong의 timestamp(= Ping 에코)를 돌려준다. 복호화 못 하는 것이 오면 실패.
    fn drain_replies(&self) -> Vec<u32> {
        let mut pongs = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok((n, _)) = self.sock.recv_from(&mut buf) {
            let pkt = &mut buf[..n];
            let h = Header::parse(pkt).expect("엔진이 보낸 패킷 헤더");
            self.rx.open(&h, pkt).expect("엔진이 보낸 패킷은 상대 키로 열린다");
            if h.ptype == PacketType::Pong {
                pongs.push(h.timestamp);
            }
        }
        pongs
    }
}

fn handle_packet_fuzz(seed: u64, n: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut rig = Rig::new();
    let mut ts: u32 = 10;
    // 받아들여야 하는 것
    let mut want_pongs = BTreeSet::new();
    let mut want_audio: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    // 받아들이면 안 되는 것의 원본(바꾸기·재전송용). Bye는 넣지 않는다(끝에서 따로).
    let mut ctrl_hist: Vec<Vec<u8>> = Vec::new();
    let mut any_hist: Vec<Vec<u8>> = Vec::new();
    let mut got_pongs = Vec::new();
    let mut got_audio = 0usize;
    let mut stray = SocketAddr::from(([127, 0, 0, 1], 9));
    for i in 0..n {
        ts = ts.wrapping_add(1);
        match rng.gen_range(0..100) {
            // 새 Ping(평문은 에코·손실·입력 버퍼 블록이나 아무 바이트) → Pong 하나
            0..=24 => {
                let plain = if rng.gen_bool(0.5) { random_bytes(&mut rng, 20) } else { ping_like(&mut rng) };
                let p = rig.seal(PacketType::Ping, ts, 0, 0, &plain);
                rig.feed(&p, rig.from);
                want_pongs.insert(ts);
                ctrl_hist.push(p.clone());
                any_hist.push(p);
            }
            // 새 Audio → 수신 큐에 그대로
            25..=44 => {
                let plain = random_bytes(&mut rng, 300);
                let fc = [1u8, 2, 4, 0, 9][rng.gen_range(0..5)];
                let p = rig.seal(PacketType::Audio, ts, rng.gen(), fc, &plain);
                rig.feed(&p, rig.from);
                want_audio.insert(rig.seq[PacketType::Audio as usize], plain);
                any_hist.push(p);
            }
            // 새 Pong(시계 블록이나 아무 평문)·남의 id를 실은 Hello/HelloAck — 관찰 결과는 없고 panic만 본다
            45..=49 => {
                let t = [PacketType::Pong, PacketType::Hello, PacketType::HelloAck][rng.gen_range(0..3)];
                let plain = if t == PacketType::Pong && rng.gen_bool(0.5) {
                    let mut c = vec![crate::net::CLOCK_TAG];
                    c.extend_from_slice(&rng.gen::<u64>().to_be_bytes());
                    c
                } else {
                    random_bytes(&mut rng, 24)
                };
                let p = rig.seal(t, rng.gen(), rng.gen(), 0, &plain);
                rig.feed(&p, rig.from);
                ctrl_hist.push(p.clone());
                any_hist.push(p);
            }
            // 원본을 바꾼 패킷(인증 실패) → 아무 효과 없어야 한다
            50..=74 if !any_hist.is_empty() => {
                let orig = &any_hist[rng.gen_range(0..any_hist.len())];
                let m = mutate(&mut rng, orig);
                rig.feed(&m, rig.from);
            }
            // 제어 패킷 그대로 재전송 → 재전송 방어로 버려야 한다
            75..=84 if !ctrl_hist.is_empty() => {
                let orig = ctrl_hist[rng.gen_range(0..ctrl_hist.len())].clone();
                rig.feed(&orig, rig.from);
            }
            // 무작위 바이트(가끔 그럴듯한 엔진/릴레이 헤더) — 출발 주소도 경로이거나 아무 곳
            85..=94 => {
                let mut b = random_bytes(&mut rng, 1300);
                if b.len() >= 4 && rng.gen_bool(0.6) {
                    b[0..2].copy_from_slice(&MAGIC);
                    b[2] = VERSION;
                    b[3] = rng.gen_range(0..12);
                    if b.len() >= 8 && rng.gen_bool(0.5) {
                        b[4..8].copy_from_slice(&rig.ssrc.to_be_bytes());
                    }
                }
                let from = if rng.gen_bool(0.7) { rig.from } else { stray };
                rig.feed(&b, from);
            }
            // 경로가 아닌 주소에서 온 바뀐 패킷
            _ if !any_hist.is_empty() => {
                stray.set_port(rng.gen_range(1..65535));
                let orig = &any_hist[rng.gen_range(0..any_hist.len())];
                let m = mutate(&mut rng, orig);
                rig.feed(&m, stray);
            }
            _ => {}
        }
        if i % 16 == 15 {
            got_pongs.extend(rig.drain_replies());
            got_audio += drain_audio(&mut rig.media, &mut want_audio, seed, i);
        }
    }
    // 루프백 UDP는 사실상 동기지만, 마지막 Pong이 아직 소켓에 없을 수 있어 기대한 수가 모일 때까지(최대 2초) 다시 읽는다(#190 리뷰 L5).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        got_pongs.extend(rig.drain_replies());
        if got_pongs.len() >= want_pongs.len() || Instant::now() >= deadline {
            break;
        }
        std::thread::yield_now();
    }
    got_audio += drain_audio(&mut rig.media, &mut want_audio, seed, n);
    assert!(want_audio.is_empty(), "seed {seed:#x}: 새 Audio {}개가 수신 큐에 없음", want_audio.len());
    let got: BTreeSet<u32> = got_pongs.iter().copied().collect();
    assert_eq!(got.len(), got_pongs.len(), "seed {seed:#x}: 같은 Ping에 Pong이 두 번(재전송 수락)");
    assert_eq!(got, want_pongs, "seed {seed:#x}: Pong을 받은 Ping 집합이 새 Ping 집합과 다르다");
    assert!(got_audio > 0 && !got.is_empty(), "시험이 수락 경로를 지나지 않음");
    assert!(!rig.peer.conn().bye, "seed {seed:#x}: 위조 Bye 수락");
    // 오라클 확인: 진짜 Bye는 받는다
    let bye = rig.seal(PacketType::Bye, ts, 0, 0, &[]);
    rig.feed(&mutate(&mut rng, &bye), rig.from);
    assert!(!rig.peer.conn().bye);
    rig.feed(&bye, rig.from);
    assert!(rig.peer.conn().bye, "진짜 Bye");
}

/// 수신 큐에서 꺼낸 Audio가 보낸 것과 바이트까지 같은지 보고 지운다. 보내지 않은(바꾼·무작위) 패킷이 나오면 실패.
fn drain_audio(media: &mut MediaRx, want: &mut BTreeMap<u32, Vec<u8>>, seed: u64, i: usize) -> usize {
    media.drain(|p| {
        let sent = want.remove(&p.hdr.seq);
        assert_eq!(sent.as_deref(), Some(p.payload()), "seed {seed:#x} 반복 {i}: 보내지 않은 Audio seq {} 수락", p.hdr.seq);
    })
}

/// Ping 평문 모양(에코·손실·입력 버퍼 블록을 무작위로 잇고 가끔 자른다).
fn ping_like(rng: &mut StdRng) -> Vec<u8> {
    let mut v = Vec::new();
    if rng.gen_bool(0.5) {
        v.push(peer::ECHO_TAG);
        v.extend((0..5).map(|_| rng.gen::<u8>()));
    }
    if rng.gen_bool(0.5) {
        v.push(0x4C);
        v.extend((0..4).map(|_| rng.gen::<u8>()));
    }
    if rng.gen_bool(0.5) {
        v.push(0x49);
        v.extend((0..2).map(|_| rng.gen::<u8>()));
    }
    if rng.gen_bool(0.3) {
        let n = rng.gen_range(0..=v.len());
        v.truncate(n);
    }
    v
}

#[test]
fn fuzz_handle_packet_accepts_only_fresh_authentic() {
    for s in 0..2u64 {
        handle_packet_fuzz(0xF022_0000 + s, iters(3_000));
    }
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test -p band-engine --release --lib fuzz -- --ignored"]
fn fuzz_handle_packet_accepts_only_fresh_authentic_long() {
    if !long_enabled("fuzz_handle_packet_accepts_only_fresh_authentic_long") {
        return;
    }
    for s in 0..8u64 {
        handle_packet_fuzz(0xF022_1000 + s, iters(100_000));
    }
}

/// 암호 프레임: 봉인한 패킷을 어떻게 바꿔도 열리지 않고, 무작위 바이트·짧은 버퍼에서 panic 없다.
fn crypto_fuzz(seed: u64, n: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let c = PacketCipher::new(&rng.gen());
    let mut out = [0u8; MAX_PACKET_BYTES];
    for _ in 0..n {
        let plain = random_bytes(&mut rng, MAX_PACKET_BYTES - HEADER_BYTES - TAG_BYTES);
        let h = Header { ptype: PacketType::Audio, ssrc: rng.gen(), seq: rng.gen(), timestamp: rng.gen(), flags: rng.gen(), frame_code: rng.gen() };
        let len = c.seal(&h, &plain, &mut out).unwrap();
        let orig = out[..len].to_vec();
        let mut ok = orig.clone();
        assert_eq!(c.open(&h, &mut ok).map(|r| ok[r].to_vec()), Some(plain));
        let mut m = mutate(&mut rng, &orig);
        // 헤더를 바꿨으면 바뀐 헤더로(수신 경로와 같게), 헤더가 깨졌으면 원래 헤더로 연다 — 어느 쪽이든 실패해야 한다
        let hh = Header::parse(&m).unwrap_or(h);
        assert!(c.open(&hh, &mut m).is_none(), "seed {seed:#x}: 바뀐 패킷이 열림");
        let mut junk = random_bytes(&mut rng, 80);
        assert!(c.open(&h, &mut junk).is_none());
    }
}

#[test]
fn fuzz_crypto_open_rejects_mutations() {
    crypto_fuzz(0xC0DE_0001, iters(5_000));
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test -p band-engine --release --lib fuzz -- --ignored"]
fn fuzz_crypto_open_rejects_mutations_long() {
    if !long_enabled("fuzz_crypto_open_rejects_mutations_long") {
        return;
    }
    crypto_fuzz(0xC0DE_1001, iters(1_000_000));
}

/// 순수 파서 묶음: STUN 응답, Ping 평문 블록, Pong 시계, 에코 상태, 릴레이 BindAck·토큰.
fn parsers_fuzz(seed: u64, n: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let txn: [u8; 12] = rng.gen();
    // STUN: 올바른 응답 하나를 바꿔 가며 + 무작위
    let mut good = vec![0x01, 0x01, 0x00, 0x18, 0x21, 0x12, 0xa4, 0x42];
    good.extend_from_slice(&txn);
    good.extend_from_slice(&[0x00, 0x01, 0x00, 0x08, 0x00, 0x01, 0x1f, 0x40, 10, 0, 0, 1]);
    good.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x3e, 0x52, 0xe1, 0x12, 0xa4, 0x43]);
    assert!(stun::parse_response(&good, &txn).is_some());
    let mut echo = EchoState::default();
    let tok = RelayToken { bytes: vec![1; 60], expires_at: u64::MAX, ssrc: 7, bind_key: None };
    let mut relay = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
    let t0 = Instant::now();
    let mut bind = [0u8; crate::net::relay::BIND_MAX_BYTES];
    for i in 0..n {
        let b = if rng.gen_bool(0.5) { mutate(&mut rng, &good) } else { random_bytes(&mut rng, 600) };
        if stun::parse_response(&b, &txn).is_some() {
            assert_eq!(&b[0..2], &[1, 1]);
            assert_eq!(&b[8..20], &txn, "다른 트랜잭션 수락");
        }
        let p = if rng.gen_bool(0.5) { ping_like(&mut rng) } else { random_bytes(&mut rng, 20) };
        if let Some(r) = LossReport::parse_ping(&p) {
            assert!((0.0..=100.0).contains(&r.loss_pct) && (0.0..=100.0).contains(&r.conceal_pct), "{r:?}");
        }
        let _ = peer::parse_ping_input_buffer(&p);
        let _ = parse_clock_payload(&p);
        echo.on_ping(&p, t0);
        let _ = echo.ping_payload(t0);
        // 릴레이 BindAck: 길이 50 근처 무작위. 내 ssrc가 아니면 상태가 바뀌지 않는다.
        let n_bind = relay.write_bind(t0, 7, i as u32, &mut bind);
        let mut ack = random_bytes(&mut rng, 70);
        if rng.gen_bool(0.7) {
            ack.resize(50, 0);
            ack[0..4].copy_from_slice(&[MAGIC[0], MAGIC[1], VERSION, 9]);
            if rng.gen_bool(0.7) {
                ack[4..16].copy_from_slice(&bind[4..16]);
            }
        }
        let before = (relay.state, relay.reject, relay.observed, relay.rtt_ms);
        let act = relay.on_ack(&ack, 7, t0, i as u32);
        let mine = ack.len() == 50 && ack[0..4] == [MAGIC[0], MAGIC[1], VERSION, 9] && ack[4..8] == 7u32.to_be_bytes();
        if !mine {
            assert_eq!(act, AckAction::None);
            assert_eq!((relay.state, relay.reject, relay.observed, relay.rtt_ms), before, "남의/깨진 Ack로 상태 변경");
        }
        assert!(n_bind <= bind.len());
        // 릴레이 토큰(서버가 준 바이트·base64)
        let tb = if rng.gen_bool(0.5) { random_bytes(&mut rng, 260) } else { token_like(&mut rng) };
        if let Ok(t) = RelayToken::from_bytes(tb.clone(), 7) {
            assert_eq!(t.ssrc, 7);
            assert!(tb.len() >= 49 && tb[0] == 1);
        }
        let s: String = (0..rng.gen_range(0..120)).map(|_| rng.gen_range(0x20u8..0x7f) as char).collect();
        let _ = RelayToken::parse(&s, 7);
        let _ = crate::net::relay::parse_bind_key(&s);
    }
}

/// 형식이 거의 맞는 릴레이 토큰(버전 1, ssrc 7, 길이 필드는 가끔 틀림).
fn token_like(rng: &mut StdRng) -> Vec<u8> {
    let room = rng.gen_range(0..140usize);
    let member = rng.gen_range(0..70usize);
    let mut v = vec![1u8];
    v.extend_from_slice(&rng.gen::<u64>().to_be_bytes());
    v.extend_from_slice(&7u32.to_be_bytes());
    v.push(room as u8);
    v.extend((0..room).map(|_| b'r'));
    v.push(member as u8);
    v.extend((0..member).map(|_| b'm'));
    v.extend((0..32).map(|_| rng.gen::<u8>()));
    if rng.gen_bool(0.3) {
        let n = rng.gen_range(0..=v.len());
        v.truncate(n);
    }
    v
}

#[test]
fn fuzz_pure_parsers() {
    parsers_fuzz(0x9A25_0001, iters(20_000));
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test -p band-engine --release --lib fuzz -- --ignored"]
fn fuzz_pure_parsers_long() {
    if !long_enabled("fuzz_pure_parsers_long") {
        return;
    }
    parsers_fuzz(0x9A25_1001, iters(2_000_000));
}

/// 오래 걸리지 않는다(무한 루프·이상 경로 감지): 변조 패킷 하나 처리가 50ms를 넘지 않는다(최댓값 — 느린 CI 여유 포함).
#[test]
fn fuzz_handle_packet_is_fast() {
    let mut rig = Rig::new();
    let mut rng = StdRng::seed_from_u64(0xFA57);
    let p = rig.seal(PacketType::Ping, 1, 0, 0, &[]);
    let mut worst = Duration::ZERO;
    for _ in 0..2_000 {
        let m = mutate(&mut rng, &p);
        let t = Instant::now();
        rig.feed(&m, rig.from);
        worst = worst.max(t.elapsed());
    }
    assert!(worst < Duration::from_millis(50), "가장 느린 패킷 {worst:?}");
}

/// Pong 시계(상대가 정하는 u64)가 극단값이어도: 오프셋 추정은 유한하고, 기준 피어로 따르는 메트로놈 스케줄러(`tick`의 다음 박 찾기
/// 반복)가 끝난다. 계획의 `start_us`도 검증(유한·0 이상)만 거친 값이라 아주 큰 값을 섞는다.
#[test]
fn fuzz_hostile_clock_does_not_stall_metronome() {
    use crate::engine::metronome::{BeatScheduler, ClickShared, MetronomePlan, OutClock};
    let mut rng = StdRng::seed_from_u64(0xC10C);
    let mut ran = 0;
    let extreme = [0u64, 1, u64::MAX, u64::MAX / 2, 1 << 53, (1 << 53) + 1, 1_000_000_000_000];
    for i in 0..iters(2_000) {
        let mut cs = band_dsp::clock::ClockSync::new();
        let base = rng.gen_range(1.0e6..1.0e9);
        for j in 0..rng.gen_range(1..12) {
            let remote = if rng.gen_bool(0.5) { extreme[rng.gen_range(0..extreme.len())] } else { rng.gen() };
            let t0 = base + j as f64 * 400_000.0;
            cs.on_sample(t0, t0 + rng.gen_range(0.0..300_000.0), remote as f64);
        }
        let at = base + 5e6;
        let Some(est) = cs.estimate(at) else { continue };
        ran += 1;
        assert!(est.offset_us.is_finite() && est.drift_ppm.is_finite(), "반복 {i}: {est:?}");
        let start_us = match rng.gen_range(0..4) {
            0 => 0.0,
            1 => rng.gen_range(0.0..1e12),
            2 => 10f64.powi(rng.gen_range(13..300)),
            _ => (u64::MAX as f64) * rng.gen_range(0.0..2.0),
        };
        let plan = MetronomePlan { running: true, bpm: rng.gen_range(20.0..=300.0), beats_per_bar: rng.gen_range(1..=16), start_us, ref_peer_id: None };
        let clock = OutClock { dev_now_us: at, played: rng.gen_range(0..1u64 << 40), rate: 48_000.0, burst: 128, extra_out_us: 0.0 };
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let t = Instant::now();
        for g in 0..3 {
            let _ = s.tick(1 + g / 2, &plan, Some(est.offset_us), &clock, &sh);
        }
        assert!(t.elapsed() < Duration::from_millis(200), "반복 {i}: tick이 {:?} 걸림(offset {}, start {start_us})", t.elapsed(), est.offset_us);
    }
    assert!(ran > 0, "추정이 한 번도 나오지 않음");
}
