//! 입력 파서 퍼징(P2 발굴 — 네트워크 파서). Windows에서 cargo-fuzz(libFuzzer)를 쓰기 어려워 시드 고정 무작위 바이트로 대신한다.
//!
//! 기본 `cargo test`는 짧게(수 초) 돈다. 긴 판: `BAND_FUZZ_LONG=1 cargo test -p band-dsp --release --test fuzz_parsers -- --ignored`.
//! 반복 수는 `BAND_FUZZ_ITERS`로 바꿀 수 있다(짧은 판·긴 판 모두에 곱해지는 배수가 아니라 그 값 자체).
//!
//! 대상: 엔진 패킷 헤더(`packet::Header::parse`)와, 인증을 통과한 **악의적 피어**가 보낼 수 있는 임의 헤더 필드·페이로드를
//! 받는 지터 버퍼(`insert` → `pop`/`end_burst`, Opus·PCM 디코드·FEC·PLC 포함). 판정: panic 없음, 출력 유한, 끝남.

use band_dsp::codec::Encoder;
use band_dsp::config::{CodecKind, HEADER_BYTES, MAX_PAYLOAD_BYTES, TAG_BYTES};
use band_dsp::jitter::{JitterBuffer, JitterSettings};
use band_dsp::packet::{Header, PacketType, FLAG_OPUS, FLAG_STEREO, MAGIC, VERSION};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

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

/// 무작위 바이트. 절반은 앞 4바이트를 그럴듯한 엔진 헤더로 만들어 파서 깊이 들어가게 한다.
fn packet_like(rng: &mut StdRng) -> Vec<u8> {
    let len = match rng.gen_range(0..4) {
        0 => rng.gen_range(0..HEADER_BYTES + TAG_BYTES + 2),
        _ => rng.gen_range(0..1400),
    };
    let mut b: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
    if rng.gen_bool(0.5) && b.len() >= 4 {
        b[0..2].copy_from_slice(&MAGIC);
        b[2] = if rng.gen_bool(0.9) { VERSION } else { rng.gen() };
        b[3] = rng.gen_range(0..12);
    }
    b
}

fn header_parse_fuzz(seed: u64, n: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut accepted = 0usize;
    for _ in 0..n {
        let b = packet_like(&mut rng);
        if let Some(h) = Header::parse(&b) {
            accepted += 1;
            // 수락 조건 = 길이·magic·version·type 전부 맞음
            assert!(b.len() >= HEADER_BYTES + TAG_BYTES, "짧은 패킷 수락: {b:02x?}");
            assert_eq!(&b[0..3], &[MAGIC[0], MAGIC[1], VERSION]);
            assert!((1..=6).contains(&b[3]), "모르는 type 수락: {}", b[3]);
            // 왕복: 다시 쓰면 원래 헤더 바이트와 같다(필드 손실·혼동 없음)
            let mut hb = [0u8; HEADER_BYTES];
            h.write(&mut hb);
            assert_eq!(&hb[..], &b[..HEADER_BYTES]);
        } else if b.len() >= HEADER_BYTES + TAG_BYTES && b[0..3] == [MAGIC[0], MAGIC[1], VERSION] {
            assert!(!(1..=6).contains(&b[3]), "올바른 헤더 거부: {:02x?}", &b[..HEADER_BYTES]);
        }
    }
    assert!(accepted > 0, "생성기가 파서 안쪽까지 못 들어감");
}

#[test]
fn header_parse_random_bytes() {
    header_parse_fuzz(0xB4ED_0001, iters(50_000));
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test -p band-dsp --release --test fuzz_parsers -- --ignored"]
fn header_parse_random_bytes_long() {
    if !long_enabled("header_parse_random_bytes_long") {
        return;
    }
    header_parse_fuzz(0xB4ED_1001, iters(5_000_000));
}

/// 인증을 통과한(키를 가진) 피어가 보낼 수 있는 아무 Audio 패킷: 헤더 필드는 전부 공격자 마음대로다.
struct Hostile {
    rng: StdRng,
    seq: u32,
    ts: u32,
    enc: [Encoder; 2],
}

impl Hostile {
    fn new(seed: u64) -> Self {
        let rng = StdRng::seed_from_u64(seed);
        let enc = [Encoder::new(CodecKind::Opus, 1, 64_000).unwrap(), Encoder::new(CodecKind::Opus, 2, 96_000).unwrap()];
        Self { rng, seq: 0xFFFF_FF00, ts: 0, enc }
    }

    fn next(&mut self) -> (Header, Vec<u8>) {
        let r = &mut self.rng;
        // seq: 대부분 순서대로, 가끔 역행·중복·먼 미래·랩어라운드
        self.seq = match r.gen_range(0..20) {
            0 => r.gen(),
            1 => self.seq.wrapping_sub(r.gen_range(0..40)),
            2 => self.seq.wrapping_add(r.gen_range(0..2000)),
            3 => self.seq,
            _ => self.seq.wrapping_add(1),
        };
        self.ts = if r.gen_range(0..10) == 0 { r.gen() } else { self.ts.wrapping_add(240) };
        let frame_code: u8 = if r.gen_range(0..8) == 0 { r.gen() } else { [1u8, 2, 4][r.gen_range(0..3)] };
        let flags: u8 = if r.gen_range(0..8) == 0 { r.gen() } else { r.gen_range(0..4) };
        let payload: Vec<u8> = match r.gen_range(0..6) {
            // 제대로 된 Opus 패킷(프레임 크기는 헤더와 다를 수 있다)
            0 | 1 => {
                let fs = [120usize, 240, 480, 960][r.gen_range(0..4)];
                let ch = if r.gen_bool(0.5) { 2 } else { 1 };
                let pcm: Vec<f32> = (0..fs * ch).map(|_| r.gen_range(-1.0..1.0)).collect();
                let mut out = [0u8; 1500];
                let n = self.enc[ch - 1].encode(&pcm, &mut out).unwrap();
                let mut p = out[..n].to_vec();
                if r.gen_bool(0.3) && !p.is_empty() {
                    let i = r.gen_range(0..p.len());
                    p[i] ^= 1 << r.gen_range(0..8); // 비트 하나 뒤집기
                }
                if r.gen_bool(0.2) {
                    p.truncate(r.gen_range(0..=p.len()));
                }
                p
            }
            // 아무 바이트(길이 0..최대+α: 상한 넘는 것도 넣어 본다)
            2 | 3 => (0..r.gen_range(0..MAX_PAYLOAD_BYTES + 40)).map(|_| r.gen()).collect(),
            // 홀수 길이 PCM, 빈 페이로드
            4 => (0..r.gen_range(0..8) * 2 + 1).map(|_| r.gen()).collect(),
            _ => Vec::new(),
        };
        (Header { ptype: PacketType::Audio, ssrc: 1, seq: self.seq, timestamp: self.ts, flags, frame_code }, payload)
    }
}

fn jitter_hostile_fuzz(seed: u64, packets: usize) {
    let mut src = Hostile::new(seed);
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    jb.set_consumer_lead(72.0);
    let mut out = vec![0f32; 2 * 2048];
    let mut now: u64 = 0;
    for i in 0..packets {
        let burst = src.rng.gen_range(0..4);
        for _ in 0..burst {
            let (h, p) = src.next();
            jb.insert(&h, &p, now);
        }
        // 소비: 장치 콜백 크기 1..1024 프레임, 가끔 0
        let frames = [0usize, 1, 64, 120, 128, 240, 256, 441, 480, 512, 1024][src.rng.gen_range(0..11)];
        jb.pop(&mut out[..frames * 2]);
        for (k, s) in out[..frames * 2].iter().enumerate() {
            // 크기는 보지 않는다: 깨진(그러나 인증된) Opus는 ±1을 넘게 디코드될 수 있고(관찰 최대 ≈ 11), 믹서 `soft_clip`이
            // 출력 전에 1.0 안으로 누른다. NaN·inf는 soft_clip을 지나 장치까지 가므로 막아야 한다.
            assert!(s.is_finite(), "seed {seed:#x} 반복 {i}: 출력[{k}] = {s}");
        }
        let _ = jb.real_frames();
        now += src.rng.gen_range(0..6_000);
        if src.rng.gen_range(0..50) == 0 {
            now += src.rng.gen_range(0..2_000_000); // 긴 정지(절전·디버거)
        }
        jb.end_burst(now, now, src.rng.gen_range(0.0..500.0));
        let st = jb.stats();
        assert!(st.buffer_ms.is_finite(), "seed {seed:#x} 반복 {i}: buffer_ms {}", st.buffer_ms);
        if src.rng.gen_range(0..5000) == 0 {
            jb.restart();
        }
    }
}

#[test]
fn jitter_survives_hostile_peer() {
    for s in 0..4u64 {
        jitter_hostile_fuzz(0x717E_0000 + s, iters(5_000));
    }
}

#[test]
#[ignore = "긴 퍼징: BAND_FUZZ_LONG=1 cargo test -p band-dsp --release --test fuzz_parsers -- --ignored"]
fn jitter_survives_hostile_peer_long() {
    if !long_enabled("jitter_survives_hostile_peer_long") {
        return;
    }
    for s in 0..16u64 {
        jitter_hostile_fuzz(0x717E_1000 + s, iters(200_000));
    }
}

/// 상한(`MAX_PAYLOAD_BYTES`)을 넘는 페이로드는 슬롯 복사 전에 버린다(경계값 회귀): 통계·상태가 새 버퍼와 같고, 상한 그대로는 받는다.
#[test]
fn jitter_rejects_oversized_payload() {
    let settings = JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 };
    // 같은 pop·end_burst를 받되 insert는 받지 않는 기준 버퍼(목표 계산은 소비 패턴으로도 바뀐다)
    let mut reference = JitterBuffer::new(settings);
    let mut jb = JitterBuffer::new(settings);
    let mut out = [0f32; 960];
    for (i, len) in [MAX_PAYLOAD_BYTES + 1, 4096].into_iter().enumerate() {
        for k in 0..40u32 {
            let h = Header { ptype: PacketType::Audio, ssrc: 1, seq: 5 + k, timestamp: k * 240, flags: FLAG_OPUS | FLAG_STEREO, frame_code: 2 };
            jb.insert(&h, &vec![0x7f; len], u64::from(k) * 5000);
            for b in [&mut jb, &mut reference] {
                b.pop(&mut out);
                b.end_burst(u64::from(k) * 5000, u64::from(k) * 5000, 0.0);
            }
        }
        assert!(!jb.is_playing(), "{i}: 상한 초과 패킷으로 재생 시작");
        assert_eq!(jb.stats(), reference.stats(), "{i}: 상한 초과 패킷이 통계에 들어감");
    }
    let h = Header { ptype: PacketType::Audio, ssrc: 1, seq: 100, timestamp: 0, flags: 0, frame_code: 2 };
    jb.insert(&h, &vec![0x7f; MAX_PAYLOAD_BYTES], 300_000);
    for b in [&mut jb, &mut reference] {
        b.end_burst(300_000, 300_000, 0.0);
    }
    assert_ne!(jb.stats(), reference.stats(), "상한 그대로의 패킷은 받는다(시험이 거름을 지나는지 확인)");
}
