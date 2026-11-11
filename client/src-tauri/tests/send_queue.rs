//! 송신 큐 지연·버스트 측정(P1-3): 6인 풀메시(원격 피어 5명)에 2.5ms 프레임을 실제 Opus로 인코딩해 보낸다.
//!
//! - `EngineStats.sendQueue`(프레임 생성 → 피어별 send_to 완료)가 채워지는지, 값이 프레임 주기보다 충분히 작은지 본다.
//! - 한 프레임을 5명에게 연달아 보내는 버스트 폭(첫 send_to 시작 → 마지막 완료)을 재서 페이싱 필요 여부의
//!   근거로 쓴다(설계 3절 "송신 페이싱"). 수치는 `--nocapture`로 출력된다.
//!
//! 벽시계·실스레드 측정이라 기본 스위트에서 빼고 CI `realtime` 잡(nextest `--run-ignored only --retries 2`)에서 돈다(리뷰 L4).
//! 로컬: `cargo test --test send_queue -- --ignored --nocapture`.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::codec::Encoder;
use band_lib::engine::config::{frame_code_from_samples, CodecKind, HEADER_BYTES, TAG_BYTES};
use band_lib::engine::jitter::JitterSettings;
use band_lib::engine::stats::{PathKind, PeerStateKind};
use band_lib::net::crypto::KeyPair;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::{Net, NetOptions, MAX_PEERS};

const FRAME: usize = 120; // 2.5ms @ 48kHz
const CH: usize = 2;

fn quantile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]
}

#[test]
#[ignore = "realtime: 결정적 짝 net::txlat::tests::{quantiles_and_window_reset, rotate_race_does_not_zero_quantiles}, engine::worker::tests::batched_frames_share_pass_start_for_send_queue"]
fn full_mesh_send_queue_latency_and_burst() {
    let opts = NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
    let net = Net::start("me".into(), &opts, JitterSettings { min_ms: 0.0, max_ms: 80.0 }).unwrap();

    // 원격 피어 5명: 받은 패킷은 별도 스레드가 비운다(수신 버퍼가 차서 송신 측 동작이 바뀌지 않게).
    let stop = Arc::new(AtomicBool::new(false));
    let mut drains = Vec::new();
    let mut dests = Vec::new();
    for i in 0..MAX_PEERS {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let addr: SocketAddr = sock.local_addr().unwrap();
        dests.push(addr);
        let keys = KeyPair::generate();
        let desc = LocalDescriptor { ssrc: 0x1000 + i as u32, key: LocalDescriptor::encode_key(&keys.public_bytes()), candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }] };
        let id = format!("p{i}");
        net.add_peer(id.clone(), &desc).unwrap();
        let p = net.peers().into_iter().find(|p| p.id == id).unwrap();
        {
            let mut c = p.conn.lock();
            c.state = PeerStateKind::Connected;
            c.path = Some((addr, PathKind::Lan));
            c.last_rx = Some(Instant::now());
        }
        let stop = stop.clone();
        drains.push(std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                if sock.recv(&mut buf).is_ok() {
                    n += 1;
                }
            }
            n
        }));
    }

    let mut enc = Encoder::new(CodecKind::Opus, CH, 128_000).unwrap();
    let code = frame_code_from_samples(FRAME).unwrap();
    let mut frame = vec![0f32; FRAME * CH];
    let mut payload = vec![0u8; 1500];
    let mut pkt = vec![0u8; 1500];
    let period = Duration::from_micros(2500);
    let frames = 2400; // 6초: 제어 스레드의 2초 창이 최소 두 번 돈다
    let mut burst_us = Vec::with_capacity(frames);
    let mut total_us = Vec::with_capacity(frames);
    let mut enc_us = Vec::with_capacity(frames);
    let mut raw_us = Vec::with_capacity(frames);
    let mut pkt_bytes = 0;
    let raw = UdpSocket::bind("127.0.0.1:0").unwrap();
    let start = Instant::now();
    let mut phase = 0f32;
    for k in 0..frames {
        let due = start + period * k as u32;
        if let Some(w) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(w);
        }
        for s in frame.chunks_mut(CH) {
            phase += 440.0 / 48_000.0;
            let v = (phase * std::f32::consts::TAU).sin() * 0.3;
            s.fill(v);
        }
        // 워커의 on_capture_frame과 같은 순서: 프레임 시각 → 인코딩 → 피어별 송신
        let frame_at = Instant::now();
        let n = enc.encode(&frame, &mut payload).unwrap();
        let t_send = Instant::now();
        net.shared().send_audio_timed(frame_at, &payload[..n], 0, code, (k * FRAME) as u32, &mut pkt, None);
        let done = Instant::now();
        burst_us.push(done.duration_since(t_send).as_secs_f64() * 1e6);
        total_us.push(done.duration_since(frame_at).as_secs_f64() * 1e6);
        enc_us.push(t_send.duration_since(frame_at).as_secs_f64() * 1e6);
        // 기준선: 같은 크기 패킷을 암호화 없이 5명에게 send_to만(시스템 호출 몫 — 빌드 모드와 무관)
        let r0 = Instant::now();
        for d in &dests {
            let _ = raw.send_to(&pkt[..n + HEADER_BYTES + TAG_BYTES], d);
        }
        raw_us.push(r0.elapsed().as_secs_f64() * 1e6);
        pkt_bytes = n + HEADER_BYTES + TAG_BYTES;
    }
    // 송신 중에 끝난 직전 창(6초 송신이라 그 창은 통째로 송신 구간 안이다). 송신을 멈춘 뒤 창이 돌면 0이 된다.
    let sq = net.send_queue_stats();
    stop.store(true, Ordering::Relaxed);
    let received: u64 = drains.into_iter().map(|h| h.join().unwrap()).sum();

    let b50 = quantile(&mut burst_us, 0.50);
    let b99 = quantile(&mut burst_us, 0.99);
    let bmax = quantile(&mut burst_us, 1.0);
    let t50 = quantile(&mut total_us, 0.50);
    let t99 = quantile(&mut total_us, 0.99);
    let e50 = quantile(&mut enc_us, 0.50);
    let e99 = quantile(&mut enc_us, 0.99);
    let r50 = quantile(&mut raw_us, 0.50);
    let r99 = quantile(&mut raw_us, 0.99);
    println!(
        "=== 송신 큐 지연 (피어 {MAX_PEERS}명, Opus stereo 2.5ms, {frames}프레임, 루프백) ===\n\
         EngineStats.sendQueue(직전 2초 창, 패킷별): p50 {:.3}ms p99 {:.3}ms max {:.3}ms ({}패킷)\n\
         프레임별 생성→마지막 send_to: p50 {:.1}µs p99 {:.1}µs\n\
         버스트 폭(첫 send_to 시작→마지막 완료, 5패킷): p50 {b50:.1}µs p99 {b99:.1}µs max {bmax:.1}µs = 프레임 주기의 {:.1}%(p99)\n\
         인코딩(Opus): p50 {e50:.1}µs p99 {e99:.1}µs\n\
         기준선 — 암호화 없는 send_to 5회({pkt_bytes}B): p50 {r50:.1}µs p99 {r99:.1}µs\n\
         수신 {received}/{}패킷(엔진 + 기준선 + 제어)",
        sq.p50_ms,
        sq.p99_ms,
        sq.max_ms,
        sq.packets,
        t50,
        t99,
        b99 / 2500.0 * 100.0,
        frames * MAX_PEERS * 2,
    );
    // 창 하나(2초)에 피어 5명 × 800프레임 근처가 잡혀야 한다(스케줄링 여유로 넓게)
    assert!(sq.packets >= 2000, "sendQueue 창이 비었다: {sq:?}");
    assert!(sq.p50_ms > 0.0 && sq.p50_ms <= sq.p99_ms && sq.p99_ms <= sq.max_ms.max(sq.p99_ms), "{sq:?}");
    // 중앙값은 프레임 주기(2.5ms)보다 한참 작아야 한다(디버그 빌드·공유 러너 여유 포함)
    assert!(sq.p50_ms < 2.5, "송신 큐 지연 중앙값이 프레임 주기 이상: {sq:?}");
}
