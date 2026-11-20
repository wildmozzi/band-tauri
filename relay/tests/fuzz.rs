//! 릴레이 입력 퍼징(P2 발굴 — 네트워크 파서). Windows에서 cargo-fuzz(libFuzzer)를 쓰기 어려워 시드 고정 무작위 입력으로 대신한다.
//!
//! 기본 `cargo test`는 짧게 돈다. 긴 판: `BAND_FUZZ_LONG=1 cargo test --release --test fuzz -- --ignored`(반복 수는 `BAND_FUZZ_ITERS`).
//!
//! 방에 정상 등록한 두 멤버(A, B) 옆에서 무작위 바이트·바꾼 Bind/RelayData·다른 주소에서 그대로 다시 보낸 Bind(쿠키 포함)를
//! 넣는다. 판정: panic 없음, 등록은 A·B뿐(새 주소 등록 0 — 재생·위조로 경로 가로채기 없음), Ack는 보낸 주소로만·요청보다 작게
//! (반사·증폭 없음), 전달은 등록된 출발지 → 같은 방의 다른 멤버로만, 내부 ssrc는 출발지 등록과 같음.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_relay::packet::{ack_cookie, write_relay_bind_mac, write_relay_data, BindStatus, COOKIE_BYTES, MAGIC, VERSION};
use band_relay::token::{self, Claims};
use band_relay::{Limits, Outcome, Relay, Stats};

const SECRET: &[u8] = b"fuzz-secret-0123456789abcdef-0123456789";
const NOW_UNIX: u64 = 1_800_000_000;

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

/// splitmix64(의존 없이 시드 고정).
struct Rng(u64);

impl Rng {
    fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.u64() % n as u64) as usize
    }
    fn byte(&mut self) -> u8 {
        self.u64() as u8
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.byte()).collect()
    }
    fn addr(&mut self) -> SocketAddr {
        let ip = if self.chance(50) { [10, 0, 0, self.byte()] } else { (self.u64() as u32).to_be_bytes() };
        SocketAddr::from((ip, 1 + self.below(65534) as u16))
    }
}

fn mutate(r: &mut Rng, orig: &[u8]) -> Vec<u8> {
    let mut b = orig.to_vec();
    for _ in 0..1 + r.below(4) {
        match r.below(5) {
            0 | 1 if !b.is_empty() => {
                let i = r.below(b.len());
                b[i] ^= 1 << r.below(8);
            }
            2 if !b.is_empty() => {
                let i = r.below(b.len());
                b[i] = r.byte();
            }
            3 => {
                let n = r.below(b.len() + 1);
                b.truncate(n);
            }
            _ => {
                for _ in 0..1 + r.below(40) {
                    b.push(r.byte());
                }
            }
        }
    }
    b
}

struct Member {
    addr: SocketAddr,
    ssrc: u32,
    tok: Vec<u8>,
    /// 쿠키까지 붙은(등록에 쓴) 마지막 Bind.
    bind: Vec<u8>,
}

fn bind_pkt(m: &Member, seq: u32, cookie: Option<&[u8; COOKIE_BYTES]>) -> Vec<u8> {
    let mut b = Vec::new();
    write_relay_bind_mac(m.ssrc, seq, 99, &m.tok, cookie, &token::bind_key(SECRET, &m.tok), &mut b);
    b
}

fn register(r: &mut Relay, now: Instant, m: &mut Member) {
    let b = bind_pkt(m, 1, None);
    assert_eq!(r.handle(now, NOW_UNIX, m.addr, &b), Outcome::Ack { to: m.addr });
    assert_eq!(r.ack()[16], BindStatus::CookieRequired as u8);
    let cookie = ack_cookie(r.ack());
    m.bind = bind_pkt(m, 2, Some(&cookie));
    assert_eq!(r.handle(now, NOW_UNIX, m.addr, &m.bind), Outcome::Ack { to: m.addr });
    assert_eq!(r.ack()[16], BindStatus::Ok as u8);
}

fn engine_inner(r: &mut Rng, ssrc: u32) -> Vec<u8> {
    let mut p = vec![0u8; 18 + r.below(1300) + 16];
    p[0..4].copy_from_slice(&[MAGIC[0], MAGIC[1], VERSION, 1 + r.below(7) as u8]);
    p[4..8].copy_from_slice(&ssrc.to_be_bytes());
    p
}

fn relay_fuzz(seed: u64, n: usize, require_bind_mac: bool) {
    let mut r = Rng(seed);
    let limits = Limits { require_bind_mac, ..Limits::default() };
    let mut relay = Relay::new(SECRET.to_vec(), limits, Arc::new(Stats::default()));
    let t0 = Instant::now();
    let mk = |addr: SocketAddr, ssrc: u32, client: &str| Member {
        addr,
        ssrc,
        tok: token::sign(SECRET, &Claims { expires_at: NOW_UNIX + 3600, ssrc, room: "room-1", client }),
        bind: Vec::new(),
    };
    let mut ms = [mk("10.0.0.1:4000".parse().unwrap(), 0xA, "a"), mk("10.0.0.2:4000".parse().unwrap(), 0xB, "b")];
    for m in ms.iter_mut() {
        register(&mut relay, t0, m);
    }
    let (mut forwards, mut acks) = (0usize, 0usize);
    for i in 0..n {
        // 시간은 1ms씩, 25초마다 A·B keepalive(등록 TTL 30초)
        let now = t0 + Duration::from_millis(i as u64);
        if i % 25_000 == 24_999 {
            for m in &ms {
                assert_eq!(relay.handle(now, NOW_UNIX, m.addr, &m.bind), Outcome::Ack { to: m.addr });
                assert_eq!(relay.ack()[16], BindStatus::Ok as u8, "keepalive");
            }
        }
        let k = r.below(2);
        let (from, buf) = match r.below(100) {
            // 무작위 바이트(절반은 릴레이 헤더 모양), 아무 주소
            0..=24 => {
                let mut b = r.bytes(1400);
                if b.len() >= 4 && r.chance(60) {
                    b[0..3].copy_from_slice(&[MAGIC[0], MAGIC[1], VERSION]);
                    b[3] = 6 + r.below(5) as u8;
                }
                let from = if r.chance(30) { ms[k].addr } else { r.addr() };
                (from, b)
            }
            // 바꾼 Bind: 멤버 주소에서 또는 아무 주소에서
            25..=49 => {
                let from = if r.chance(50) { ms[k].addr } else { r.addr() };
                (from, mutate(&mut r, &ms[k].bind))
            }
            // 쿠키·MAC까지 맞는 Bind를 그대로 다른 주소에서(캡처 재생) → 쿠키 요청까지만
            50..=59 => (r.addr(), ms[k].bind.clone()),
            // RelayData: 정상 모양(대상은 상대·자기·아무 ssrc, 내부 ssrc는 자기·남), 바꾼 것, 남의 주소에서
            _ => {
                let dest = [ms[1 - k].ssrc, ms[k].ssrc, r.u64() as u32][r.below(3)];
                let inner_ssrc = if r.chance(80) { ms[k].ssrc } else { r.u64() as u32 };
                let mut b = Vec::new();
                write_relay_data(dest, &engine_inner(&mut r, inner_ssrc), &mut b);
                if r.chance(30) {
                    b = mutate(&mut r, &b);
                }
                let from = if r.chance(85) { ms[k].addr } else { r.addr() };
                (from, b)
            }
        };
        let out = relay.handle(now, NOW_UNIX, from, &buf);
        match out {
            Outcome::Ack { to } => {
                acks += 1;
                assert_eq!(to, from, "seed {seed:#x} 반복 {i}: 보낸 주소가 아닌 곳으로 Ack(반사)");
                assert!(relay.ack().len() < buf.len(), "seed {seed:#x} 반복 {i}: Ack가 요청보다 크다(증폭)");
                let st = relay.ack()[16];
                if !ms.iter().any(|m| m.addr == from) {
                    assert_eq!(st, BindStatus::CookieRequired as u8, "seed {seed:#x} 반복 {i}: 새 주소 {from}가 쿠키 없이 상태 {st}");
                }
            }
            Outcome::Forward { to, offset } => {
                forwards += 1;
                let src = ms.iter().position(|m| m.addr == from).unwrap_or_else(|| panic!("seed {seed:#x} 반복 {i}: 미등록 {from}의 패킷 전달"));
                assert_eq!(offset, 8);
                assert_eq!(to, ms[1 - src].addr, "seed {seed:#x} 반복 {i}: 같은 방 다른 멤버가 아닌 곳으로 전달");
                let inner = &buf[offset..];
                assert!((34..=1280).contains(&inner.len()));
                assert_eq!(&inner[4..8], &ms[src].ssrc.to_be_bytes(), "내부 ssrc 위장 전달");
                assert_eq!(&buf[4..8], &ms[1 - src].ssrc.to_be_bytes());
            }
            // 무작위·바꾼 입력(type 10 포함)이 서명된 회수로 통하면 안 된다
            Outcome::Handled => panic!("seed {seed:#x} 반복 {i}: 위조 RelayRevoke를 받음 — 입력 {buf:02x?}"),
            Outcome::Drop(_) => {}
        }
        assert_eq!(relay.registration_count(), 2, "seed {seed:#x} 반복 {i}: 등록 수 변화(가로채기·해제) — 입력 {buf:02x?} from {from}");
    }
    assert!(forwards > 0 && acks > 0, "시험이 전달·Ack 경로를 지나지 않음 ({forwards}, {acks})");
    // A·B 사이 정상 전달은 여전히 된다
    let mut b = Vec::new();
    write_relay_data(ms[1].ssrc, &engine_inner(&mut r, ms[0].ssrc), &mut b);
    b.truncate(8 + 100);
    let end = t0 + Duration::from_millis(n as u64 + 1000);
    assert_eq!(relay.handle(end, NOW_UNIX, ms[0].addr, &b), Outcome::Forward { to: ms[1].addr, offset: 8 });
}

#[test]
fn fuzz_relay_handle() {
    relay_fuzz(0x2E1A_0001, iters(30_000), true);
    relay_fuzz(0x2E1A_0002, iters(30_000), false);
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test --release --test fuzz -- --ignored"]
fn fuzz_relay_handle_long() {
    if !long_enabled("fuzz_relay_handle_long") {
        return;
    }
    for s in 0..4 {
        relay_fuzz(0x2E1A_1000 + s, iters(1_000_000), s % 2 == 0);
    }
}

/// 토큰 검증: 무작위·바꾼 토큰은 panic 없이 거부되고, 받아들여진 것은 서명 원본뿐이다.
#[test]
fn fuzz_token_verify() {
    let mut r = Rng(0x70CE);
    let good = token::sign(SECRET, &Claims { expires_at: NOW_UNIX + 60, ssrc: 7, room: "room-1", client: "c" });
    for _ in 0..iters(100_000) {
        let t = if r.chance(50) { mutate(&mut r, &good) } else { r.bytes(300) };
        let ok = token::verify(SECRET, &t, NOW_UNIX).is_ok();
        assert_eq!(ok, t == good, "토큰 {t:02x?}");
        if let Some(len) = token::token_len(&t) {
            assert!(len <= t.len() && len >= token::MIN_TOKEN_BYTES);
            let _ = token::peek(&t[..len]);
        }
    }
}
