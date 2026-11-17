//! 수신 seq 창 회귀 시험(#190 발견 L1·L2, 백로그 P2-16c).
//!
//! - L1: 제어 seq를 출발 주소 확인(`accept_rx`) 전에 소모하면, 경로가 아닌 주소에서 먼저 온 사본이 진짜 Ping을 재전송으로 막았다.
//! - L2: Audio에는 net 쪽 재전송 창이 없어 사본·재생 공격 패킷이 수신 큐에 들어가고(링 128칸 차지), 지터 슬롯 창(1024)보다 옛
//!   사본은 늦은 패킷으로 세어졌으며, 재전송본이 생존 갱신 같은 `accept_rx` 부수 효과를 일으켰다.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use band_dsp::config::MAX_PACKET_BYTES;
use band_dsp::media_link::{media_link, MediaRx, PeerKey};
use band_dsp::packet::{path_tag, Header, PacketType};

use crate::net::crypto::{KeyPair, PacketCipher};
use crate::net::peer::{AudioSeqWindow, Candidate, CandidateKind, LocalDescriptor, Peer, AUDIO_REPLAY_WINDOW};
use crate::net::{Net, NetOptions, NetShared};

/// 엔진 하나(스레드 정지) + 키를 아는 상대 하나. 엔진의 응답은 상대 소켓(피어 후보 주소)으로 실제로 온다.
struct Rig {
    net: Net,
    s: Arc<NetShared>,
    sock: UdpSocket,
    from: SocketAddr,
    remote: KeyPair,
    tx: PacketCipher,
    rx: PacketCipher,
    ssrc: u32,
    seq: [u32; 7],
}

impl Rig {
    fn new() -> (Self, MediaRx) {
        let opts = NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
        let mut net = Net::start("me".into(), &opts).unwrap();
        net.stop();
        let s = net.shared().clone();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_nonblocking(true).unwrap();
        let from = sock.local_addr().unwrap();
        let remote = KeyPair::generate();
        let ssrc = 0x5EED_0002;
        let (tx, rx) = remote.pair_ciphers(ssrc, &s.keys.public_bytes(), s.ssrc).unwrap();
        let mut rig = Self { net, s, sock, from, remote, tx, rx, ssrc, seq: [1000; 7] };
        let media = rig.add();
        (rig, media)
    }

    fn desc(&self) -> LocalDescriptor {
        LocalDescriptor {
            ssrc: self.ssrc,
            key: LocalDescriptor::encode_key(&self.remote.public_bytes()),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: self.from.to_string() }],
        }
    }

    /// 피어 추가(같은 id면 교체) + 핸드셰이크(경로 확정).
    fn add(&mut self) -> MediaRx {
        let (mtx, media) = media_link(PeerKey::from_raw(1));
        self.net.add_peer("peer".into(), &self.desc(), mtx).unwrap();
        let hello = self.seal(PacketType::Hello, 1, path_tag(false), b"peer");
        self.feed(&hello, self.from);
        assert!(self.peer().conn().path.is_some(), "핸드셰이크 실패 — 시험 설정 오류");
        self.replies();
        media
    }

    fn peer(&self) -> Arc<Peer> {
        self.net.peers().into_iter().next().unwrap()
    }

    fn seal_seq(&self, t: PacketType, seq: u32, timestamp: u32, flags: u8, plain: &[u8]) -> Vec<u8> {
        let h = Header { ptype: t, ssrc: self.ssrc, seq, timestamp, flags, frame_code: 2 };
        let mut out = [0u8; MAX_PACKET_BYTES];
        let n = self.tx.seal(&h, plain, &mut out).unwrap();
        out[..n].to_vec()
    }

    fn seal(&mut self, t: PacketType, timestamp: u32, flags: u8, plain: &[u8]) -> Vec<u8> {
        let i = t as usize;
        self.seq[i] = self.seq[i].wrapping_add(1);
        self.seal_seq(t, self.seq[i], timestamp, flags, plain)
    }

    fn audio(&self, seq: u32) -> Vec<u8> {
        self.seal_seq(PacketType::Audio, seq, seq.wrapping_mul(240), 0, &seq.to_be_bytes())
    }

    fn feed(&self, pkt: &[u8], from: SocketAddr) {
        let mut b = pkt.to_vec();
        self.s.handle_packet(&mut b, from);
    }

    /// 엔진이 상대에게 보낸 Pong들의 timestamp(= Ping 에코).
    fn replies(&self) -> Vec<u32> {
        let mut pongs = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok((n, _)) = self.sock.recv_from(&mut buf) {
            let pkt = &mut buf[..n];
            let h = Header::parse(pkt).unwrap();
            self.rx.open(&h, pkt).unwrap();
            if h.ptype == PacketType::Pong {
                pongs.push(h.timestamp);
            }
        }
        pongs
    }
}

fn drained(media: &mut MediaRx) -> Vec<u32> {
    let mut v = Vec::new();
    media.drain(|p| v.push(p.hdr.seq));
    v
}

/// L1: 경로가 아닌 주소에서 먼저 온 진짜 Ping 사본(엿본 뒤 다른 출발지로 넣음, 또는 NAT 재바인딩 직전 주소)이 seq를 소모해
/// 늦게 온 진짜 Ping이 재전송으로 버려지면 안 된다. Bye·Pong도 같은 길이다.
#[test]
fn ctrl_copy_from_foreign_address_does_not_burn_seq() {
    let (mut rig, _media) = Rig::new();
    let ping = rig.seal(PacketType::Ping, 77, 0, &[]);
    let stray = SocketAddr::from(([127, 0, 0, 1], 9));
    rig.feed(&ping, stray);
    assert!(rig.replies().is_empty(), "경로가 아닌 주소의 Ping에는 답하지 않는다");
    rig.feed(&ping, rig.from);
    assert_eq!(rig.replies(), [77], "진짜 Ping이 재전송으로 버려졌다");
    // 그 뒤 같은 Ping 재전송은 버린다(창은 여전히 동작)
    rig.feed(&ping, rig.from);
    assert!(rig.replies().is_empty(), "재전송 Ping 수락");

    let bye = rig.seal(PacketType::Bye, 0, 0, &[]);
    rig.feed(&bye, stray);
    assert!(!rig.peer().conn().bye);
    rig.feed(&bye, rig.from);
    assert!(rig.peer().conn().bye, "진짜 Bye가 재전송으로 버려졌다");
}

/// L2: Audio 사본(망 중복·두 경로 유예·재생 공격)은 수신 큐에 들어가지 않는다. 손실로 건너뛴 seq의 진짜 늦은 패킷은 창 안이면 받는다.
#[test]
fn audio_duplicates_and_replays_never_reach_queue() {
    let (rig, mut media) = Rig::new();
    let base = 50_000u32;
    for s in [base, base + 1, base + 3, base + 4] {
        rig.feed(&rig.audio(s), rig.from);
    }
    rig.feed(&rig.audio(base + 1), rig.from); // 사본
    rig.feed(&rig.audio(base + 4), rig.from); // 사본(최고 seq)
    rig.feed(&rig.audio(base + 2), rig.from); // 진짜 늦은 패킷
    rig.feed(&rig.audio(base + 2), rig.from); // 그 사본
    assert_eq!(drained(&mut media), [base, base + 1, base + 3, base + 4, base + 2]);

    // 재생 홍수: 받은 패킷 사본 300개가 수신 링(128칸)을 채우지 않는다 → 진짜 새 패킷이 버려지지 않는다
    let captured: Vec<Vec<u8>> = (0..5).map(|k| rig.audio(base + k)).collect();
    for i in 0..300 {
        rig.feed(&captured[i % 5], rig.from);
    }
    rig.feed(&rig.audio(base + 5), rig.from);
    assert_eq!(drained(&mut media), [base + 5], "재생 사본이 수신 큐에 들어갔다");
}

/// L2(P2-16c): 창(지터 슬롯 창과 같은 1024)보다 옛 패킷은 사본이든 늦은 패킷이든 버린다 — 지터 버퍼가 사본인지 가릴 수 없어
/// 늦은 패킷으로 세던 것. 창 바로 안쪽의 진짜 늦은 패킷은 받는다.
#[test]
fn audio_older_than_window_is_dropped() {
    let (rig, mut media) = Rig::new();
    let base = 7u32;
    rig.feed(&rig.audio(base), rig.from);
    let top = base + AUDIO_REPLAY_WINDOW + 10;
    rig.feed(&rig.audio(top), rig.from);
    rig.feed(&rig.audio(base), rig.from); // 2.56초 넘게 옛 사본
    rig.feed(&rig.audio(base + 1), rig.from); // 창 밖의 늦은 패킷
    rig.feed(&rig.audio(top - AUDIO_REPLAY_WINDOW + 1), rig.from); // 창 맨 끝 안쪽의 늦은 패킷
    assert_eq!(drained(&mut media), [base, top, top - AUDIO_REPLAY_WINDOW + 1]);
}

/// 재전송본은 `accept_rx`의 부수 효과(생존 갱신)를 일으키지 않는다 — 엿본 패킷 재생으로 사라진 피어를 살아 있게 보이지 않게.
#[test]
fn replayed_audio_does_not_refresh_liveness() {
    let (rig, mut media) = Rig::new();
    let p = rig.audio(900);
    rig.feed(&p, rig.from);
    assert!(rig.peer().conn().last_rx.is_some());
    rig.peer().conn().last_rx = None;
    rig.feed(&p, rig.from);
    assert!(rig.peer().conn().last_rx.is_none(), "재전송 Audio가 생존을 갱신했다");
    assert_eq!(drained(&mut media), [900]);
}

/// seq 랩어라운드를 지나도 이어서 받고, 랩 앞뒤 사본은 버린다.
#[test]
fn audio_window_across_seq_wrap() {
    let (rig, mut media) = Rig::new();
    let seqs = [u32::MAX - 2, u32::MAX - 1, u32::MAX, 0, 1, 2];
    for s in seqs {
        rig.feed(&rig.audio(s), rig.from);
    }
    for s in [u32::MAX - 1, 0, 2] {
        rig.feed(&rig.audio(s), rig.from);
    }
    assert_eq!(drained(&mut media), seqs);
}

/// 피어 교체: 같은 키(같은 세션의 descriptor 재전송)면 창을 이어받아 교체 전에 엿본 사본을 버리고, 새 키(상대 재시작)면 창이
/// 새로 시작해 아무 seq에서나 받는다(송신 카운터가 무작위에서 다시 시작).
#[test]
fn audio_window_inherited_on_same_key_and_reset_on_new_key() {
    let (mut rig, mut media) = Rig::new();
    let old = rig.audio(4_000);
    rig.feed(&old, rig.from);
    assert_eq!(drained(&mut media), [4_000]);
    let mut media = rig.add(); // 같은 키로 교체
    rig.feed(&old, rig.from);
    rig.feed(&rig.audio(4_001), rig.from);
    assert_eq!(drained(&mut media), [4_001], "교체 전 사본 수락");

    // 상대 재시작: 새 키·새 송신 카운터(옛 창보다 한참 옛 seq)
    rig.remote = KeyPair::generate();
    let (tx, rx) = rig.remote.pair_ciphers(rig.ssrc, &rig.s.keys.public_bytes(), rig.s.ssrc).unwrap();
    (rig.tx, rig.rx) = (tx, rx);
    rig.seq = [3; 7];
    let mut media = rig.add();
    rig.feed(&rig.audio(10), rig.from);
    assert_eq!(drained(&mut media), [10], "새 키인데 옛 창이 남았다");
}

/// 창 단위 성질: 판정은 상태를 바꾸지 않고, 큰 앞 점프는 창을 비운다(옛 비트가 새 seq를 사본으로 오판하지 않게).
#[test]
fn audio_seq_window_unit() {
    let mut w = AudioSeqWindow::default();
    assert!(w.fresh(5));
    assert!(w.fresh(5), "fresh는 상태를 바꾸지 않는다");
    w.commit(5);
    assert!(!w.fresh(5));
    assert!(w.fresh(4) && w.fresh(6));
    // 창 크기만큼 앞으로: 5는 창 밖, 5 + 창은 새 seq(같은 비트 자리지만 비워져야 한다)
    w.commit(5 + AUDIO_REPLAY_WINDOW);
    assert!(!w.fresh(5));
    assert!(!w.fresh(5 + AUDIO_REPLAY_WINDOW));
    assert!(w.fresh(6 + AUDIO_REPLAY_WINDOW), "비트가 남아 새 seq를 막음");
    // 창보다 큰 점프
    w.commit(1 << 20);
    for s in [(1u32 << 20) - 1, (1 << 20) - 500, (1 << 20) - (AUDIO_REPLAY_WINDOW - 1)] {
        assert!(w.fresh(s), "{s}: 받은 적 없는 창 안 seq");
    }
    assert!(!w.fresh((1 << 20) - AUDIO_REPLAY_WINDOW));
    // 절반 넘게 떨어진 seq(i32 음수 끝)는 옛 것으로 본다
    assert!(!w.fresh((1u32 << 20).wrapping_add(1 << 31)));
}

/// #193 리뷰 L1: 재전송 창에서 버린 패킷을 센다 — 사본·재전송(`Dup`)과 Audio 창보다 옛 것(`Stale`). `PeerStats`로 나간다.
#[test]
fn seq_window_drops_are_counted() {
    let (mut rig, mut media) = Rig::new();
    let counts = |rig: &Rig| {
        let ns = rig.peer().net_stats(std::time::Instant::now());
        (ns.rx_dup_dropped, ns.rx_stale_dropped)
    };
    assert_eq!(counts(&rig), (0, 0));
    rig.feed(&rig.audio(100), rig.from);
    rig.feed(&rig.audio(100), rig.from); // 사본
    rig.feed(&rig.audio(100 + AUDIO_REPLAY_WINDOW), rig.from);
    rig.feed(&rig.audio(99), rig.from); // 창 밖(옛)
    let ping = rig.seal(PacketType::Ping, 5, 0, &[]);
    rig.feed(&ping, rig.from);
    rig.feed(&ping, rig.from); // 제어 재전송
    let stray = SocketAddr::from(([127, 0, 0, 1], 9));
    rig.feed(&rig.audio(101 + AUDIO_REPLAY_WINDOW), stray); // 출발지 거부는 재전송 창 폐기가 아니다
    assert_eq!(counts(&rig), (2, 1));
    assert_eq!(drained(&mut media), [100, 100 + AUDIO_REPLAY_WINDOW]);
}

/// #193 리뷰 L2: 수신 쪽에서 같은 키 피어를 **제거한 뒤 다시 추가**해도(교체가 아님) 창을 이어받아, 그 전에 엿본 사본을 받지 않는다.
#[test]
fn audio_window_survives_remove_then_readd_same_key() {
    let (mut rig, mut media) = Rig::new();
    let old = rig.audio(8_000);
    rig.feed(&old, rig.from);
    assert_eq!(drained(&mut media), [8_000]);
    let ping = rig.seal(PacketType::Ping, 9, 0, &[]);
    rig.feed(&ping, rig.from);
    rig.replies();
    rig.net.remove_peer("peer");
    let mut media = rig.add();
    rig.feed(&old, rig.from);
    rig.feed(&ping, rig.from);
    assert!(rig.replies().is_empty(), "제거 전 Ping 재전송 수락");
    rig.feed(&rig.audio(8_001), rig.from);
    assert_eq!(drained(&mut media), [8_001], "제거 전 사본 수락");
}

/// 창 판정을 단순 모델(받은 seq 집합 + 최고 seq)과 무작위로 대조한다. 창 안 점프의 워드 단위 비우기(#193 리뷰 L3)가 경계
/// (워드 경계·원형 끝·정확히 창 크기)에서 비트를 남기거나 더 지우지 않는지 본다.
#[test]
fn audio_seq_window_matches_model() {
    use rand::{Rng, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(0x5EA1);
    for round in 0..200 {
        let mut w = AudioSeqWindow::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut top: Option<u32> = None;
        let mut s: u32 = if round % 2 == 0 { rng.gen() } else { u32::MAX - 3000 };
        for _ in 0..3000 {
            s = match rng.gen_range(0..10) {
                0 => s.wrapping_add(rng.gen_range(1..=2 * AUDIO_REPLAY_WINDOW)),
                1 => s.wrapping_sub(rng.gen_range(0..=AUDIO_REPLAY_WINDOW + 5)),
                2 => s.wrapping_add([63, 64, 65, 127, 128, AUDIO_REPLAY_WINDOW - 1, AUDIO_REPLAY_WINDOW][rng.gen_range(0..7)]),
                _ => s.wrapping_add(rng.gen_range(0..3)),
            };
            let model_fresh = match top {
                None => true,
                Some(h) => {
                    let d = s.wrapping_sub(h) as i32;
                    d > 0 || (d.unsigned_abs() < AUDIO_REPLAY_WINDOW && !seen.contains(&s))
                }
            };
            assert_eq!(w.fresh(s), model_fresh, "round {round} seq {s} top {top:?}");
            if model_fresh {
                w.commit(s);
                seen.insert(s);
                if top.is_none_or(|h| (s.wrapping_sub(h) as i32) > 0) {
                    top = Some(s);
                }
                // 모델은 창 안의 것만 기억하면 된다(밖은 Stale로 판정되므로)
                let h = top.unwrap();
                seen.retain(|&x| h.wrapping_sub(x) < AUDIO_REPLAY_WINDOW);
            }
        }
    }
}
