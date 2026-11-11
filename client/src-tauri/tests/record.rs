//! 합주 녹음 통합 테스트(P2-4, 설계 문서 1.5절).
//!
//! - 결정적: 가짜 장치 두 엔진(A 사인 → B)을 루프백 UDP로 잇고 B에서 녹음한다. 트랙 구성·길이 정렬·피어 소리·
//!   통계 필드·엔진 정지 시 마무리를 본다(벽시계 품질 임계 없음).
//! - 실시간(`#[ignore]`): 같은 링크에서 녹음 끈 창과 켠 창을 교대로(ABBA) 재서 버퍼 지연·은닉·xrun을 비교한다
//!   (PR 본문 수치). 판정 규칙(`judge`)은 결정적 테스트로 따로 검사한다.
//!   로컬 실행: `cargo test --test record -- --ignored --nocapture --test-threads 1`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::record::{RecordOptions, TrackKind};
use band_lib::engine::stats::PeerStateKind;
use band_lib::engine::wav::RecordFormat;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;
use parking_lot::Mutex;

static RT_LOCK: Mutex<()> = Mutex::new(());

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
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None }
}

fn config(buffer: u32, frame_ms: f32) -> EngineConfig {
    EngineConfig { buffer_size: buffer, frame_ms, codec: CodecKind::Opus, channels: 1, input_channels: vec![0], ..Default::default() }
}

fn local_addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn with_candidate(d: &LocalDescriptor, addr: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }], ..d.clone() }
}

fn wait_connected(e: &Engine) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        let s = e.stats();
        if !s.peers.is_empty() && s.peers.iter().all(|p| p.state == PeerStateKind::Connected) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn pair(cfg: EngineConfig, ba: FakeBackend, bb: FakeBackend) -> (Engine, Engine) {
    let a = Engine::start(cfg.clone(), Arc::new(ba), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(cfg, Arc::new(bb), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    a.add_peer("bob".into(), &with_candidate(b.descriptor().unwrap(), local_addr(&b))).unwrap();
    b.add_peer("alice".into(), &with_candidate(a.descriptor().unwrap(), local_addr(&a))).unwrap();
    assert!(wait_connected(&a) && wait_connected(&b), "연결 실패");
    (a, b)
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("band-rec-it-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    d
}

/// 24비트 스테레오 WAV(헤더 80바이트)의 L 채널.
fn read_left(path: &Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    b[80..].as_chunks::<6>().0.iter().map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_607.0).collect()
}

#[test]
fn records_local_and_peer_tracks_aligned() {
    let _g = RT_LOCK.lock();
    let (a, b) = pair(config(256, 5.0), sine_backend(), FakeBackend::silence(48_000));
    std::thread::sleep(Duration::from_millis(500));
    let dir = tmp("pair");
    let opts = RecordOptions {
        include_mix: true,
        name: Some("합주 테스트".into()),
        local_name: Some("나".into()),
        track_names: [("alice".to_string(), "앨리스".to_string())].into(),
        ..Default::default()
    };
    let st = b.record_start(opts, &dir).unwrap();
    assert!(st.folder.ends_with("합주 테스트"));
    assert!(b.record_start(RecordOptions::default(), &dir).is_err(), "중복 시작 거부");
    std::thread::sleep(Duration::from_millis(1500));
    let live = b.stats().recording.expect("녹음 통계");
    assert!(live.duration_sec > 1.0, "{live:?}");
    assert!(live.bytes > 0 && live.tracks == 3, "{live:?}");
    assert_eq!(live.dropped_frames, 0);

    let sum = b.recorder().lock().stop().unwrap();
    assert!(b.stats().recording.is_none());
    assert!(sum.error.is_none(), "{:?}", sum.error);
    assert_eq!(sum.dropped_frames, 0);
    assert_eq!(sum.sample_rate, 48_000);
    let kinds: Vec<_> = sum.tracks.iter().map(|t| t.kind).collect();
    assert!(kinds.contains(&TrackKind::Local) && kinds.contains(&TrackKind::Peer) && kinds.contains(&TrackKind::Mix), "{kinds:?}");
    let folder = PathBuf::from(&sum.folder);
    let file = |k: TrackKind| folder.join(&sum.tracks.iter().find(|t| t.kind == k).unwrap().file);
    let peer = sum.tracks.iter().find(|t| t.kind == TrackKind::Peer).unwrap();
    assert_eq!(peer.peer_id.as_deref(), Some("alice"));
    assert!(peer.file.ends_with("앨리스.wav"), "{}", peer.file);

    let me = read_left(&file(TrackKind::Local));
    let pa = read_left(&file(TrackKind::Peer));
    let mix = read_left(&file(TrackKind::Mix));
    assert_eq!(me.len(), pa.len(), "트랙 길이가 같다");
    assert_eq!(me.len(), mix.len());
    assert!((me.len() as f64 / 48_000.0 - sum.duration_sec).abs() < 1e-6);
    assert!(sum.duration_sec > 1.2, "{}", sum.duration_sec);
    // B 입력은 무음, 피어(A 사인 0.5)는 소리가 있다
    assert!(me.iter().all(|&v| v.abs() < 1e-6));
    let peak = pa.iter().fold(0f32, |m, &v| m.max(v.abs()));
    assert!(peak > 0.4 && peak < 0.6, "피어 트랙 피크 {peak}");
    assert!(folder.join("session.json").exists());
    // 마커 사이드카(P2-4a): 트랙마다 항목이 있다(가짜 장치·로컬 네트워크라 구간은 보통 0개)
    assert_eq!(sum.markers_file.as_deref(), Some("markers.json"));
    let mj: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("markers.json")).unwrap()).unwrap();
    assert_eq!(mj["tracks"].as_array().map(|t| t.len()), Some(sum.tracks.len()));

    // 엔진 정지가 진행 중인 녹음을 마무리한다(FLAC: 엔진 정지 경로에서도 STREAMINFO가 확정돼 표준 디코더로 열린다)
    b.record_start(RecordOptions { format: RecordFormat::Flac, ..Default::default() }, &dir).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    drop(b);
    drop(a);
    let sessions = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().join("session.json").exists()).count();
    assert_eq!(sessions, 2, "정지로 마무리된 두 번째 세션");
    let flacs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .flat_map(|e| std::fs::read_dir(e.path()).unwrap().filter_map(|f| f.ok()).map(|f| f.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "flac"))
        .collect();
    assert!(!flacs.is_empty());
    for p in &flacs {
        let mut r = claxon::FlacReader::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let total = r.streaminfo().samples.expect("총 샘플 수 확정");
        assert!(total > 0, "{}", p.display());
        assert_eq!(r.samples().count() as u64, total * 2, "{}", p.display());
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// 한 측정 창의 관찰값.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// 창 동안 4Hz로 읽은 bufferMs 평균
    buf_ms: f32,
    concealed: u64,
    xruns: u64,
    produced: u64,
}

/// 한 창(`ms`밀리초) 관찰.
fn window(b: &Engine, ms: u64) -> Window {
    let s0 = b.stats();
    let j0 = b.peer_jitter_stats()[0].1;
    let mut buf = 0.0;
    let n = (ms / 250).max(1);
    for _ in 0..n {
        std::thread::sleep(Duration::from_millis(250));
        buf += b.stats().peers[0].buffer_ms;
    }
    let s1 = b.stats();
    let j1 = b.peer_jitter_stats()[0].1;
    Window { buf_ms: buf / n as f32, concealed: j1.concealed - j0.concealed, xruns: s1.xruns - s0.xruns, produced: j1.produced - j0.produced }
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// xrun·은닉이 생긴 창 수: 켬 쪽이 끔 쪽보다 이만큼까지 많아도 통과(러너 한두 번의 끊김).
const BAD_WINDOW_SLACK: usize = 2;

/// 녹음 켬/끔 판정 (P3-0d). 같은 링크에서 끔·켬 창을 ABBA로 교대해 잰 값을 받는다.
///
/// 호스티드 windows 러너에서는 링크의 버퍼 지연이 수 초에 걸쳐 1~2ms 떠다니고 xrun이 한 창에 몰려(3개) 나온다
/// (#49 실패 로그). 끔 8초 → 켬 8초를 한 번 비교하면 그 흔들림이 한쪽에만 떨어질 때 실패했다. 그래서
/// - 버퍼 지연: 켬 창 중앙값 − 끔 창 중앙값. 허용치는 반 프레임(`frame_ms/2`, 최소 1ms) — 녹음이 RT 경로를
///   방해해 지터 버퍼가 한 프레임이라도 늘면(켬 창마다 +`frame_ms`) 잡힌다.
/// - xrun·은닉: 개수 합이 아니라 "생긴 창 수". 러너 한 번의 몰림은 창 하나만 더럽힌다. 켬 쪽 창 수가 끔 쪽보다
///   [`BAD_WINDOW_SLACK`]개 넘게 많으면 실패 — 녹음이 계속 방해하면 켬 창 대부분에서 생기므로 잡힌다.
fn judge(frame_ms: f32, off: &[Window], on: &[Window]) -> Result<(), String> {
    let tol = (frame_ms / 2.0).max(1.0);
    let d = median(on.iter().map(|w| w.buf_ms).collect()) - median(off.iter().map(|w| w.buf_ms).collect());
    if d.abs() > tol {
        return Err(format!("녹음 중 버퍼 지연 변화(중앙값) {d:+.2}ms > 허용 {tol:.2}ms"));
    }
    type Count = fn(&Window) -> u64;
    let checks: [(&str, Count); 2] = [("xrun", |w| w.xruns), ("은닉", |w| w.concealed)];
    for (what, f) in checks {
        let bad = |ws: &[Window]| ws.iter().filter(|w| f(w) > 0).count();
        let (b_off, b_on) = (bad(off), bad(on));
        if b_on > b_off + BAD_WINDOW_SLACK {
            return Err(format!("녹음 중 {what} 생긴 창 증가: 끔 {b_off}/{} → 켬 {b_on}/{}", off.len(), on.len()));
        }
    }
    Ok(())
}

fn win(buf_ms: f32, concealed: u64, xruns: u64) -> Window {
    Window { buf_ms, concealed, xruns, produced: 800 }
}

/// 판정 규칙의 결정적 검사: #49에서 본 러너 흔들림은 통과, 녹음이 실제로 지연·은닉·xrun을 늘리는 경우는 실패.
#[test]
fn record_compare_judge_tolerates_runner_noise_but_catches_regressions() {
    // #49 실패 로그 수치를 6쌍 교대 창에 흩뿌린 꼴: 버퍼가 6→10ms로 떠다니고 xrun 3개가 한 창에 몰림
    let off = [win(5.9, 1, 0), win(7.4, 0, 3), win(10.3, 0, 0), win(8.4, 0, 0), win(9.9, 0, 1), win(6.0, 0, 0)];
    let on = [win(6.7, 0, 3), win(7.6, 0, 0), win(9.4, 0, 0), win(10.5, 1, 1), win(9.0, 0, 0), win(6.3, 0, 0)];
    assert_eq!(judge(2.5, &off, &on), Ok(()));
    assert_eq!(judge(5.0, &off, &on), Ok(()));
    // 한 창 몰림(러너 한 번 끊김)은 통과
    let quiet = [win(6.0, 0, 0); 6];
    let burst = [win(6.0, 4, 3), win(6.0, 0, 0), win(6.0, 0, 0), win(6.0, 0, 0), win(6.0, 0, 0), win(6.0, 0, 0)];
    assert_eq!(judge(2.5, &quiet, &burst), Ok(()));

    // 녹음이 지터 버퍼를 한 프레임 키움(켬 창마다 +frame_ms)
    for frame_ms in [2.5f32, 5.0] {
        let worse: Vec<_> = off.iter().map(|x| win(x.buf_ms + frame_ms, x.concealed, x.xruns)).collect();
        assert!(judge(frame_ms, &off, &worse).unwrap_err().contains("버퍼 지연"), "{frame_ms}");
    }
    // 켬 창 대부분(4/6)에서 xrun / 은닉 — 창마다 한두 개뿐이어도 잡힌다
    let xr = [win(6.0, 0, 1), win(6.0, 0, 2), win(6.0, 0, 1), win(6.0, 0, 0), win(6.0, 0, 1), win(6.0, 0, 0)];
    assert!(judge(2.5, &quiet, &xr).unwrap_err().contains("xrun"));
    let cc = [win(6.0, 1, 0), win(6.0, 2, 0), win(6.0, 0, 0), win(6.0, 1, 0), win(6.0, 3, 0), win(6.0, 0, 0)];
    assert!(judge(2.5, &quiet, &cc).unwrap_err().contains("은닉"));
}

/// 교대 쌍 수와 창 길이. 6쌍 × 2초 × 2 = 링크당 24초 측정 (예전 끔 8초 + 켬 8초).
const PAIRS: usize = 6;
const WINDOW_MS: u64 = 2_000;

#[test]
#[ignore = "realtime: 녹음 켬/끔 비교(PR 수치). 결정적 짝 engine::record::tests::*, records_local_and_peer_tracks_aligned, record_compare_judge_*"]
fn recording_does_not_change_buffer_concealment_or_xruns() {
    let _g = RT_LOCK.lock();
    // FLAC(P2-4a)은 쓰기 스레드에서 인코딩하므로 CPU를 더 쓴다 — 128 버퍼에서 같은 기준으로 본다.
    for (buf, frame_ms, fmt) in [(128u32, 2.5f32, RecordFormat::WavF32), (256, 5.0, RecordFormat::WavF32), (128, 2.5, RecordFormat::Flac)] {
        let mut ba = sine_backend();
        ba.wake_jitter_us = 1000;
        let mut bb = FakeBackend::silence(48_000);
        bb.wake_jitter_us = 1000;
        bb.seed = 9;
        let (a, b) = pair(config(buf, frame_ms), ba, bb);
        std::thread::sleep(Duration::from_secs(3));
        let dir = tmp(&format!("rt-{buf}-{fmt:?}"));
        let (mut off, mut on) = (Vec::new(), Vec::new());
        let (mut rec_secs, mut rec_bytes, mut dropped) = (0.0, 0u64, 0u64);
        // ABBA(끔·켬, 켬·끔, …): 링크가 천천히 떠다녀도(드리프트) 양쪽에 고르게 나뉜다
        for i in 0..PAIRS {
            for recording in if i % 2 == 0 { [false, true] } else { [true, false] } {
                if !recording {
                    off.push(window(&b, WINDOW_MS));
                    continue;
                }
                b.record_start(RecordOptions { include_mix: true, format: fmt, ..Default::default() }, &dir).unwrap();
                on.push(window(&b, WINDOW_MS));
                let sum = b.recorder().lock().stop().unwrap();
                assert!(sum.error.is_none(), "{:?}", sum.error);
                rec_secs += sum.duration_sec;
                rec_bytes += sum.bytes;
                dropped += sum.dropped_frames;
            }
        }
        let late = b.peer_jitter_stats()[0].1.late;
        let est = b.stats().est_latency_ms;
        drop(b);
        drop(a);
        std::fs::remove_dir_all(&dir).ok();
        let line = |ws: &[Window]| {
            let (c, p) = (ws.iter().map(|w| w.concealed).sum::<u64>(), ws.iter().map(|w| w.produced).sum::<u64>());
            format!(
                "buffer 중앙값 {:.2}ms [{}] 은닉 {c} ({:.2}%) xrun {} [은닉/xrun {}]",
                median(ws.iter().map(|w| w.buf_ms).collect()),
                ws.iter().map(|w| format!("{:.1}", w.buf_ms)).collect::<Vec<_>>().join(" "),
                c as f64 / p.max(1) as f64 * 100.0,
                ws.iter().map(|w| w.xruns).sum::<u64>(),
                ws.iter().map(|w| format!("{}/{}", w.concealed, w.xruns)).collect::<Vec<_>>().join(" "),
            )
        };
        println!(
            "REC {fmt:?} buf {buf} / {frame_ms}ms (estLatency {est:.2}ms, {PAIRS}쌍 × {WINDOW_MS}ms ABBA): 끔 {} | 켬 {} | 녹음 {rec_secs:.2}s {rec_bytes}B 버림 {dropped}프레임 (late {late})",
            line(&off),
            line(&on),
        );
        assert_eq!(dropped, 0, "녹음 링 넘침");
        if let Err(e) = judge(frame_ms, &off, &on) {
            panic!("buf {buf} / {frame_ms}ms: {e}");
        }
    }
}

/// `armed`가 켜진 뒤 A → B 방향 패킷을 `every`개마다 하나씩 버리는 UDP 프록시(결정적 손실). 반환: (A가 B로 보낼 주소, B가 A로 보낼 주소, 정지 플래그).
fn lossy_proxy(a: SocketAddr, b: SocketAddr, every: usize, armed: Arc<std::sync::atomic::AtomicBool>) -> (SocketAddr, SocketAddr, Arc<std::sync::atomic::AtomicBool>) {
    use std::net::UdpSocket;
    use std::sync::atomic::{AtomicBool, Ordering};
    let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
    sa.set_nonblocking(true).unwrap();
    sb.set_nonblocking(true).unwrap();
    let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let ar = armed;
    std::thread::spawn(move || {
        let _prio = band_lib::engine::prio::promote_current_thread();
        let mut buf = [0u8; 2048];
        let mut k = 0usize;
        while !st.load(Ordering::Relaxed) {
            while let Ok((n, _)) = sa.recv_from(&mut buf) {
                k += 1;
                // 연결 수립 뒤(`armed`)에만 버린다
                if ar.load(Ordering::Relaxed) && k.is_multiple_of(every) {
                    continue;
                }
                let _ = sb.send_to(&buf[..n], b);
            }
            while let Ok((n, _)) = sb.recv_from(&mut buf) {
                let _ = sa.send_to(&buf[..n], a);
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    });
    (pa, pb, stop)
}

/// 워커 은닉 마커(P2-4a): A → B 패킷을 주기적으로 잃게 하고 B에서 녹음하면, 피어 트랙 `conceal` 마커의 프레임 합이
/// 지터 버퍼 은닉 카운터 증가분(× 패킷 길이)과 같고, 마커는 녹음 길이 안에 고르게 퍼져 있다.
#[test]
#[ignore = "realtime: 손실 프록시 루프백. 결정적 짝 engine::record::tests::markers_sidecar_maps_events_to_tracks"]
fn concealment_markers_follow_jitter_counter() {
    let _g = RT_LOCK.lock();
    let cfg = config(256, 5.0);
    let a = Engine::start(cfg.clone(), Arc::new(sine_backend()), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(cfg, Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (to_b, to_a, stop) = lossy_proxy(local_addr(&a), local_addr(&b), 25, armed.clone());
    a.add_peer("bob".into(), &with_candidate(b.descriptor().unwrap(), to_b)).unwrap();
    b.add_peer("alice".into(), &with_candidate(a.descriptor().unwrap(), to_a)).unwrap();
    assert!(wait_connected(&a) && wait_connected(&b), "연결 실패");
    std::thread::sleep(Duration::from_millis(1000));
    armed.store(true, std::sync::atomic::Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(500));
    let dir = tmp("conceal");
    let j0 = b.peer_jitter_stats()[0].1;
    b.record_start(RecordOptions::default(), &dir).unwrap();
    std::thread::sleep(Duration::from_secs(4));
    let sum = b.recorder().lock().stop().unwrap();
    let j1 = b.peer_jitter_stats()[0].1;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let folder = PathBuf::from(&sum.folder);
    let mj: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("markers.json")).unwrap()).unwrap();
    let peer = mj["tracks"].as_array().unwrap().iter().find(|t| t["kind"] == "peer").expect("피어 트랙").clone();
    let ms = peer["markers"].as_array().unwrap();
    let conceal: Vec<_> = ms.iter().filter(|m| m["kind"] == "conceal").collect();
    let frames: u64 = conceal.iter().map(|m| m["frames"].as_u64().unwrap()).sum();
    let events: u64 = conceal.iter().map(|m| m["events"].as_u64().unwrap()).sum();
    let fs = 240u64; // 5ms @ 48k
    println!(
        "은닉 패킷 {} (녹음 전후 카운터), 마커 구간 {} / 이벤트 {} / 프레임 {} (= {} 패킷), 녹음 {:.2}s, 잃은 이벤트 {}",
        j1.concealed - j0.concealed,
        conceal.len(),
        events,
        frames,
        frames / fs,
        sum.duration_sec,
        mj["lostEvents"]
    );
    assert!(!conceal.is_empty(), "손실이 있는데 은닉 마커가 없다: {mj}");
    // 녹음 시작·정지 경계의 몇 패킷 차이만 허용
    let counted = j1.concealed - j0.concealed;
    assert!((frames / fs).abs_diff(counted) <= 4, "마커 {} 패킷 vs 카운터 {counted}", frames / fs);
    let end = (sum.duration_sec * 48_000.0) as u64;
    assert!(conceal.iter().all(|m| m["end"].as_u64().unwrap() <= end));
    // 25개마다 1개 손실(5ms 패킷 → 125ms마다) → 100ms 병합 간격보다 넓으니 구간이 여럿
    assert!(conceal.len() >= 10, "{}", conceal.len());
    drop(b);
    drop(a);
    std::fs::remove_dir_all(&dir).ok();
}

/// 녹음 켬/끔 순간 xrun(P2-4b). 같은 링크에서 1초 주기 20회를 ABBA(대조·녹음, 녹음·대조, …)로 교대한다. 녹음 주기는
/// 400ms 뒤 녹음 시작, 600ms 뒤 정지. 대조 주기는 같은 박자에 아무것도 하지 않는다. xrun 시각을 1ms 간격으로 잡아
/// 시작·정지 시각과 대조한다. 판정: 녹음 시작·정지 직후 창(앞 5ms ~ 호출이 끝난 뒤 50ms)에 든 xrun이 0개, 녹음 주기
/// xrun 합이 대조 주기 합 + 2 이하(PR #64 판정과 같은 여유).
///
/// 배경: PR #64 ABBA 비교에서 128/2.5ms 켬 창 하나에 xrun이 늘 1개 나왔다. 원인은 녹음이 아니라 가짜 장치의 고정 시드
/// 기상 지터(`bb.seed = 9`)였다 — 녹음을 전혀 켜지 않아도 엔진 시작 약 22초 뒤 같은 시각에 xrun 1개가 나고(시드를 바꾸면
/// 시각이 옮겨감), 그 시각이 ABBA 순서상 늘 켬 창에 떨어졌다. 녹음 시작·정지 순간에 몰린 xrun은 없었다(PR 본문 수치).
#[test]
#[ignore = "realtime: 녹음 켬/끔 반복 xrun 시각 대조(PR 수치). 결정적 짝 tests/rt_alloc.rs rt_callbacks_do_not_allocate(녹음 핸드오프 무할당), engine::record::tests::prefaulted_rings_start_empty"]
fn record_toggle_adds_no_xruns_at_start_or_stop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let _g = RT_LOCK.lock();
    const CYCLES: usize = 10;
    for fmt in [RecordFormat::WavF32, RecordFormat::Flac] {
        let mut ba = sine_backend();
        ba.wake_jitter_us = 1000;
        let mut bb = FakeBackend::silence(48_000);
        bb.wake_jitter_us = 1000;
        bb.seed = 9;
        let (a, b) = pair(config(128, 2.5), ba, bb);
        let b = Arc::new(b);
        std::thread::sleep(Duration::from_secs(2));
        let t0 = Instant::now();
        let ms = move || t0.elapsed().as_secs_f64() * 1000.0;
        let stop = Arc::new(AtomicBool::new(false));
        let (bm, st) = (b.clone(), stop.clone());
        let mon = std::thread::spawn(move || {
            let mut ev = vec![];
            let mut last = bm.stats().xruns;
            while !st.load(Ordering::Relaxed) {
                let x = bm.stats().xruns;
                if x != last {
                    ev.push((ms(), x - last));
                    last = x;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            ev
        });
        let dir = tmp(&format!("toggle-{fmt:?}"));
        // 시작·정지 경계 (호출 전, 호출 끝, 녹음 주기인가)와 주기 구간 (시작, 끝, 녹음 주기인가)
        let mut edges: Vec<(f64, f64, bool)> = vec![];
        let mut cycles: Vec<(f64, f64, bool)> = vec![];
        let mut start_ms: Vec<f64> = vec![];
        let mut stop_ms: Vec<f64> = vec![];
        for i in 0..CYCLES * 2 {
            // ABBA: 링크·가짜 장치 지터가 천천히 떠다녀도 양쪽에 고르게 나뉜다
            let rec = matches!(i % 4, 1 | 2);
            let c0 = ms();
            std::thread::sleep(Duration::from_millis(400));
            let s = ms();
            if rec {
                b.record_start(RecordOptions { include_mix: true, format: fmt, ..Default::default() }, &dir).unwrap();
                start_ms.push(ms() - s);
            }
            edges.push((s, ms(), rec));
            std::thread::sleep(Duration::from_millis(600));
            let s = ms();
            if rec {
                // 앱(`engine_record_stop`)과 같은 3단계: 마무리는 recorder 락 밖에서. 락을 쥔 채 마무리하면 감시 스레드의
                // `stats()`가 그동안 막혀 그 사이 xrun이 정지 끝 시각으로 찍힌다(측정 편향).
                let job = b.recorder().lock().begin_stop().unwrap();
                let sum = job.finish().unwrap();
                b.recorder().lock().reclaim();
                assert_eq!(sum.dropped_frames, 0, "녹음 링 넘침");
                stop_ms.push(ms() - s);
            }
            edges.push((s, ms(), rec));
            cycles.push((c0, ms(), rec));
        }
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::Relaxed);
        let ev = mon.join().unwrap();
        let near = |rec: bool| -> u64 {
            ev.iter().filter(|(t, _)| edges.iter().any(|&(s, e, r)| r == rec && *t >= s - 5.0 && *t <= e + 50.0)).map(|e| e.1).sum()
        };
        // 정지 직후 50ms는 다음 주기로 넘어갈 수 있으나 경계 창 판정이 따로 본다
        let total = |rec: bool| -> u64 { ev.iter().filter(|(t, _)| cycles.iter().any(|&(s, e, r)| r == rec && *t >= s && *t < e)).map(|e| e.1).sum() };
        let (n_ctl, n_rec, a_ctl, a_rec) = (near(false), near(true), total(false), total(true));
        let max = |v: &[f64]| v.iter().cloned().fold(0.0, f64::max);
        println!(
            "TOGGLE {fmt:?} 128/2.5ms {CYCLES}+{CYCLES}회 ABBA: 대조(녹음 없음) xrun {a_ctl} (경계 창 {n_ctl}) | 녹음 켬/끔 xrun {a_rec} (경계 창 {n_rec}) | 시작 호출 최대 {:.2}ms, 정지 호출 최대 {:.2}ms | xrun 시각 {:?}",
            max(&start_ms),
            max(&stop_ms),
            ev.iter().map(|e| e.0.round() as i64).collect::<Vec<_>>()
        );
        drop(b);
        drop(a);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(n_rec, 0, "녹음 시작·정지 직후 xrun {n_rec}개: {ev:?} / {edges:?}");
        assert!(a_rec <= a_ctl + 2, "녹음 켬/끔 구간 xrun 증가: {a_ctl} → {a_rec}");
    }
}
