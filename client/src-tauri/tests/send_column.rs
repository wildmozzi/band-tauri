//! 송신 패킷 열 시험(엔진 ②b, `docs/overhaul/engine-stage2b-design.md` 5.4절, PR #125 검토 M3).
//!
//! 워커의 송신 경로(캡처 프레임 → 인코드 → 패킷)를 단계 구조체로 옮길 때 지켜야 하는 조건 — 인코더가 **모든 프레임을
//! 순서대로** 인코드한다(C1), `ts`가 프레임마다 프레임 크기만큼 는다(C6), 플래그가 그대로다 — 을 패킷 **내용**으로 지킨다.
//! 실시간 테스트의 "은닉 수가 같은 범위"로는 인코더가 한 프레임을 건너뛰어도 드러나지 않는다.
//!
//! 방법: 가짜 장치에 결정적 입력(샘플 번호의 함수)을 넣고, 원시 UDP 소켓이 상대 피어 역할을 하며 엔진이 보낸 Audio
//! 패킷을 복호화해 `(ts 증가분, flags, 페이로드)` 열을 모은다. 그 열을 두 가지와 비교한다.
//! - **참조 인코더**: 시험이 같은 입력을 프레임 0부터 순서대로 따로 인코드한 결과. 받은 i번째 패킷의 페이로드는 참조의
//!   `k0 + i`번째와 바이트가 같아야 한다(`k0` = 연결이 선 뒤 처음 받은 프레임 번호). Opus는 인코더 상태가 있으므로
//!   프레임 하나를 건너뛰거나 두 번 인코드하면 그 뒤가 전부 달라진다. 같은 프로세스·같은 라이브러리라 기계와 무관하다.
//! - **fixture**(PCM16만): 프레임 `FIX_FROM..FIX_FROM + FIX_LEN`의 열 해시를 `tests/fixtures/send-column-pcm16.sha256`과
//!   비교한다. PCM16 양자화는 기계와 무관하다(Opus 부동소수 경로는 CPU 종류에 따라 다를 수 있어 fixture로 두지 않는다).
//!   이 fixture는 5분 하네스 fixture와 같은 규칙이다 — **갱신 금지**, 달라지면 버그.
//!
//! 조건(이 안에서는 패킷 내용이 타이밍과 무관하다): 설정 코덱 고정, 자동 전환 없음(직접 경로 피어 하나, 손실 보고 없음),
//! 입력 링 넘침 없음, 48kHz(리샘플러 통과). 프레임 경계는 입력 샘플 수로만 정해진다 — 가짜 장치의 클럭 원점을 미래로 두어
//! 입력이 샘플 0부터 시작하게 한다. 도착 시각·패킷 수의 끝자리는 비교하지 않는다.
//! 벽시계 임계가 없으므로 기본 `cargo test`(결정적)에서 돈다.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::codec::Encoder;
use band_lib::engine::config::{CodecKind, EngineConfig, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use band_lib::engine::device::fake::{FakeBackend, SourceFn};
use band_lib::engine::mixer::MixControls;
use band_lib::engine::Engine;
use band_lib::net::crypto::KeyPair;
use band_lib::net::packet::{path_tag, Header, PacketType, FLAG_OPUS, FLAG_STEREO};
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;
use sha2::{Digest, Sha256};

/// 행 감지 상한. 판정은 받은 패킷의 내용으로 한다.
const HANG: Duration = Duration::from_secs(30);
/// 비교하는 패킷 수(2.5ms 프레임 2,000개 = 5초분).
const COLUMN: usize = 2_000;
/// fixture 구간(프레임 번호). 연결은 엔진 시작 직후 로컬에서 서므로 2초(프레임 800) 전에 첫 패킷이 온다.
const FIX_FROM: usize = 800;
const FIX_LEN: usize = 2_000;

/// 샘플 번호 → [-0.5, 0.5) 결정적 잡음(splitmix64). 프레임마다 내용이 달라 참조 열에서 위치가 하나로 정해진다.
fn sample(i: u64) -> f32 {
    let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 24) as f32) - 0.5
}

fn source() -> Arc<SourceFn> {
    Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            frame.fill(sample(start + i as u64));
        }
    })
}

/// 참조: 같은 입력을 프레임 0부터 순서대로 인코드한다(필요한 만큼 늘린다).
struct Reference {
    enc: Encoder,
    frame: usize,
    ch: usize,
    payloads: Vec<Vec<u8>>,
}

impl Reference {
    fn new(cfg: &EngineConfig) -> Self {
        let ch = cfg.channels as usize;
        Self { enc: Encoder::new(cfg.codec, ch, cfg.opus_bitrate()).unwrap(), frame: cfg.frame_samples(), ch, payloads: Vec::new() }
    }

    fn get(&mut self, k: usize) -> &[u8] {
        let mut pcm = vec![0f32; self.frame * self.ch];
        let mut out = vec![0u8; MAX_PAYLOAD_BYTES];
        while self.payloads.len() <= k {
            let k0 = self.payloads.len() as u64 * self.frame as u64;
            for (i, fr) in pcm.chunks_exact_mut(self.ch).enumerate() {
                fr.fill(sample(k0 + i as u64));
            }
            let n = self.enc.encode(&pcm, &mut out).unwrap();
            self.payloads.push(out[..n].to_vec());
        }
        &self.payloads[k]
    }
}

struct Packet {
    ts: u32,
    flags: u8,
    payload: Vec<u8>,
}

/// 엔진을 띄우고 원시 소켓 피어로 Audio 패킷 `want`개를 순서대로 받는다(복호화한 페이로드).
fn collect(cfg: &EngineConfig, want: usize) -> Vec<Packet> {
    let mut backend = FakeBackend::new(48_000, source());
    backend.record_output = false;
    // 클럭 원점을 미래로: 입력 콜백이 샘플 0부터 시작한다(원점이 과거면 "지금"에 해당하는 블록부터라 실행마다 다르다).
    backend.t0 = Some(Instant::now() + Duration::from_millis(300));
    let opts = NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
    let e = Engine::start(cfg.clone(), Arc::new(backend), Arc::new(MixControls::default()), Some(("alice".into(), opts))).unwrap();
    let e_addr: SocketAddr = format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap();
    let e_desc = e.descriptor().unwrap().clone();

    let keys = KeyPair::generate();
    let ssrc = 0x5EED_C01Du32;
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let desc = LocalDescriptor {
        ssrc,
        key: LocalDescriptor::encode_key(&keys.public_bytes()),
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: sock.local_addr().unwrap().to_string() }],
    };
    e.add_peer("bob".into(), &desc).unwrap();
    let (tx, rx) = keys.pair_ciphers(ssrc, &e_desc.decode_key().unwrap(), e_desc.ssrc).unwrap();

    let mut out = Vec::with_capacity(want);
    let mut buf = [0u8; MAX_PACKET_BYTES];
    let mut hello = [0u8; MAX_PACKET_BYTES];
    let (mut hello_seq, mut hello_at) = (1u32, None::<Instant>);
    let t0 = Instant::now();
    while out.len() < want {
        assert!(t0.elapsed() < HANG, "Audio 패킷을 {}개만 받음", out.len());
        // 인증된 Hello(평문 = 엔진이 아는 내 id)로 경로를 세우고, 주기적으로 다시 보내 연결을 유지한다.
        if hello_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(200)) {
            let h = Header { ptype: PacketType::Hello, ssrc, seq: hello_seq, timestamp: 0, flags: path_tag(false), frame_code: 0 };
            let n = tx.seal(&h, b"bob", &mut hello).unwrap();
            sock.send_to(&hello[..n], e_addr).unwrap();
            hello_seq += 1;
            hello_at = Some(Instant::now());
        }
        let Ok(n) = sock.recv(&mut buf) else { continue };
        let Some(h) = Header::parse(&buf[..n]) else { continue };
        if h.ptype != PacketType::Audio {
            continue;
        }
        let range = rx.open(&h, &mut buf[..n]).expect("엔진의 Audio 패킷이 복호화돼야 한다");
        out.push(Packet { ts: h.timestamp, flags: h.flags, payload: buf[range].to_vec() });
    }
    // 입력 링이 넘치면(워커가 2초 넘게 멈춤) 프레임 경계가 밀린다 — 그때는 아래 참조 비교가 "어디에도 없다"로 실패한다.
    out
}

/// 받은 열이 참조 열의 연속 구간과 같은지 본다. 반환: 첫 패킷의 프레임 번호.
fn assert_matches_reference(cfg: &EngineConfig, got: &[Packet], reference: &mut Reference) -> usize {
    let frame = cfg.frame_samples() as u32;
    let want_flags = if cfg.codec == CodecKind::Opus { FLAG_OPUS } else { 0 } | if cfg.channels == 2 { FLAG_STEREO } else { 0 };
    // 첫 패킷이 몇 번째 프레임인지: 참조에서 같은 페이로드를 찾는다(연결은 수 초 안에 선다).
    let limit = (HANG.as_secs_f64() * 48_000.0 / frame as f64) as usize;
    let k0 = (0..limit).find(|&k| reference.get(k) == got[0].payload.as_slice()).expect("첫 패킷이 참조 열 어디에도 없다 — 프레임 경계나 인코더 상태가 다르다");
    for (i, p) in got.iter().enumerate() {
        assert_eq!(p.flags, want_flags, "패킷 {i}의 flags");
        if i > 0 {
            assert_eq!(p.ts.wrapping_sub(got[i - 1].ts), frame, "패킷 {i}의 ts 증가분(프레임 {})", k0 + i);
        }
        assert!(reference.get(k0 + i) == p.payload.as_slice(), "패킷 {i}(프레임 {})의 페이로드가 참조 인코더와 다르다", k0 + i);
    }
    k0
}

fn cfg(codec: CodecKind) -> EngineConfig {
    EngineConfig { buffer_size: 128, frame_ms: 2.5, codec, channels: 1, input_channels: vec![0], ..Default::default() }
}

/// 열 해시: 패킷마다 `ts 증가분(u32 BE) ‖ flags ‖ 페이로드 길이(u16 BE) ‖ 페이로드`.
fn column_hash(frame: u32, flags: u8, payloads: impl Iterator<Item = Vec<u8>>) -> String {
    let mut h = Sha256::new();
    for p in payloads {
        h.update(frame.to_be_bytes());
        h.update([flags]);
        h.update((p.len() as u16).to_be_bytes());
        h.update(&p);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn send_column_opus_matches_reference_encoder() {
    let cfg = cfg(CodecKind::Opus);
    let got = collect(&cfg, COLUMN);
    let k0 = assert_matches_reference(&cfg, &got, &mut Reference::new(&cfg));
    println!("Opus: 프레임 {k0}부터 {}개 패킷이 참조 인코더와 같다", got.len());
}

#[test]
fn send_column_pcm16_matches_reference_and_fixture() {
    let cfg = cfg(CodecKind::Pcm16);
    let frame = cfg.frame_samples() as u32;
    // fixture 구간 끝까지 받는다(첫 패킷이 프레임 k0이면 FIX_FROM + FIX_LEN − k0개).
    let got = collect(&cfg, FIX_FROM + FIX_LEN);
    let mut reference = Reference::new(&cfg);
    let k0 = assert_matches_reference(&cfg, &got, &mut reference);
    assert!(k0 <= FIX_FROM, "첫 패킷이 프레임 {k0} — fixture 구간({FIX_FROM}부터) 앞에서 연결이 서야 한다");
    let window = &got[FIX_FROM - k0..FIX_FROM - k0 + FIX_LEN];
    for w in window.windows(2) {
        assert_eq!(w[1].ts.wrapping_sub(w[0].ts), frame);
    }
    let hash = column_hash(frame, window[0].flags, window.iter().map(|p| p.payload.clone()));
    // 참조 열의 같은 구간도 같은 해시여야 한다(위 비교의 재확인 — fixture가 참조 인코더의 것임을 보인다)
    let ref_hash = column_hash(frame, 0, (FIX_FROM..FIX_FROM + FIX_LEN).map(|k| reference.get(k).to_vec()));
    assert_eq!(hash, ref_hash);
    let fixture = include_str!("fixtures/send-column-pcm16.sha256").trim();
    assert_eq!(hash, fixture, "송신 패킷 열(PCM16, 프레임 {FIX_FROM}..{})이 fixture와 다르다 — fixture를 갱신하지 말 것(설계 5.4절)", FIX_FROM + FIX_LEN);
}

/// 두 번 돌려 같은 열이 나오는지(결정적인지) 본다 — ②b-0의 가능 여부 확인을 시험으로 남긴다.
#[test]
fn send_column_is_deterministic_across_runs() {
    let cfg = cfg(CodecKind::Opus);
    let mut reference = Reference::new(&cfg);
    let a = collect(&cfg, 600);
    let ka = assert_matches_reference(&cfg, &a, &mut reference);
    let b = collect(&cfg, 600);
    let kb = assert_matches_reference(&cfg, &b, &mut reference);
    // 두 실행이 겹치는 프레임 구간의 페이로드가 같다(둘 다 참조와 같으므로 당연하지만, 겹침이 실제로 있는지 본다).
    let (lo, hi) = (ka.max(kb), (ka + a.len()).min(kb + b.len()));
    assert!(lo < hi, "두 실행의 프레임 구간이 겹치지 않는다: {ka}.. / {kb}..");
    for k in lo..hi {
        assert_eq!(a[k - ka].payload, b[k - kb].payload, "프레임 {k}");
    }
}
