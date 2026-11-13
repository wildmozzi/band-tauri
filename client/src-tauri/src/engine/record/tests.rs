//! `engine::record` 단위 테스트(옛 `record.rs`의 `mod tests`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer};

use super::marks::*;
use super::rt_ring::*;
use super::sidecar::*;
use super::writer::*;
use crate::diag::mask::pseudonymize;
use crate::engine::config::LOCAL_SOURCE;
use crate::engine::wav::RecordFormat;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("band-rec-test-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    d
}

/// 정지하는 동안 콜백처럼 `poll`을 돌려 끝점을 돌려받게 한다.
fn stop_with_rt(rec: &mut Recorder, port_rt: RecordPortRt) -> (RecordSummary, RecordPortRt) {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    let h = std::thread::spawn(move || {
        let mut p = port_rt;
        while !d.load(Ordering::Relaxed) {
            p.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        p
    });
    let sum = rec.stop().unwrap();
    done.store(true, Ordering::Relaxed);
    (sum, h.join().unwrap())
}

/// 24비트 스테레오 WAV를 표준 파서(hound)로 읽는다(EXTENSIBLE 헤더 확인을 겸함, PR #47 L3). 반환: (프레임 수, 인터리브 샘플).
fn data_frames(path: &Path) -> (usize, Vec<i32>) {
    let mut r = hound::WavReader::open(path).unwrap();
    assert_eq!((r.spec().channels, r.spec().bits_per_sample), (2, 24));
    let s: Vec<i32> = r.samples::<i32>().map(|v| v.unwrap()).collect();
    (s.len() / 2, s)
}

/// 콜백을 흉내 낸다: 로컬은 처음부터, 피어(ssrc 7)는 480프레임 뒤부터, 한 청크는 링 부족으로 버린 것처럼 건너뛴다.
#[test]
fn tracks_share_timeline_and_pad_late_peer() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("align");
    let opts = RecordOptions {
        include_mix: true,
        name: Some("세션: 1".into()),
        track_names: HashMap::from([("peer-a".to_string(), "기타".to_string())]),
        ..Default::default()
    };
    let resolve: PeerResolver = Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string()));
    let st = rec.start(opts, &dir, resolve, 0).unwrap();
    assert!(st.folder.ends_with("세션_ 1"), "{}", st.folder);
    assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_err());

    let n = 240usize;
    let local = vec![0.25f32; n * 2];
    let peer = vec![-0.5f32; n * 2];
    let mix = vec![0.125f32; n * 2];
    let base = 1_000_000u64; // 녹음 시작 전부터 돌던 출력 타임라인
    for k in 0..6u64 {
        port_rt.poll();
        let r = port_rt.active().expect("녹음 끝점");
        if k == 3 {
            continue; // 버린 청크(블록 없음)
        }
        r.begin(base + k * n as u64, n);
        r.source(LOCAL_SOURCE, 0, &local, 0);
        if k >= 2 {
            r.source(1, 7, &peer, 0);
        }
        r.finish(&mix);
    }
    let (sum, mut port_rt) = stop_with_rt(&mut rec, port_rt);
    assert!(port_rt.active().is_none(), "끝점을 돌려줬다");
    assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_ok(), "돌려받았으니 다시 시작된다");
    rec.stop().unwrap();
    assert!(sum.error.is_none(), "{:?}", sum.error);
    assert_eq!(sum.tracks.len(), 3);
    assert!((sum.duration_sec - 6.0 * n as f64 / 48_000.0).abs() < 1e-9);

    let folder = PathBuf::from(&sum.folder);
    let me = folder.join(&sum.tracks.iter().find(|t| t.kind == TrackKind::Local).unwrap().file);
    let gt = sum.tracks.iter().find(|t| t.kind == TrackKind::Peer).unwrap();
    assert_eq!(gt.name, "기타");
    // 사이드카·반환의 피어 id는 진단 내보내기와 같은 가명(PR #62 L5), 닉네임은 그대로
    assert_eq!(gt.peer_id.as_deref(), Some(pseudonymize("peer-a").as_str()));
    let sj = std::fs::read_to_string(folder.join("session.json")).unwrap();
    assert!(!sj.contains("peer-a") && sj.contains(&pseudonymize("peer-a")) && sj.contains("기타"), "{sj}");
    let (f_me, s_me) = data_frames(&me);
    let (f_peer, s_peer) = data_frames(&folder.join(&gt.file));
    assert_eq!(f_me, 6 * n);
    assert_eq!(f_peer, 6 * n, "모든 트랙 길이가 같다");
    // 피어 앞 2청크는 무음, 3번째 청크부터 소리
    assert!(s_peer[..2 * n * 2].iter().all(|&v| v == 0));
    assert!(s_peer[2 * n * 2] < -4_000_000);
    // 버린 청크(3) 자리는 모든 트랙에서 무음
    assert!(s_me[3 * n * 2..4 * n * 2].iter().all(|&v| v == 0));
    assert!(s_me[4 * n * 2] > 2_000_000);
    assert!(folder.join("session.json").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #49 M1: 녹음 중에 이름을 더하면 이후 트랙은 그 이름으로 만들고, 이름 없이 만든 트랙은 정지 때 파일 이름을 바꾼다.
#[test]
fn late_track_names_apply() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("latenames");
    assert!(rec.set_track_names(HashMap::new()).is_err(), "녹음 중이 아니면 에러");
    let resolve: PeerResolver = Box::new(|ssrc| match ssrc {
        7 => Some("peer-aaaaaaaaaa".to_string()),
        9 => Some("peer-b".to_string()),
        _ => None,
    });
    rec.start(RecordOptions::default(), &dir, resolve, 0).unwrap();
    let n = 240usize;
    let data = vec![0.25f32; n * 2];
    let feed = |port_rt: &mut RecordPortRt, k: u64, peers: &[(usize, u32)]| {
        port_rt.poll();
        let r = port_rt.active().expect("녹음 끝점");
        r.begin(k * n as u64, n);
        r.source(LOCAL_SOURCE, 0, &data, 0);
        for &(slot, ssrc) in peers {
            r.source(slot, ssrc, &data, 0);
        }
        r.finish(&data);
    };
    for k in 0..2 {
        feed(&mut port_rt, k, &[(1, 7)]);
    }
    // 쓰기 스레드가 이름 없는 트랙을 만들 때까지 기다린다
    let t0 = Instant::now();
    while rec.stats().map(|s| s.tracks).unwrap_or(0) < 2 && t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    rec.set_track_names(HashMap::from([("peer-aaaaaaaaaa".to_string(), "베이스".to_string()), ("peer-b".to_string(), "드럼".to_string())])).unwrap();
    for k in 2..4 {
        feed(&mut port_rt, k, &[(1, 7), (2, 9)]);
    }
    let (sum, _port_rt) = stop_with_rt(&mut rec, port_rt);
    assert!(sum.error.is_none(), "{:?}", sum.error);
    let peers: Vec<(&str, &str)> = sum.tracks.iter().filter(|t| t.kind == TrackKind::Peer).map(|t| (t.name.as_str(), t.file.as_str())).collect();
    assert_eq!(peers, vec![("베이스", "02-베이스.wav"), ("드럼", "03-드럼.wav")]);
    let folder = PathBuf::from(&sum.folder);
    assert!(!folder.join(format!("02-{}.wav", pseudonymize("peer-aaaaaaaaaa"))).exists());
    for t in &sum.tracks {
        assert_eq!(data_frames(&folder.join(&t.file)).0, 4 * n, "{}", t.file);
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #47 M2: 트랙 하나를 만들지 못해도(같은 이름 폴더가 막음) 나머지 트랙은 끝까지 쓴다.
#[test]
fn one_track_create_failure_keeps_others() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("trackfail");
    let opts = RecordOptions {
        include_mix: true,
        track_names: HashMap::from([("peer-a".to_string(), "blocker".to_string())]),
        ..Default::default()
    };
    let st = rec.start(opts, &dir, Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string())), 0).unwrap();
    std::fs::create_dir_all(PathBuf::from(&st.folder).join("02-blocker.wav")).unwrap();
    let n = 240usize;
    let data = vec![0.25f32; n * 2];
    for k in 0..4u64 {
        port_rt.poll();
        let r = port_rt.active().expect("녹음 끝점");
        r.begin(k * n as u64, n);
        r.source(LOCAL_SOURCE, 0, &data, 0);
        r.source(1, 7, &data, 0);
        r.finish(&data);
    }
    let (sum, _port_rt) = stop_with_rt(&mut rec, port_rt);
    let err = sum.error.clone().expect("트랙 실패를 알린다");
    assert!(err.contains("02-blocker.wav"), "{err}");
    let kinds: Vec<TrackKind> = sum.tracks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, vec![TrackKind::Local, TrackKind::Mix]);
    let folder = PathBuf::from(&sum.folder);
    for t in &sum.tracks {
        assert_eq!(data_frames(&folder.join(&t.file)).0, 4 * n, "{}", t.file);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stop_without_callback_times_out_but_finishes() {
    let (_port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("dead");
    rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
    let sum = rec.stop().unwrap();
    assert_eq!(sum.duration_sec, 0.0);
    assert!(rec.stop().is_err());
    // 콜백이 끝점을 아직 안 받았으므로 다음 시작은 거부된다(링을 두 번 넘기지 않음)
    assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn full_ring_drops_whole_chunk() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 100); // 링 300프레임
    let dir = tmp("drop");
    rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
    port_rt.poll();
    let r = port_rt.active().unwrap();
    let data = vec![0.1f32; 200];
    let mut pushed = 0;
    for k in 0..100u64 {
        r.begin(k * 100, 100);
        r.source(0, 0, &data, 0);
        r.finish(&data);
        pushed += 1;
    }
    let dropped = r.shared.dropped_frames.load(Ordering::Relaxed);
    assert!(dropped > 0 && dropped < pushed * 100, "{dropped}");
    let (sum, _) = stop_with_rt(&mut rec, port_rt);
    assert_eq!(sum.dropped_frames, dropped);
    std::fs::remove_dir_all(&dir).ok();
}

/// P2-4b: 미리 건드린 링은 비어 있고(가득 빈 자리) 그대로 RT 쓰기·쓰기 스레드 읽기에 쓸 수 있다.
#[test]
fn prefaulted_rings_start_empty() {
    let (mut s_p, mut s_c, mut b_p, mut b_c) = alloc_rings(100, true);
    assert_eq!(s_p.vacant_len(), s_p.capacity().get());
    assert_eq!(s_c.occupied_len(), 0);
    assert_eq!(b_p.vacant_len(), b_p.capacity().get());
    assert_eq!(b_c.occupied_len(), 0);
    let data: Vec<f32> = (0..64).map(|i| i as f32).collect();
    assert_eq!(s_p.push_slice(&data), 64);
    let mut out = [0f32; 64];
    assert_eq!(s_c.pop_slice(&mut out), 64);
    assert_eq!(&out[..], &data[..]);
    assert!(b_p.try_push(RecBlock { pos: 7, frames: 3, ..Default::default() }).is_ok());
    assert_eq!(b_c.try_pop().map(|b| (b.pos, b.frames)), Some((7, 3)));
}

/// P2-4b 측정: 출력 콜백이 녹음 링에 쓰는 비용 — 새로 할당한 링(첫 바퀴 = 페이지 폴트) vs 미리 건드린 링.
/// 48k, 128프레임 청크, 슬롯 2개 + 믹스, 링 한 바퀴 넘게. `cargo test --release --lib prefault_rt_write_cost -- --ignored --nocapture`
#[test]
#[ignore = "측정(벽시계)"]
fn prefault_rt_write_cost() {
    for prefault in [false, true, false, true] {
        let t_alloc = Instant::now();
        let (s_p, mut s_c, b_p, mut b_c) = alloc_rings(48_000, prefault);
        let alloc_ms = t_alloc.elapsed().as_secs_f64() * 1e3;
        let shared = Arc::new(RecordRtShared::default());
        let mut r = RecordRt { samples: s_p, blocks: b_p, include_mix: true, shared: shared.clone(), cur: RecBlock::default(), open: false };
        let n = 128usize;
        let data = vec![0.1f32; n * 2];
        let chunks = s_c.capacity().get() / (n * 2 * 3) + 200;
        let (mut total, mut max) = (Duration::ZERO, Duration::ZERO);
        let mut over_20us = 0;
        for k in 0..chunks {
            let t = Instant::now();
            r.begin(k as u64 * n as u64, n);
            r.source(0, 0, &data, 0);
            r.source(1, 7, &data, 0);
            r.finish(&data);
            let d = t.elapsed();
            total += d;
            max = max.max(d);
            over_20us += usize::from(d > Duration::from_micros(20));
            s_c.skip(s_c.occupied_len());
            b_c.skip(b_c.occupied_len());
        }
        assert_eq!(shared.dropped_frames.load(Ordering::Relaxed), 0);
        println!(
            "prefault={prefault}: 준비 {alloc_ms:.2}ms, 청크 {chunks}개 RT 쓰기 합 {:.2}ms 평균 {:.2}us 최대 {:.1}us, 20us 넘은 청크 {over_20us}",
            total.as_secs_f64() * 1e3,
            total.as_secs_f64() * 1e6 / chunks as f64,
            max.as_secs_f64() * 1e6
        );
    }
}

/// P2-4a: FLAC 녹음 — 트랙 파일이 `.flac`이고 표준 디코더(claxon)로 열리며, 타임라인 정렬(늦은 피어 앞 무음)과
/// 이름 바꾸기가 WAV와 같다.
#[test]
fn flac_tracks_decode_and_align() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("flac");
    let opts = RecordOptions { format: RecordFormat::Flac, include_mix: true, ..Default::default() };
    rec.start(opts, &dir, Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string())), 0).unwrap();
    let n = 240usize;
    let local = vec![0.25f32; n * 2];
    let peer = vec![-0.5f32; n * 2];
    for k in 0..40u64 {
        port_rt.poll();
        let r = port_rt.active().expect("녹음 끝점");
        r.begin(k * n as u64, n);
        r.source(LOCAL_SOURCE, 0, &local, 0);
        if k >= 20 {
            r.source(1, 7, &peer, 0);
        }
        r.finish(&local);
    }
    rec.set_track_names(HashMap::from([("peer-a".to_string(), "기타".to_string())])).unwrap();
    let (sum, _p) = stop_with_rt(&mut rec, port_rt);
    assert!(sum.error.is_none(), "{:?}", sum.error);
    let files: Vec<&str> = sum.tracks.iter().map(|t| t.file.as_str()).collect();
    assert_eq!(files, vec!["01-me.flac", "00-mix.flac", "03-기타.flac"]);
    let folder = PathBuf::from(&sum.folder);
    for t in &sum.tracks {
        let mut r = claxon::FlacReader::open(folder.join(&t.file)).expect("claxon으로 열린다");
        assert_eq!(r.streaminfo().samples, Some(40 * n as u64), "{}", t.file);
        assert_eq!(r.streaminfo().bits_per_sample, 24);
        let s: Vec<i32> = r.samples().map(|x| x.unwrap()).collect();
        assert_eq!(s.len(), 40 * n * 2);
        if t.kind == TrackKind::Peer {
            assert!(s[..20 * n * 2].iter().all(|&v| v == 0), "늦은 피어 앞은 무음");
            assert!(s[20 * n * 2..].iter().all(|&v| v == -4_194_304));
        } else {
            assert!(s.iter().all(|&v| v == 2_097_152));
        }
    }
    assert!(sum.bytes > 0 && sum.bytes < 40 * n as u64 * 6 * 3, "{}", sum.bytes);
    std::fs::remove_dir_all(&dir).ok();
}

/// P2-4a: 언더런(청크 머리 `short`)과 워커 마커(은닉·버림)가 트랙별 구간으로 `markers.json`에 남는다.
/// 가까운 같은 종류는 합치고, 녹음 시작 기준 프레임 위치이며, 녹음 밖 이벤트는 버린다.
#[test]
fn markers_sidecar_maps_events_to_tracks() {
    let (mut port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let marks = rec.marks();
    assert!(!marks.active());
    let dir = tmp("marks");
    let resolve: PeerResolver = Box::new(|ssrc| match ssrc {
        7 => Some("peer-a".to_string()),
        9 => Some("peer-b".to_string()),
        _ => None,
    });
    let opts = RecordOptions { include_mix: true, track_names: HashMap::from([("peer-a".to_string(), "기타".to_string())]), ..Default::default() };
    rec.start(opts, &dir, resolve, 0).unwrap();
    assert!(marks.active());
    let base = 500_000u64;
    let n = 480usize;
    let data = vec![0.1f32; n * 2];
    for k in 0..200u64 {
        port_rt.poll();
        let r = port_rt.active().expect("녹음 끝점");
        r.begin(base + k * n as u64, n);
        r.source(LOCAL_SOURCE, 0, &data, if k == 10 { 100 } else { 0 });
        // 피어 a: 50번째 청크 끝 30프레임 언더런
        r.source(1, 7, &data, if k == 50 { 30 } else { 0 });
        r.source(2, 9, &data, 0);
        r.finish(&data);
    }
    let at = |k: u64| base + k * n as u64;
    // 피어 a 은닉 두 번(1.0초 근처, 20ms 간격 → 한 구간) + 멀리 하나, 피어 b 버림, 녹음 앞·뒤 이벤트
    marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(100), frames: 480 });
    marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(102), frames: 480 });
    marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(150), frames: 240 });
    marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Drop, start: at(120), frames: 960 });
    marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Conceal, start: at(500), frames: 480 });
    marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Conceal, start: base - 1000, frames: 1480 });
    let (sum, _p) = stop_with_rt(&mut rec, port_rt);
    assert!(!marks.active(), "정지하면 마커를 받지 않는다");
    assert!(sum.error.is_none(), "{:?}", sum.error);
    assert_eq!(sum.markers_file.as_deref(), Some("markers.json"));
    let folder = PathBuf::from(&sum.folder);
    let j: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("markers.json")).unwrap()).unwrap();
    assert_eq!(j["sampleRate"], 48_000);
    assert_eq!(j["lostEvents"], 0);
    assert_eq!(j["overflowEvents"], 0);
    let track = |file: &str| j["tracks"].as_array().unwrap().iter().find(|t| t["file"] == file).cloned().unwrap_or_else(|| panic!("{file}: {j}"));
    let ms = |t: &serde_json::Value| -> Vec<(String, u64, u64, u64, u64)> {
        t["markers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["kind"].as_str().unwrap().to_string(), m["start"].as_u64().unwrap(), m["end"].as_u64().unwrap(), m["frames"].as_u64().unwrap(), m["events"].as_u64().unwrap()))
            .collect()
    };
    let nn = n as u64;
    assert_eq!(ms(&track("01-me.wav")), vec![("underrun".into(), 11 * nn - 100, 11 * nn, 100, 1)]);
    let a = track("02-기타.wav");
    assert_eq!(a["peerId"], pseudonymize("peer-a"));
    assert_eq!(
        ms(&a),
        vec![("underrun".into(), 51 * nn - 30, 51 * nn, 30, 1), ("conceal".into(), 100 * nn, 103 * nn, 960, 2), ("conceal".into(), 150 * nn, 150 * nn + 240, 240, 1),]
    );
    assert!((a["markers"][1]["startSec"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    // 피어 b: 녹음 앞 은닉은 0부터 잘리고, 녹음 뒤(500청크)는 버린다. 버림은 길이 없는 지점.
    // 닉네임이 없는 피어 b는 트랙 이름·파일도 가명(원문 id 앞부분을 남기지 않음, PR #62 L5)
    let b = track(&format!("03-{}.wav", pseudonymize("peer-b")));
    assert_eq!(b["name"], pseudonymize("peer-b"));
    assert_eq!(b["peerId"], pseudonymize("peer-b"));
    let raw = std::fs::read_to_string(folder.join("markers.json")).unwrap() + &std::fs::read_to_string(folder.join("session.json")).unwrap();
    assert!(!raw.contains("peer-a") && !raw.contains("peer-b"), "사이드카에 피어 id 원문이 없다");
    assert_eq!(ms(&b), vec![("conceal".into(), 0, 480, 1480, 1), ("drop".into(), 120 * nn, 120 * nn, 960, 1)]);
    assert!(ms(&track("00-mix.wav")).is_empty(), "믹스 트랙에는 마커가 없다");
    assert_eq!(sum.markers, 6);
    std::fs::remove_dir_all(&dir).ok();
}

/// PR #47 L4: 락 밖에서 마무리 중인 정지가 있으면 `waiter`가 그것이 끝날 때까지 기다린다.
#[test]
fn waiter_waits_for_inflight_stop() {
    let (_port_rt, ctl) = record_port();
    let mut rec = Recorder::new(ctl, 48_000);
    let dir = tmp("inflight");
    let wait = rec.waiter();
    assert!(wait(Duration::from_millis(1)), "정지 중인 것이 없으면 바로 끝");
    rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
    let job = rec.begin_stop().unwrap();
    assert!(!wait(Duration::from_millis(20)), "마무리 전에는 기다린다");
    let h = std::thread::spawn(move || job.finish()); // 콜백이 없어 1초 뒤 시간 초과로 마무리
    assert!(wait(Duration::from_secs(5)));
    assert!(h.join().unwrap().is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn names_are_sanitized() {
    assert_eq!(sanitize_name("a/b:c*?"), "a_b_c__");
    assert_eq!(sanitize_name("  con.txt "), "_con.txt");
    assert_eq!(sanitize_name("trail... "), "trail");
    assert_eq!(sanitize_name("..\\x"), "_x");
    assert_eq!(default_session_name(1_762_819_200_000), "band-rec-20251111-000000Z");
}
