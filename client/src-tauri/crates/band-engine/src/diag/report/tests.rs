//! 진단 보고서 시험: 실제 내보내기 경로(`export::build`)로 만든 작은 zip, 손으로 쓴 옛 형식 zip, 잘못된 zip.
//! 큰 바이너리 fixture는 두지 않는다 — 시험 안에서 만든다.

use std::io::Write;

use super::*;
use crate::diag::export::{build, AppInfo};
use crate::diag::timeline::tests::stats_with_peer;
use crate::diag::Diagnostics;
use crate::engine::report::EngineSnapshot;
use crate::net::connlog::{ConnEvent, ConnOutcome, ConnPhase};
use band_dsp::types::PathKind;

const T0: u64 = 1_794_388_000_000;
const PEER: &str = "8a1f0c2e-1111-2222-3333-444455556666";

fn ev(seq: u64, at_ms: u64, phase: ConnPhase, outcome: ConnOutcome, ms: u32) -> ConnEvent {
    ConnEvent { seq, at_ms, peer_id: PEER.into(), phase, outcome, ms }
}

/// 120초 합주(WASAPI 독점, LAN 피어 1명): bufferMs 5·5.25·5.5·5.75·6 반복, 은닉 1/s(F 5ms → 0.5%), 확정 손실 1/2s,
/// 50초에 xrun +2, 0초에 connect→lan 120ms.
fn sample_diag(session: u64, events: Vec<ConnEvent>) -> Diagnostics {
    let d = Diagnostics::new(4000);
    for i in 0..120u64 {
        let mut stats = stats_with_peer();
        stats.backend = "wasapiExclusive".into();
        stats.est_latency_ms = 7.5;
        stats.device_buffer = 128;
        stats.sample_rate = 48_000;
        stats.xruns = if i >= 50 { 2 } else { 0 };
        let p = &mut stats.peers[0];
        p.path = Some(PathKind::Lan);
        p.buffer_ms = 5.0 + (i % 5) as f32 * 0.25;
        p.jitter_ms = 1.5;
        p.rtt_ms = Some(1.2);
        p.concealed = i;
        p.produced = i * 200;
        p.target_ms = 5.5;
        p.loss_shape = band_dsp::jitter::LossShapeStats {
            frame_ms: 5.0,
            loss_runs: [i / 2, 0, 0, 0, 0, 0],
            conceal_plc: i,
            arrival_margin: [0, 0, 0, 0, 0, 0, 0, 200 * i, 0, 0],
            ..Default::default()
        };
        let conn_events = if i == 0 { events.clone() } else { Vec::new() };
        let config = band_dsp::config::EngineConfig { frame_ms: 5.0, jitter_min_ms: Some(0.0), jitter_max_ms: Some(80.0), ..Default::default() };
        d.record(T0 + i * 1000, Some(EngineSnapshot { stats, config, network: true, session, conn_events, nat: None }));
    }
    d.record(T0 + 120_000, None);
    d
}

fn export(d: &Diagnostics) -> Vec<u8> {
    let app = AppInfo { name: "Band".into(), version: "9.9.9".into() };
    build(None, Ok(Vec::new()), d, app, None, T0 + 121_000).unwrap()
}

fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn goal_of<'a>(rows: &'a [GoalRow], id: &str) -> Vec<&'a GoalRow> {
    rows.iter().filter(|r| r.id == id).collect()
}

/// 진단 형식 계약: 실제 내보내기로 만든 zip을 읽어 핵심 수치가 기대값과 같다. 타임라인 필드 이름이 바뀌면 여기서 깨진다.
#[test]
fn report_reads_real_export() {
    let d = sample_diag(3, vec![ev(1, T0 - 200, ConnPhase::Connect, ConnOutcome::Lan, 120)]);
    let diag = read_zip_bytes("a.zip", export(&d)).unwrap();
    let r = report(&[diag]);
    let f = &r.files[0];
    assert_eq!((f.format, f.app_version.as_deref()), (Some(3), Some("9.9.9")));
    assert_eq!(f.sessions.len(), 1);
    let s = &f.sessions[0];
    assert_eq!((s.session, s.network, s.running_samples), (3, Some(true), 120));
    assert_eq!(s.duration_s, 119.0);
    assert_eq!(s.backend.as_deref(), Some("wasapiExclusive"));
    assert_eq!((s.device_buffer, s.sample_rate), (Some(128), Some(48_000)));
    let c = s.config.as_ref().expect("마지막 세션 설정");
    assert_eq!((c.preset.as_str(), c.frame_ms, c.input_driver.as_str()), ("균형", 5.0, "wasapiShared"));
    assert_eq!(s.est_latency_ms.as_ref().map(|d| d.median), Some(7.5));
    assert_eq!(s.xruns, Some(2));
    assert_eq!(s.xrun_steps, vec![XrunStep { at_s: 50.0, add: 2 }]);

    assert_eq!(s.peers.len(), 1);
    let p = &s.peers[0];
    assert!(p.peer.starts_with("id-"), "가명: {}", p.peer);
    assert_eq!((p.entries, p.connected_samples), (1, 120));
    assert_eq!(p.paths.get("lan"), Some(&120));
    assert_eq!(p.buffer_ms, Some(Dist { median: 5.5, p95: 6.0, max: 6.0 }));
    assert_eq!(p.frame_ms, Some(5.0));
    let b = p.band.as_ref().expect("targetMs 기록(#246)");
    assert_eq!((b.over_limit, b.covered_s, b.gaps), (0, 120.0, 0));
    let cc = p.conceal.as_ref().unwrap();
    // 재생 23800프레임(produced 차분), 은닉 119 → 0.5%, 확정 손실 59 → 0.248%
    assert_eq!((cc.produced, cc.concealed, cc.dup_dropped), (Some(23_800), 119, Some(0)));
    assert!((cc.pct.unwrap() - 0.5).abs() < 1e-9, "{:?}", cc.pct);
    assert!((cc.plc_pct.unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(cc.fec_pct, Some(0.0));
    assert!((cc.loss_pct.unwrap() - 59.0 * 100.0 / 23_800.0).abs() < 1e-9);
    assert_eq!(p.connect_ms, vec![120.0]);
    assert_eq!(p.first_audio_ms, vec![180.5]);
    assert_eq!(p.loss_shape.len(), 1);
    assert_eq!(p.loss_shape[0].loss_runs[0], 59);
    assert_eq!(s.conn, vec![ConnRow { at_s: 0.0, peer: p.peer.clone(), phase: "connect".into(), result: "lan".into(), ms: Some(120.0) }]);

    let v = |id: &str| goal_of(&s.goal, id).iter().map(|r| r.verdict).collect::<Vec<_>>();
    assert_eq!(v("1-1"), vec![Verdict::Met]); // 독점 7.5 ≤ 15
    assert_eq!(v("1-3"), vec![Verdict::Met]); // 5.5 ≤ 5 + 1.5
    assert_eq!(v("1-4"), vec![Verdict::Unknown]); // 피어 기록 2분 < 60분
    assert_eq!(v("2-1"), vec![Verdict::Unknown]); // 은닉 0.5% ≤ 1.5%지만 손실 0.25% — 1% 회선이 아니다
    assert_eq!(v("2-2"), vec![Verdict::NotMet]); // 독점에서 xrun 2

    assert_eq!((r.conn.attempts, r.conn.success, r.conn.direct), (1, 1, 1));
    let g = |id: &str| goal_of(&r.goal, id)[0].verdict;
    assert_eq!(g("3-1"), Verdict::Unknown); // NAT 추정 없음(스냅샷 nat: None)
    assert_eq!(g("3-2"), Verdict::Unknown); // 시도 1회 < 50
    // format 3: 입장 → 추가 812ms + 첫 소리 180.5ms = 992.5ms ≤ 3초, 모든 항목에 입장 값이 있다 → 충족
    assert_eq!(p.entry_first_audio_ms, vec![992.5]);
    assert_eq!(g("3-3"), Verdict::Met);
    // 리버퍼 카운터(2회·431.3ms)가 세션 내내 그대로 → 실측 무음 0
    let cc = p.conceal.as_ref().unwrap();
    assert!(cc.silent_measured && cc.silent_s == Some(0.0) && cc.rebuffers == Some(0), "{cc:?}");
    // 요약: 릴레이 상태·RTT, 송신 코덱
    assert_eq!((s.relay_states.clone(), s.relay_rtt_ms.as_ref().map(|d| d.median)), (vec!["bound".to_string()], Some(20.0)));
    assert_eq!(p.tx_codec.get("opus(자동: lossFeedback)"), Some(&120));

    let md = markdown(&r);
    assert!(md.contains("| 1-1 |") && md.contains("**충족**") && md.contains("균형 · 5ms"), "{md}");
    assert!(md.contains("50 +2") || md.contains("0:50 +2"), "{md}");
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["files"][0]["sessions"][0]["peers"][0]["bufferMs"]["median"], 5.5);
    assert_eq!(json["goal"][2]["verdict"], "met"); // 3-3(입장 값 있음)
}

/// 여러 zip → 연결 성공률 집계(후보 ③): 경로별 시도·중앙 ms, 실패·중단은 분모에 든다, recover·switch는 시도가 아니다.
#[test]
fn aggregates_connection_success_over_zips() {
    let a = sample_diag(1, vec![
        ev(1, T0, ConnPhase::Connect, ConnOutcome::Lan, 100),
        ev(2, T0, ConnPhase::Reconnect, ConnOutcome::Public, 900),
    ]);
    let b = sample_diag(2, vec![
        ev(1, T0, ConnPhase::Connect, ConnOutcome::Failed, 10_000),
        ev(2, T0, ConnPhase::Recover, ConnOutcome::Relay, 1_500),
        ev(3, T0, ConnPhase::Connect, ConnOutcome::Relay, 1_600),
        ev(4, T0, ConnPhase::Connect, ConnOutcome::Aborted, 300),
        ev(5, T0, ConnPhase::Switch, ConnOutcome::Lan, 60_000),
    ]);
    let diags = vec![read_zip_bytes("a.zip", export(&a)).unwrap(), read_zip_bytes("b.zip", export(&b)).unwrap()];
    let r = report(&diags);
    let c = &r.conn;
    assert_eq!(c.sessions, 2);
    assert_eq!((c.attempts, c.success, c.direct, c.relay, c.failed, c.aborted), (5, 3, 2, 1, 1, 1));
    assert_eq!((c.recover, c.switch, c.sessions_with_relay), (1, 1, 1));
    // 두 zip의 세션이 같은 시간이라(같은 합주의 양쪽일 수 있다) 문턱용 하한은 큰 쪽(b의 3회)만
    assert_eq!(c.attempts_lower, 3);
    assert_eq!(c.success_pct, Some(60.0));
    let rowr = |r: &str| c.rows.iter().find(|x| x.result == r).unwrap().clone();
    assert_eq!(rowr("lan"), ConnPathRow { result: "lan".into(), connect: 1, connect_median_ms: Some(100.0), reconnect: 0, reconnect_median_ms: None });
    assert_eq!(rowr("public").reconnect_median_ms, Some(900.0));
    assert_eq!(rowr("relay").connect_median_ms, Some(1600.0));
    assert_eq!(rowr("failed").connect, 1);
    assert_eq!(c.first_audio_ms, vec![180.5, 180.5]);
    assert_eq!(goal_of(&r.goal, "3-2")[0].verdict, Verdict::NotMet); // 60% < 98%
    let md = markdown(&r);
    assert!(md.contains("| relay | 1 | 1600ms |") && md.contains("릴레이로 붙은 세션 1/2"), "{md}");
}

/// 옛 형식(meta 없음, conn·lossShape·corePlacement·connectMs 없음)과 피어 0명 세션, 깨진 줄도 죽지 않고 "없음"으로.
#[test]
fn old_format_and_empty_sessions_do_not_panic() {
    let old = r#"{"t":1000,"running":true,"network":true,"session":1,"backend":"cpalWasapi","estLatencyMs":21.0,"xruns":0,"peers":[{"peer":"id-aaaa","state":"connected","path":"public","bufferMs":20.0,"jitterMs":3.0,"concealed":0}]}
not json
{"t":2000,"running":true,"network":true,"session":1,"backend":"cpalWasapi","estLatencyMs":21.0,"xruns":0,"peers":[{"peer":"id-aaaa","state":"connected","path":"public","bufferMs":22.0,"jitterMs":3.0,"concealed":4}]}
{"t":3000,"running":false,"network":true,"session":1,"peers":[]}
{"t":9000,"running":true,"network":false,"session":2,"backend":"wasapiExclusive","xruns":0,"peers":[]}
"#;
    let diag = read_zip_bytes("old.zip", zip_of(&[("timeline.jsonl", old)])).unwrap();
    let r = report(&[diag]);
    let f = &r.files[0];
    assert_eq!((f.format, f.app_version.as_deref(), f.bad_lines), (None, None, 1));
    assert_eq!(f.sessions.len(), 2);
    let s1 = &f.sessions[0];
    assert!(s1.config.is_none() && s1.core_placement.is_none() && s1.conn.is_empty());
    let p = &s1.peers[0];
    assert_eq!(p.buffer_ms.as_ref().map(|d| d.median), Some(21.0));
    // 재생 프레임 수(produced)가 없다 → 은닉 % 없음(벽시계로 대신하지 않는다)
    let c = p.conceal.as_ref().unwrap();
    assert!(c.pct.is_none() && c.loss_pct.is_none() && c.concealed == 4);
    assert!(p.loss_shape.is_empty() && p.first_audio_ms.is_empty() && p.frame_ms.is_none());
    assert_eq!((p.no_audio_entries, p.no_audio_short), (0, 1)); // 2초만 있었다
    assert!(s1.goal.iter().all(|g| g.verdict == Verdict::Unknown || g.id == "1-1"), "{:?}", s1.goal);
    assert_eq!(goal_of(&s1.goal, "1-1")[0].verdict, Verdict::Met); // 공유 21 ≤ 30
    let s2 = &f.sessions[1];
    assert_eq!((s2.network, s2.peers.len(), s2.duration_s), (Some(false), 0, 0.0));
    assert_eq!(goal_of(&s2.goal, "2-2")[0].verdict, Verdict::Unknown); // xrun 0이지만 세션 0초 < 30분
    assert_eq!(r.conn.attempts, 0);
    assert!(r.goal.iter().all(|g| g.verdict == Verdict::Unknown));
    let md = markdown(&r);
    assert!(md.contains("피어 없음") && md.contains("연결 기록(conn): 없음") && md.contains("없음(engine.json 설정은"), "{md}");

    // 타임라인이 비어 있는 zip
    let empty = report(&[read_zip_bytes("e.zip", zip_of(&[("timeline.jsonl", "")])).unwrap()]);
    assert!(empty.files[0].sessions.is_empty());
    assert!(markdown(&empty).contains("타임라인 샘플 없음"));
}

#[test]
fn invalid_zip_is_a_clear_error() {
    let e = read_zip_bytes("x.zip", b"not a zip".to_vec()).err().unwrap();
    assert!(e.contains("zip 파일이 아닙니다"), "{e}");
    let e = read_zip_bytes("y.zip", zip_of(&[("README.txt", "hi")])).err().unwrap();
    assert!(e.contains("진단 zip이 아닙니다"), "{e}");
    assert!(read_zip("Z:/definitely/not/here.zip").is_err());
}

/// ±3ms 띠(GOAL 1-4): targetMs가 기록된 형식이면 1분 이상 밖 구간을 센다. 1초 이하 복귀는 이어 센다(`tests/sim/long.rs`와 같다).
#[test]
fn band_counts_minute_long_excursions() {
    let mut lines = String::new();
    for i in 0..200u64 {
        // 10~79초 밖(+5ms), 40초 한 표본만 안(이어 셈), 150~170초 밖(20초)
        let out = (10..80).contains(&i) && i != 40 || (150..170).contains(&i);
        let buf = if out { 15.0 } else { 10.0 };
        lines.push_str(&format!(
            "{{\"t\":{},\"running\":true,\"network\":true,\"session\":5,\"peers\":[{{\"peer\":\"id-b\",\"state\":\"connected\",\"path\":\"lan\",\"bufferMs\":{buf},\"targetMs\":10.0,\"jitterMs\":1.0}}]}}\n",
            i * 1000
        ));
    }
    let r = report(&[read_zip_bytes("b.zip", zip_of(&[("timeline.jsonl", &lines)])).unwrap()]);
    let s = &r.files[0].sessions[0];
    let b = s.peers[0].band.as_ref().unwrap();
    assert_eq!(b.over_limit, 1);
    assert_eq!(b.longest_s, 70.0);
    assert_eq!((b.covered_s, b.gaps), (200.0, 0));
    assert!((b.outside_pct - 89.0 * 100.0 / 200.0).abs() < 1e-9);
    assert_eq!(goal_of(&s.goal, "1-4")[0].verdict, Verdict::NotMet);
}

/// 보고서가 문자열로 맞추는 값이 진단 형식(엔진 enum 직렬화)과 같다 — 이름이 바뀌면 여기서 깨진다.
#[test]
fn matched_strings_follow_engine_enums() {
    use crate::engine::device::BackendKind;
    use band_dsp::types::PeerStateKind;
    let name = |v: serde_json::Value| v.as_str().unwrap().to_string();
    assert_eq!(name(serde_json::to_value(PeerStateKind::Connected).unwrap()), "connected");
    for (p, s) in [(ConnPhase::Connect, "connect"), (ConnPhase::Reconnect, "reconnect"), (ConnPhase::Recover, "recover"), (ConnPhase::Switch, "switch")] {
        assert_eq!(name(serde_json::to_value(p).unwrap()), s);
    }
    for (o, s) in [(ConnOutcome::Lan, "lan"), (ConnOutcome::Public, "public"), (ConnOutcome::Relay, "relay"), (ConnOutcome::Failed, "failed"), (ConnOutcome::Aborted, "aborted")] {
        assert_eq!(name(serde_json::to_value(o).unwrap()), s);
    }
    for (b, c) in [
        (BackendKind::CpalAsio, BackendClass::Asio),
        (BackendKind::WasapiExclusive, BackendClass::Exclusive),
        (BackendKind::WasapiShared3, BackendClass::Shared),
        (BackendKind::CpalWasapi, BackendClass::Shared),
        (BackendKind::Fake, BackendClass::Other),
    ] {
        assert_eq!(backend_class(b.as_str()), c, "{}", b.as_str());
    }
}

/// GOAL 2-1: 손실 1% 이상 회선에서 한도 안이면 충족, 1% 이하 회선에서 한도 밖이면 미달, 그 밖은 판정 불가.
#[test]
fn conceal_rule_needs_the_loss_condition() {
    let z = Some(0);
    assert_eq!(conceal_verdict((1.2, 1.2), (1.0, 1.0), z, (0.0, 0.0)).0, Verdict::Met);
    assert_eq!(conceal_verdict((1.4, 1.4), (2.0, 2.0), Some(30), (0.0, 0.0)).0, Verdict::Met); // 복구 후 손실 ≥ 1%면 회선 손실도 ≥ 1%
    assert_eq!(conceal_verdict((1.6, 1.6), (1.0, 1.0), z, (0.0, 0.0)).0, Verdict::NotMet);
    assert_eq!(conceal_verdict((2.0, 2.0), (0.1, 0.1), z, (0.0, 0.0)).0, Verdict::NotMet);
    assert_eq!(conceal_verdict((0.1, 0.1), (0.0, 0.0), z, (0.0, 0.0)).0, Verdict::Unknown);
    assert_eq!(conceal_verdict((3.0, 3.0), (2.5, 2.5), z, (0.0, 0.0)).0, Verdict::Unknown);
    // 사본·재전송이 손실을 메웠거나(버린 사본 > 0) 기록이 없으면 회선 손실이 1%보다 컸을 수 있다 → 미달이라 할 수 없다
    assert_eq!(conceal_verdict((2.0, 2.0), (0.5, 0.5), Some(12), (0.0, 0.0)).0, Verdict::Unknown);
    assert_eq!(conceal_verdict((2.0, 2.0), (0.5, 0.5), None, (0.0, 0.0)).0, Verdict::Unknown);
    // 구간(#246 H1''): 충족은 은닉 상한·손실 하한으로, 미달은 은닉 하한·손실 상한으로도 성립할 때만
    let s = (13.0, 12.5);
    let v = conceal_verdict((1.40, 3.55), (1.10, 3.27), z, s);
    assert_eq!(v.0, Verdict::Unknown);
    assert!(v.1.as_deref().unwrap().contains("은닉 하한 1.40% ≤ 1.5% < 상한 3.55%") && v.1.as_deref().unwrap().contains("소리 없는 시간 13.0s"), "{v:?}");
    assert_eq!(conceal_verdict((1.40, 1.49), (1.10, 1.20), z, s).0, Verdict::Met);
    assert_eq!(conceal_verdict((1.40, 1.40), (0.98, 1.20), z, s).0, Verdict::Unknown); // 손실이 조건에 걸침
    assert_eq!(conceal_verdict((2.0, 2.4), (0.3, 0.65), z, s).0, Verdict::NotMet); // L1'': 무음은 미달을 덮지 않는다
    let v = conceal_verdict((2.0, 17.0), (0.5, 16.0), z, s); // 무음 동안 회선이 끊겼으면 손실 1% 회선이 아니다
    assert_eq!(v.0, Verdict::Unknown);
    assert!(v.1.as_deref().unwrap().contains("조건 1%에 걸침"), "{v:?}");
}

// ---- #246 리뷰: "충족"으로 잘못 떨어지던 입력들 ----

fn shape_json(frame_ms: f32, lost: u64, underrun: u64) -> serde_json::Value {
    serde_json::to_value(band_dsp::jitter::LossShapeStats { frame_ms, loss_runs: [lost, 0, 0, 0, 0, 0], conceal_underrun: underrun, ..Default::default() })
        .unwrap()
}

fn line(t_s: u64, session: u64, backend: &str, peers: Vec<serde_json::Value>) -> String {
    serde_json::json!({"t": T0 + t_s * 1000, "running": true, "network": true, "session": session, "backend": backend, "xruns": 0, "peers": peers})
        .to_string()
        + "\n"
}

fn report_of(name: &str, text: &str) -> Report {
    report(&[read_zip_bytes(name, zip_of(&[("timeline.jsonl", text)])).unwrap()])
}

/// H1 반례(zip A): 600초 중 처음 120초는 연결 중(재생·은닉 0), 뒤 480초 재생 96000프레임 중 은닉 1632(1.70%)·손실 1440(1.50%).
/// 벽시계 분모면 은닉 1.36%·손실 1.20% → 충족으로 떨어졌다. 재생 프레임 분모로 1.70%·1.50% — 손실이 조건보다 나쁜 회선이라 판정 불가.
#[test]
fn conceal_uses_played_frames_not_wall_clock() {
    let mk = |lost_total: u64, dup: u64| {
        let mut text = String::new();
        for i in 0..=600u64 {
            let (state, k) = if i < 120 { ("connecting", 0) } else { ("connected", i - 120) };
            let p = serde_json::json!({
                "peer": "id-a", "state": state, "path": "lan", "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
                "concealed": 1632 * k / 480, "produced": 200 * k, "rxDupDropped": dup * k / 480,
                "lossShape": shape_json(5.0, lost_total * k / 480, 0),
            });
            text += &line(i, 1, "wasapiExclusive", vec![p]);
        }
        report_of("a.zip", &text)
    };
    let r = mk(1440, 0);
    let p = &r.files[0].sessions[0].peers[0];
    let c = p.conceal.as_ref().unwrap();
    assert_eq!((c.produced, c.concealed, c.connected_s), (Some(96_000), 1632, 481.0));
    assert!((c.pct.unwrap() - 1.70).abs() < 1e-9, "{:?}", c.pct);
    assert!((c.loss_pct.unwrap() - 1.50).abs() < 1e-9, "{:?}", c.loss_pct);
    let g = goal_of(&r.files[0].sessions[0].goal, "2-1")[0];
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");

    // 같은 은닉에 복구 후 손실 0.9%, 사본 없음 → 1% 이하 회선에서 1.5% 초과 = 미달
    let r = mk(864, 0);
    assert_eq!(goal_of(&r.files[0].sessions[0].goal, "2-1")[0].verdict, Verdict::NotMet);
    // 사본이 손실을 메웠다면(버린 사본 > 0) 회선 손실이 더 컸을 수 있다 → 판정 불가
    let r = mk(864, 50);
    assert_eq!(goal_of(&r.files[0].sessions[0].goal, "2-1")[0].verdict, Verdict::Unknown);
}

/// produced가 없는 옛 형식은 손실 모양이 있고 길어도 은닉 %를 내지 않는다(판정 불가).
#[test]
fn conceal_without_produced_is_unknown() {
    let mut text = String::new();
    for i in 0..=300u64 {
        let p = serde_json::json!({"peer": "id-a", "state": "connected", "path": "lan", "concealed": i, "rxDupDropped": 0, "lossShape": shape_json(5.0, i * 3, 0)});
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    let r = report_of("old.zip", &text);
    let s = &r.files[0].sessions[0];
    assert!(s.peers[0].conceal.as_ref().unwrap().pct.is_none());
    let g = goal_of(&s.goal, "2-1")[0];
    assert_eq!(g.verdict, Verdict::Unknown);
    assert!(g.note.as_deref().unwrap().contains("produced"), "{g:?}");
}

/// H2(zip B): 120초 연결됐는데 첫 소리가 없는 피어 → 미달. 3초 이내만 있으면 판정 불가(충족 아님), 2초만 있다 나간 피어는 가르지 않는다.
#[test]
fn first_audio_never_met_and_missing_audio_is_not_met() {
    let peer = |name: &str, fa: Option<f64>| {
        serde_json::json!({"peer": name, "state": "connected", "path": "lan", "connectMs": 100.0, "firstAudioMs": fa})
    };
    let mut b = String::new();
    for i in 0..120u64 {
        b += &line(i, 1, "wasapiExclusive", vec![peer("id-b", None), peer("id-c", Some(500.0))]);
    }
    let r = report_of("b.zip", &b);
    assert_eq!((r.conn.no_audio_entries, r.conn.first_audio_ms.clone()), (1, vec![500.0]));
    assert_eq!(goal_of(&r.goal, "3-3")[0].verdict, Verdict::NotMet);

    let mut ok = String::new();
    for i in 0..120u64 {
        let mut peers = vec![peer("id-c", Some(500.0))];
        if i < 2 {
            peers.push(peer("id-e", None)); // 2초 머물고 나감
        }
        ok += &line(i, 1, "wasapiExclusive", peers);
    }
    let r = report_of("ok.zip", &ok);
    assert_eq!((r.conn.no_audio_entries, r.conn.no_audio_short), (0, 1));
    let g = goal_of(&r.goal, "3-3")[0];
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.note.as_deref().unwrap().contains("기록표"), "{g:?}");

    // 3초 초과는 미달
    let mut late = String::new();
    for i in 0..10u64 {
        late += &line(i, 1, "wasapiExclusive", vec![peer("id-c", Some(3200.0))]);
    }
    assert_eq!(goal_of(&report_of("late.zip", &late).goal, "3-3")[0].verdict, Verdict::NotMet);
}

/// M1(zip C): 61:39 세션에서 피어가 마지막 100초만 있었다 → 1-4는 피어 기록 시간으로 본다(판정 불가). 기록에 틈이 있어도 판정 불가,
/// 60분 이어 기록되고 띠 안이면 충족.
#[test]
fn band_judges_peer_presence_not_session_length() {
    let pd = |name: &str| serde_json::json!({"peer": name, "state": "connected", "path": "lan", "bufferMs": 10.0, "targetMs": 10.0, "jitterMs": 1.0});
    let mut c = String::new();
    for i in 0..3699u64 {
        let peers = if i >= 3599 { vec![pd("id-d")] } else { Vec::new() };
        c += &line(i, 1, "wasapiExclusive", peers);
    }
    let r = report_of("c.zip", &c);
    let s = &r.files[0].sessions[0];
    assert!(s.duration_s >= 3600.0);
    assert_eq!(s.peers[0].band.as_ref().unwrap().covered_s, 100.0);
    assert_eq!(goal_of(&s.goal, "1-4")[0].verdict, Verdict::Unknown);

    // 60분 넘게 띠 안이지만 중간에 5초 기록이 빠짐 → 판정 불가
    let mut g = String::new();
    for i in (0..3700u64).filter(|i| !(1000..1005).contains(i)) {
        g += &line(i, 1, "wasapiExclusive", vec![pd("id-d")]);
    }
    let r = report_of("g.zip", &g);
    let s = &r.files[0].sessions[0];
    assert_eq!(s.peers[0].band.as_ref().unwrap().gaps, 1);
    assert_eq!(goal_of(&s.goal, "1-4")[0].verdict, Verdict::Unknown);

    // 이어 60분 기록, 띠 안 → 충족
    let mut m = String::new();
    for i in 0..3600u64 {
        m += &line(i, 1, "wasapiExclusive", vec![pd("id-d")]);
    }
    let r = report_of("m.zip", &m);
    assert_eq!(goal_of(&r.files[0].sessions[0].goal, "1-4")[0].verdict, Verdict::Met);
}

fn conn_text(t0_s: u64, n: usize, result: &str) -> String {
    let conn: Vec<_> =
        (0..n).map(|k| serde_json::json!({"t": T0 + t0_s * 1000 + k as u64, "peer": "id-x", "phase": "connect", "result": result, "ms": 100})).collect();
    let mut v = serde_json::json!({"t": T0 + t0_s * 1000, "running": true, "network": true, "session": t0_s, "backend": "wasapiExclusive", "peers": []});
    v["conn"] = serde_json::Value::Array(conn);
    v.to_string() + "\n"
}

/// M2: 같은 zip 두 번은 빼고, 시간이 겹치는 다른 zip(한 합주의 양쪽)은 98% 문턱에서 큰 쪽만 센다. 겹치지 않으면 다 센다.
/// 결과를 모르는 시도가 있으면 가르지 않는다.
#[test]
fn connection_attempts_are_not_double_counted() {
    let z = |name: &str, text: &str| read_zip_bytes(name, zip_of(&[("timeline.jsonl", text)])).unwrap();
    let a = conn_text(0, 30, "lan");
    // 같은 zip 두 번
    let r = report(&[z("a.zip", &a), z("a-copy.zip", &a)]);
    assert_eq!(r.files[1].duplicate_of.as_deref(), Some("a.zip"));
    assert_eq!((r.conn.attempts, r.conn.duplicate_files.clone()), (30, vec!["a-copy.zip".to_string()]));
    assert!(markdown(&r).contains("같은 zip"));
    // 같은 시간의 두 zip(양쪽 기록) — 기록 60, 하한 30 → 판정 불가
    let b = conn_text(0, 30, "public").replace("\"session\":0", "\"session\":7");
    let r = report(&[z("a.zip", &a), z("b.zip", &b)]);
    assert_eq!((r.conn.attempts, r.conn.attempts_lower), (60, 30));
    assert_eq!(goal_of(&r.goal, "3-2")[0].verdict, Verdict::Unknown);
    // 다른 시간의 두 zip — 하한 60 → 충족
    let r = report(&[z("a.zip", &a), z("c.zip", &conn_text(5000, 30, "relay"))]);
    assert_eq!((r.conn.attempts, r.conn.attempts_lower), (60, 60));
    assert_eq!(goal_of(&r.goal, "3-2")[0].verdict, Verdict::Met);
    // 모르는 결과가 섞이면 판정 불가
    let r = report(&[z("a.zip", &a), z("c.zip", &conn_text(5000, 30, "relay")), z("d.zip", &conn_text(9000, 1, "teleport"))]);
    assert_eq!(r.conn.unknown_results, 1);
    assert_eq!(goal_of(&r.goal, "3-2")[0].verdict, Verdict::Unknown);
}

/// 1-1·1-3·2-2에서 충족으로 잘못 떨어질 입력: 최대가 한도 밖, 경로가 섞임, 세션 중 백엔드가 바뀜.
#[test]
fn session_rows_do_not_meet_on_partial_evidence() {
    // 1-1: 처음 10분 20ms(독점 15ms 한도)·중앙 10ms. 2-2: 31분 xrun 0이지만 처음 1분은 공유. 1-3: LAN이 대부분이지만 public이 섞임.
    let mut text = String::new();
    for i in 0..1900u64 {
        let backend = if i < 60 { "wasapiShared3" } else { "wasapiExclusive" };
        let path = if i % 10 == 0 { "public" } else { "lan" };
        let jitter = if path == "public" { 20.0 } else { 0.5 };
        let p = serde_json::json!({"peer": "id-a", "state": "connected", "path": path, "bufferMs": 6.0, "jitterMs": jitter, "remoteFrameMs": 5.0});
        let mut v: serde_json::Value = serde_json::from_str(&line(i, 1, backend, vec![p])).unwrap();
        v["estLatencyMs"] = serde_json::json!(if i < 600 { 20.0 } else { 10.0 });
        text += &(v.to_string() + "\n");
    }
    let r = report_of("p.zip", &text);
    let s = &r.files[0].sessions[0];
    assert_eq!(s.backends, vec!["wasapiShared3".to_string(), "wasapiExclusive".to_string()]);
    for id in ["1-1", "1-3", "2-2"] {
        let g = goal_of(&s.goal, id)[0];
        assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    }

    // 백엔드 한 가지, 중앙은 한도 안인데 최대가 넘음 → 1-1 판정 불가
    let mut t2 = String::new();
    for i in 0..100u64 {
        let mut v: serde_json::Value = serde_json::from_str(&line(i, 1, "wasapiExclusive", Vec::new())).unwrap();
        v["estLatencyMs"] = serde_json::json!(if i < 10 { 20.0 } else { 10.0 });
        t2 += &(v.to_string() + "\n");
    }
    assert_eq!(goal_of(&report_of("q.zip", &t2).files[0].sessions[0].goal, "1-1")[0].verdict, Verdict::Unknown);
}

/// 압축 폭탄 상한: 압축을 푼 timeline.jsonl이 상한을 넘으면 오류.
#[test]
fn oversized_entry_is_an_error() {
    let body = "x".repeat(1025);
    let e = read_zip_bytes_max("bomb.zip", zip_of(&[("timeline.jsonl", &body)]), 1024).err().unwrap();
    assert!(e.contains("넘습니다"), "{e}");
    assert!(read_zip_bytes_max("ok.zip", zip_of(&[("timeline.jsonl", &body[..1024])]), 1024).is_ok());
    assert_eq!(MAX_ENTRY_BYTES, 64 << 20);
    assert!(e.contains("넘습니다"), "{e}");
}

// ---- #246 재리뷰: H1'·L1'·L2' ----

/// 타임라인 줄(ms 시각). `running`·세션 1·독점.
fn line_ms(t_ms: u64, peers: Vec<serde_json::Value>) -> String {
    serde_json::json!({"t": T0 + t_ms, "running": true, "network": true, "session": 1, "backend": "wasapiExclusive", "xruns": 0, "peers": peers})
        .to_string()
        + "\n"
}

/// H1' 반례(zip E1): 600초 내내 `connected`·LAN·F 5ms(기대 재생 120000프레임)인데, 10초짜리 무수신 리셋 10번으로 produced는
/// 100000(100초 무음). 은닉 1400·복구 후 손실 1100 → 1.40%·1.10%라 예전엔 충족. 소리 없는 시간 100초를 은닉으로 본 상한이
/// 17.76%라 판정 불가(#246 H1''), 그 시간을 따로 보여 준다.
#[test]
fn silent_resets_make_conceal_unknown() {
    let mut text = String::new();
    let mut played = 0u64;
    for i in 0..=600u64 {
        let silent = i > 0 && (i - 1) % 50 >= 20 && (i - 1) % 50 < 30 && i <= 500;
        if i > 0 && !silent {
            played += 1;
        }
        let p = serde_json::json!({
            "peer": "id-e", "state": "connected", "path": "lan", "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
            "concealed": 1400 * played / 500, "produced": 200 * played, "rxDupDropped": 0,
            "lossShape": shape_json(5.0, 1100 * played / 500, 0),
        });
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    assert_eq!(played, 500);
    let r = report_of("e1.zip", &text);
    let s = &r.files[0].sessions[0];
    let c = s.peers[0].conceal.as_ref().unwrap();
    assert_eq!((c.produced, c.concealed), (Some(100_000), 1400));
    assert!((c.pct.unwrap() - 1.40).abs() < 1e-9 && (c.loss_pct.unwrap() - 1.10).abs() < 1e-9, "{c:?}");
    assert_eq!(c.playing_span_s, 600.0);
    assert!((c.silent_s.unwrap() - 100.0).abs() < 1e-9, "{c:?}");
    // 한 구간이라 끝 오차 0.5s만 빼고 99.5s(19900프레임)를 상한에 넣는다
    assert!((c.silent_hi_s.unwrap() - 99.5).abs() < 1e-9, "{c:?}");
    assert!((c.pct_hi.unwrap() - 21_300.0 * 100.0 / 119_900.0).abs() < 1e-9, "{c:?}");
    let g = goal_of(&s.goal, "2-1")[0];
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.note.as_deref().unwrap().contains("은닉 하한 1.40% ≤ 1.5% < 상한 17.76%"), "{g:?}");
    assert!(g.measured.contains("은닉 1.40–17.76%"), "{g:?}");
    assert!(g.measured.contains("소리 없는 시간 100.0s(16.7%)"), "{g:?}");
    // 표에도 따로 보인다
    let md = markdown(&r);
    assert!(md.contains("소리 없는 시간") && md.contains("100.0s(16.7%)"), "{md}");

    // 상대 프레임 길이를 모르면(lossShape·remoteFrameMs 없음) 무음 시간을 확인할 수 없다 → 판정 불가
    let mut nf = String::new();
    for i in 0..=120u64 {
        let p = serde_json::json!({"peer": "id-e", "state": "connected", "path": "lan", "concealed": i, "produced": 200 * i, "rxDupDropped": 0});
        nf += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    let r = report_of("nf.zip", &nf);
    assert!(r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap().silent_s.is_none());
    assert_eq!(goal_of(&r.files[0].sessions[0].goal, "2-1")[0].verdict, Verdict::Unknown);
}

/// 끊김 없는 정상 세션은 허용 오차 때문에 판정 불가로 떨어지지 않는다: 표본 시각이 0–80ms 흔들리고, 첫 연결 표본 뒤 300ms 동안
/// 지터 버퍼를 채우고, 장치 클록이 시스템 시계보다 0.05% 느리고, 장치 주기 10ms 단위로 재생해도(F 2.5ms) 충족.
#[test]
fn steady_session_is_not_flagged_silent() {
    let mut text = String::new();
    for i in 0..=600u64 {
        let t = i * 1000 + i * 37 % 81; // 표본 시각 0–80ms 흔들림
        let play_ms = (t.saturating_sub(300) as f64 * 0.9995) as u64 / 10 * 10;
        let produced = play_ms * 2 / 5; // ÷ 2.5ms
        let p = serde_json::json!({
            "peer": "id-s", "state": "connected", "path": "lan", "bufferMs": 6.0, "jitterMs": 1.0, "targetMs": 6.0,
            "concealed": produced * 12 / 1000, "produced": produced, "rxDupDropped": 0,
            "lossShape": shape_json(2.5, produced * 11 / 1000, 0),
        });
        text += &line_ms(t, vec![p]);
    }
    let r = report_of("steady.zip", &text);
    let s = &r.files[0].sessions[0];
    let c = s.peers[0].conceal.as_ref().unwrap();
    let silent = c.silent_s.unwrap();
    assert!(silent > 0.2 && silent < 1.0, "{c:?}");
    // 끝 오차(0.5s) 안팎이라 상한이 하한과 거의 같다
    assert!(c.pct_hi.unwrap() - c.pct.unwrap() < 0.05, "{c:?}");
    let g = goal_of(&s.goal, "2-1")[0];
    assert_eq!(g.verdict, Verdict::Met, "{g:?}");
}

/// L1'(zip E2): 61분 `connected`인데 재생하지 않음(bufferMs 0, targetMs 0) → 1-4 충족이 아니다.
#[test]
fn band_does_not_count_idle_buffer_as_inside() {
    let mut text = String::new();
    for i in 0..3660u64 {
        let p = serde_json::json!({"peer": "id-i", "state": "connected", "path": "lan", "bufferMs": 0.0, "targetMs": 0.0, "jitterMs": 0.0});
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    let r = report_of("e2.zip", &text);
    let s = &r.files[0].sessions[0];
    assert_eq!(s.peers[0].band.as_ref().unwrap().outside_pct, 100.0);
    assert_eq!(goal_of(&s.goal, "1-4")[0].verdict, Verdict::NotMet);
}

/// L2': 내보낼 때 아직 연결 중인 시도는 분모에 "미완"으로 든다. 미완이 모두 성공해도 98% 아래일 때만 미달, 결과에 따라 갈리면 판정
/// 불가. 세션이 끝난 기록(마지막 줄이 실행 아님)에는 미완이 없다.
#[test]
fn pending_attempts_stay_in_the_denominator() {
    let mk = |ok: usize, failed: usize, pending: usize, stopped: bool| {
        let mut conn: Vec<_> = (0..ok).map(|k| serde_json::json!({"t": T0 + k as u64, "peer": "id-x", "phase": "connect", "result": "lan", "ms": 100})).collect();
        conn.extend((0..failed).map(|k| serde_json::json!({"t": T0 + 500 + k as u64, "peer": "id-y", "phase": "connect", "result": "failed", "ms": 9000})));
        let mut text = serde_json::json!({"t": T0, "running": true, "network": true, "session": 1, "backend": "wasapiExclusive", "conn": conn}).to_string() + "\n";
        let peers: Vec<_> = (0..pending).map(|k| serde_json::json!({"peer": format!("id-p{k}"), "state": if k % 2 == 0 { "connecting" } else { "reconnecting" }})).collect();
        text += &line(1, 1, "wasapiExclusive", peers);
        if stopped {
            text += &(serde_json::json!({"t": T0 + 2000, "running": false, "session": 1}).to_string() + "\n");
        }
        report_of("p.zip", &text)
    };
    // 성공 97 + 미완 3: 미완이 모두 성공이면 100%, 모두 실패면 97% → 판정 불가(예전엔 97/97 = 100% 충족)
    let r = mk(97, 0, 3, false);
    assert_eq!((r.conn.pending, r.conn.attempts, r.conn.attempts_lower), (3, 100, 100));
    assert!((r.conn.success_pct.unwrap() - 97.0).abs() < 1e-9);
    let g = goal_of(&r.goal, "3-2")[0];
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.measured.contains("미완 3") && g.note.as_deref().unwrap().contains("미완"), "{g:?}");
    assert!(markdown(&r).contains("미완 3회"));
    // 실패 5 + 미완 1: 미완이 성공이어도 95% → 미달
    assert_eq!(goal_of(&mk(94, 5, 1, false).goal, "3-2")[0].verdict, Verdict::NotMet);
    // 성공 99 + 미완 1(최악 99%) → 충족
    assert_eq!(goal_of(&mk(99, 0, 1, false).goal, "3-2")[0].verdict, Verdict::Met);
    // 세션이 끝난 기록: 연결 중으로 남은 피어는 엔진 정지가 aborted로 남긴다 — 미완으로 세지 않는다
    assert_eq!(mk(97, 0, 3, true).conn.pending, 0);
}

// ---- #246 3차 리뷰: H1''·L1''·L2'' ----

/// `total`초 내내 `connected`·LAN·F 5ms. 100초 뒤 `silent`초 동안 무수신(produced·은닉 그대로). 은닉·복구 후 손실은 재생
/// 프레임의 `conceal`/`loss` × 0.001%.
fn silent_session(total: u64, silent: u64, conceal: u64, loss: u64) -> Report {
    let mut text = String::new();
    let mut played = 0u64;
    for i in 0..=total {
        if i > 0 && !(i > 100 && i <= 100 + silent) {
            played += 1;
        }
        let produced = 200 * played;
        let p = serde_json::json!({
            "peer": "id-h", "state": "connected", "path": "lan", "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
            "concealed": conceal * produced / 100_000, "produced": produced, "rxDupDropped": 0,
            "lossShape": shape_json(5.0, loss * produced / 100_000, 0),
        });
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    report_of("h.zip", &text)
}

fn verdict_21(r: &Report) -> &GoalRow {
    goal_of(&r.files[0].sessions[0].goal, "2-1")[0]
}

/// H1'' 재현(3차 리뷰 표): 허용치(2% + 1s) 아래의 무음이 통째로 빠져 충족이 나던 입력. 무음을 은닉으로 본 상한이 1.5%를 넘으면
/// 판정 불가다. 무음이 없으면 상한 = 하한이라 충족이 그대로다.
#[test]
fn silence_below_old_tolerance_is_not_met() {
    for (total, silent, conceal) in [(600, 13, 1400), (600, 13, 1480), (600, 13, 0), (3600, 73, 1400)] {
        let r = silent_session(total, silent, conceal, 1100);
        let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
        assert!((c.silent_s.unwrap() - silent as f64).abs() < 1e-9, "{c:?}");
        assert!(c.pct_hi.unwrap() > goal::CONCEAL_MAX_PCT, "{c:?}");
        let g = verdict_21(&r);
        assert_eq!(g.verdict, Verdict::Unknown, "{total}s 중 {silent}s, 은닉 {conceal}: {g:?}");
        if conceal > 0 {
            assert!(g.note.as_deref().unwrap().contains("< 상한") && g.note.as_deref().unwrap().contains(&format!("소리 없는 시간 {silent}.0s")), "{g:?}");
        }
    }
    // 600s 중 13s 무음, 은닉 1.40%: 상한 = (1643 + 2500) / (117400 + 2500)
    let r = silent_session(600, 13, 1400, 1100);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!((c.pct_hi.unwrap() - 4143.0 * 100.0 / 119_900.0).abs() < 1e-9, "{c:?}");
    assert!(markdown(&r).contains("1.40–3.46%"), "{}", markdown(&r));

    // 무음 0 → 상한 = 하한, 충족 유지
    let r = silent_session(600, 0, 1400, 1100);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert_eq!((c.silent_s, c.silent_hi_s, c.pct_hi), (Some(0.0), Some(0.0), c.pct));
    assert_eq!(verdict_21(&r).verdict, Verdict::Met, "{:?}", verdict_21(&r));
}

/// L1'': 은닉 하한만으로 미달이고 무음을 손실로 넣어도 손실 1% 이하 회선이면 미달(무음이 확정 미달을 판정 불가로 덮지 않는다).
/// 무음이 길어 손실 상한이 1%를 넘으면(무음 동안 회선이 끊겼을 수 있음) 손실 1% 회선인지 몰라 판정 불가.
#[test]
fn silence_does_not_hide_a_clear_miss() {
    let r = silent_session(600, 2, 2000, 300);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!(c.loss_pct_hi.unwrap() < 1.0, "{c:?}");
    assert_eq!(verdict_21(&r).verdict, Verdict::NotMet, "{:?}", verdict_21(&r));

    let r = silent_session(600, 100, 2000, 500);
    let g = verdict_21(&r);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.note.as_deref().unwrap().contains("조건 1%에 걸침"), "{g:?}");
}

/// L2'': 장치 클록이 빠른 구간의 음수(재생 > 기대)가 다른 구간의 진짜 무음을 상쇄하지 않는다 — 구간마다 0 아래를 자른다.
#[test]
fn fast_clock_run_does_not_cancel_silence_elsewhere() {
    let mut text = String::new();
    let mut produced = 0u64;
    for i in 0..=500u64 {
        let state = if i == 201 { "reconnecting" } else { "connected" };
        if i > 0 && i <= 200 {
            produced += 201; // 구간 A: 장치가 0.5% 빠름(기대 200s, 재생 201s)
        } else if i > 202 && !(300..310).contains(&i) {
            produced += 200; // 구간 B: 10s 무음
        }
        let p = serde_json::json!({
            "peer": "id-l", "state": state, "path": "lan", "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
            "concealed": 0, "produced": produced, "rxDupDropped": 0, "lossShape": shape_json(5.0, 0, 0),
        });
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    let r = report_of("l2.zip", &text);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!((c.silent_s.unwrap() - 10.0).abs() < 1e-9, "{c:?}");
    // 끝 오차는 첫 구간(A)에서만 뺀다 — 재연결 뒤 구간 B의 무음은 전부 상한에 든다(#246 H1''')
    assert!((c.silent_hi_s.unwrap() - 10.0).abs() < 1e-9, "{c:?}");
    assert_eq!(c.runs, 2);
}

// ---- #246 4차 리뷰: H1''' ----

/// `total`초, `period`초마다 `reconnecting` 표본 하나(`i % period == 0`), 구간마다 첫 쌍에서 `silent_ms` 무음(1초 쌍에 재생
/// 1000 − silent_ms ms). F 5ms, 은닉 1.40%·복구 후 손실 1.10%(재생 프레임 기준), LAN, 사본 0.
fn reconnecting_session(total: u64, period: u64, silent_ms: u64) -> Report {
    let mut text = String::new();
    let mut produced = 0u64;
    for i in 0..=total {
        let connected = i % period != 0;
        if connected && i % period == 2 {
            produced += (1000 - silent_ms) / 5;
        } else if connected && i % period > 2 {
            produced += 200;
        }
        let p = serde_json::json!({
            "peer": "id-r", "state": if connected { "connected" } else { "reconnecting" }, "path": "lan",
            "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
            "concealed": 1400 * produced / 100_000, "produced": produced, "rxDupDropped": 0,
            "lossShape": shape_json(5.0, 1100 * produced / 100_000, 0),
        });
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    report_of("r.zip", &text)
}

/// H1''' 재현(4차 리뷰 표): 끝 오차 0.5s를 구간마다 빼면 재연결이 잦을 때 무음이 "구간 수 × 0.5s"까지 상한에서 빠져 충족이 났다.
/// 빼는 총량은 구간 수와 상관없이 `SILENT_EDGE_S` 이하여야 한다.
#[test]
fn frequent_reconnects_do_not_erase_silence() {
    // 600s, 3s마다 재연결(구간 200개), 구간마다 0.5s 무음 → 기대 재생 200s 중 100s 무음
    let r = reconnecting_session(600, 3, 500);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert_eq!(c.runs, 200, "{c:?}");
    assert!((c.playing_span_s - 200.0).abs() < 1e-9 && (c.silent_s.unwrap() - 100.0).abs() < 1e-9, "{c:?}");
    assert!((c.silent_s.unwrap() - c.silent_hi_s.unwrap() - goal::SILENT_EDGE_S).abs() < 1e-9, "{c:?}");
    let g = verdict_21(&r);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    let note = g.note.as_deref().unwrap();
    assert!(note.contains("< 상한") && note.contains("연결 구간 200개(재연결·재입장 199회)"), "{g:?}");

    // 600s, 12s마다 재연결(구간 50개), 구간마다 0.5s 무음 → 25s(5%) 무음
    let r = reconnecting_session(600, 12, 500);
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert_eq!(c.runs, 50, "{c:?}");
    assert!((c.silent_s.unwrap() - 25.0).abs() < 1e-9 && (c.silent_hi_s.unwrap() - 24.5).abs() < 1e-9, "{c:?}");
    assert_ne!(verdict_21(&r).verdict, Verdict::Met, "{:?}", verdict_21(&r));

    // 빼는 총량 ≤ 고정 상수: 구간 수를 늘려도 (무음 − 상한에 넣은 무음)은 SILENT_EDGE_S를 넘지 않는다
    for period in [3, 5, 12, 60] {
        let r = reconnecting_session(600, period, 500);
        let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
        assert!(c.silent_s.unwrap() - c.silent_hi_s.unwrap() <= goal::SILENT_EDGE_S + 1e-9, "{period}: {c:?}");
    }
}

/// 정상 세션: 3600s에 재연결 1회(재연결 뒤 리버퍼 0.5s 무음, 또는 무음 없음)면 은닉 1.40%가 충족을 유지한다.
#[test]
fn one_reconnect_session_stays_met() {
    for silent_ms in [0, 500] {
        let r = reconnecting_session(3600, 1800, silent_ms);
        let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
        assert_eq!(c.runs, 2, "{c:?}");
        assert!(c.pct_hi.unwrap() <= goal::CONCEAL_MAX_PCT, "{c:?}");
        let g = verdict_21(&r);
        assert_eq!(g.verdict, Verdict::Met, "{silent_ms}: {g:?}");
        assert!(g.note.as_deref().unwrap().contains("연결 구간 2개(재연결·재입장 1회)"), "{g:?}");
    }
    // 재연결 없는 세션은 구간 수를 비고에 적지 않는다
    let g = verdict_21(&silent_session(600, 0, 1400, 1100)).clone();
    assert_eq!(g.note.as_deref(), Some("수치만 — 청감은 [사람]"), "{g:?}");
}

/// L2: 은닉이 문턱에 걸치고 손실도 조건 아래면 비고에 손실 쪽도 적는다.
#[test]
fn straddling_note_mentions_low_loss() {
    let r = silent_session(600, 13, 1400, 500);
    let g = verdict_21(&r);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    let note = g.note.as_deref().unwrap();
    assert!(note.contains("< 상한") && note.contains("도 조건 1%"), "{g:?}");
}

/// #245: threadPolicy는 값이 바뀐 줄에만 있다 — 세션 첫 줄에만 있어도 요약에 나오고, 없는 세션은 "없음".
#[test]
fn thread_policy_from_changed_row_only() {
    let pol = serde_json::json!({"worker": {"highQos": "ok", "pCores": "off"}, "netRecv": null});
    let mut text = String::new();
    for i in 0..=5u64 {
        let mut v: serde_json::Value = serde_json::from_str(&line(i, 1, "wasapiExclusive", vec![])).unwrap();
        if i == 0 {
            v["threadPolicy"] = pol.clone();
        }
        text += &(v.to_string() + "\n");
    }
    for i in 10..=12u64 {
        text += &line(i, 2, "wasapiExclusive", vec![]);
    }
    let r = report_of("tp.zip", &text);
    let ss = &r.files[0].sessions;
    assert_eq!(ss[0].thread_policy.as_ref(), Some(&pol));
    assert!(ss[1].thread_policy.is_none());
    let md = markdown(&r);
    assert!(md.contains("| threadPolicy | {"), "{md}");
    assert!(md.contains("| threadPolicy | 없음 |"), "{md}");
}

// ---- 진단 값 노출(format 3): 리버퍼 실측·입장 후 첫 소리·NAT 콘형 쌍 ----

fn report_v3(name: &str, text: &str) -> Report {
    report(&[read_zip_bytes(name, zip_of(&[("meta.json", r#"{"format":3}"#), ("timeline.jsonl", text)])).unwrap()])
}

/// 600초 `connected`·F 5ms, 100초 뒤 `gap`초 동안 produced가 멈춘다(무수신). 엔진 리버퍼 실측은 `rebuf_s`초(format 3 — 0이면 키 없음).
/// 은닉 1.40%·복구 후 손실 1.10%.
fn rebuffer_session(gap: u64, rebuf_s: f64) -> String {
    let mut text = String::new();
    let mut played = 0u64;
    for i in 0..=600u64 {
        if i > 0 && !(i > 100 && i <= 100 + gap) {
            played += 1;
        }
        let produced = 200 * played;
        let mut p = serde_json::json!({
            "peer": "id-r", "state": "connected", "path": "lan", "bufferMs": 10.0, "jitterMs": 1.0, "targetMs": 10.0,
            "concealed": 1400 * produced / 100_000, "produced": produced, "rxDupDropped": 0,
            "lossShape": shape_json(5.0, 1100 * produced / 100_000, 0),
        });
        if i > 100 && rebuf_s > 0.0 {
            p["rebuffers"] = serde_json::json!(1);
            p["rebufferMs"] = serde_json::json!((((i - 100) as f64).min(rebuf_s)) * 1000.0);
        }
        text += &line(i, 1, "wasapiExclusive", vec![p]);
    }
    text
}

/// 리버퍼 카운터가 있는 zip(format 3)은 소리 없는 시간을 실측으로 센다: 카운터가 0이면 기대치 추정의 무음(예: 장치 클록·기록 누락)은
/// 상한에 넣지 않아 충족, 카운터가 13초를 세면 그 13초를 끝 오차 없이 전부 상한에 넣어 판정 불가(2-1 보수성 유지). 같은 입력이라도
/// format 3 이전이면(카운터 없음) 기존 추정을 쓴다.
#[test]
fn rebuffer_counter_replaces_silence_estimate() {
    // 추정으로는 13초 무음이지만 엔진은 리버퍼가 없다고 셌다(키 없음 = 0)
    let r = report_v3("c0.zip", &rebuffer_session(13, 0.0));
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!(c.silent_measured && c.silent_s == Some(0.0) && c.rebuffers == Some(0), "{c:?}");
    assert!((c.silent_est_s.unwrap() - 13.0).abs() < 1e-9, "추정값은 참고로 남긴다: {c:?}");
    assert_eq!(c.pct_hi, c.pct);
    assert_eq!(verdict_21(&r).verdict, Verdict::Met, "{:?}", verdict_21(&r));
    assert!(markdown(&r).contains("0.0s(0.0%) 실측(재버퍼 0회, 기대치 추정 13.0s)"), "{}", markdown(&r));

    // 엔진이 13초를 셌다 → 상한 = (1643 + 2600) / (117400 + 2600) — 끝 오차(0.5s)를 빼지 않는다
    let r = report_v3("c13.zip", &rebuffer_session(13, 13.0));
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!(c.silent_measured && (c.silent_s.unwrap() - 13.0).abs() < 1e-9 && c.silent_hi_s == c.silent_s, "{c:?}");
    assert_eq!(c.rebuffers, Some(1));
    assert!((c.pct_hi.unwrap() - 4243.0 * 100.0 / 120_000.0).abs() < 1e-9, "{c:?}");
    assert_eq!(verdict_21(&r).verdict, Verdict::Unknown);

    // 1.5% 경계: 무음 실측 1.0s → 상한 (1643 + 200) / (117400 + 200) = 1.567% > 1.5% → 충족이 아니다
    let r = report_v3("c1.zip", &rebuffer_session(1, 1.0));
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!(c.pct_hi.unwrap() > goal::CONCEAL_MAX_PCT, "{c:?}");
    assert_eq!(verdict_21(&r).verdict, Verdict::Unknown);

    // 같은 타임라인을 format 표시 없이(옛 zip) 주면 카운터가 없다고 보고 추정(13s − 끝 오차 0.5s)을 쓴다
    let r = report_of("old.zip", &rebuffer_session(13, 0.0));
    let c = r.files[0].sessions[0].peers[0].conceal.as_ref().unwrap();
    assert!(!c.silent_measured && c.rebuffers.is_none() && (c.silent_s.unwrap() - 13.0).abs() < 1e-9, "{c:?}");
    assert_eq!(verdict_21(&r).verdict, Verdict::Unknown);
}

/// 입장 후 첫 소리(3-3): `entryLeadMs + firstAudioMs`. 모든 항목에 있고 3초 이내면 충족, 하나라도 넘으면 미달, 입장 값이 없는 항목이
/// 섞이면 판정 불가. 소리 없이 머문 시간에도 입장 값을 더한다.
#[test]
fn first_audio_from_entry() {
    let peer = |name: &str, lead: Option<f64>, fa: Option<f64>| {
        let mut p = serde_json::json!({"peer": name, "state": "connected", "path": "lan", "connectMs": 100.0, "firstAudioMs": fa});
        if let Some(l) = lead {
            p["entryLeadMs"] = serde_json::json!(l);
        }
        p
    };
    let run = |peers: &dyn Fn(u64) -> Vec<serde_json::Value>, n: u64| {
        let mut t = String::new();
        for i in 0..n {
            t += &line(i, 1, "wasapiExclusive", peers(i));
        }
        report_v3("e.zip", &t)
    };
    let r = run(&|_| vec![peer("id-a", Some(900.0), Some(600.0)), peer("id-b", Some(0.0), Some(400.0))], 30);
    assert_eq!(r.conn.entry_first_audio_ms, vec![1500.0, 400.0]);
    let g = goal_of(&r.goal, "3-3")[0];
    assert_eq!(g.verdict, Verdict::Met, "{g:?}");
    assert!(g.measured.contains("입장 후 2개 · 중앙 950ms · 최대 1500ms"), "{g:?}");
    assert!(markdown(&r).contains("| 100ms / 600ms / 1500ms |"), "{}", markdown(&r));

    // 입장부터 3초 넘음(피어 추가 후로는 0.6초)
    let r = run(&|_| vec![peer("id-a", Some(2500.0), Some(600.0))], 30);
    assert_eq!(goal_of(&r.goal, "3-3")[0].verdict, Verdict::NotMet);
    // 입장 값 없는 항목(옛 앱)이 섞이면 판정 불가
    let r = run(&|_| vec![peer("id-a", Some(900.0), Some(600.0)), peer("id-b", None, Some(400.0))], 30);
    let g = goal_of(&r.goal, "3-3")[0];
    assert_eq!((g.verdict, r.conn.no_lead_entries), (Verdict::Unknown, 1), "{g:?}");
    // 2초 머물고 소리 없음: 입장 값 1.5초를 더하면 3초 넘게 소리가 없었다 → 미달
    let r = run(&|i| if i < 2 { vec![peer("id-c", Some(1500.0), None)] } else { vec![] }, 10);
    assert_eq!((r.conn.no_audio_entries, goal_of(&r.goal, "3-3")[0].verdict), (1, Verdict::NotMet));
    // 입장 값이 없으면 같은 2초는 가를 수 없다(짧게 머묾)
    let r = run(&|i| if i < 2 { vec![peer("id-c", None, None)] } else { vec![] }, 10);
    assert_eq!((r.conn.no_audio_entries, r.conn.no_audio_short), (0, 1));
}

/// 한 사람 zip: `nat` 매핑, 피어 1명, 연결 기록 `results`. 세션 번호로 zip을 구분한다(시각은 같다 = 한 합주).
fn nat_zip(name: &str, session: u64, nat: Option<&str>, results: &[&str]) -> Diag {
    let mut text = String::new();
    for i in 0..20u64 {
        let mut v = serde_json::json!({"t": T0 + i * 1000, "running": true, "network": true, "session": session, "backend": "wasapiExclusive",
            "peers": [{"peer": format!("id-{session}"), "state": "connected", "path": "lan"}]});
        if i == 0 {
            if let Some(m) = nat {
                v["nat"] = serde_json::json!({"mapping": m, "responses": 2});
            }
            v["conn"] = serde_json::Value::Array(
                results.iter().map(|r| serde_json::json!({"t": T0, "peer": format!("id-{session}"), "phase": "connect", "result": r, "ms": 120})).collect(),
            );
        }
        text += &(v.to_string() + "\n");
    }
    read_zip_bytes(name, zip_of(&[("meta.json", r#"{"format":3}"#), ("timeline.jsonl", &text)])).unwrap()
}

/// 3-1: 같은 합주의 양쪽 zip이 모두 콘형(매핑 독립)·NAT 없음이면 그 시도는 콘형 쌍 시도다 — 모두 직접이면 충족, 릴레이·실패가 있으면
/// 미달. 한쪽만 있거나, 한쪽이 대칭형·모름이거나, NAT 추정이 없는 옛 zip이면 판정 불가.
#[test]
fn direct_rate_judged_on_cone_pairs() {
    let g31 = |zips: Vec<Diag>| {
        let r = report(&zips);
        (goal_of(&r.goal, "3-1")[0].clone(), r)
    };
    let (g, r) = g31(vec![nat_zip("a.zip", 1, Some("independent"), &["lan"]), nat_zip("b.zip", 2, Some("open"), &["public"])]);
    assert_eq!(g.verdict, Verdict::Met, "{g:?}");
    assert_eq!((r.conn.cone_groups, r.conn.cone_attempts, r.conn.cone_direct), (1, 2, 2));
    assert!(markdown(&r).contains("independent — 매핑 독립(콘형), STUN 응답 2"), "{}", markdown(&r));

    let (g, _) = g31(vec![nat_zip("a.zip", 1, Some("independent"), &["relay"]), nat_zip("b.zip", 2, Some("independent"), &["relay"])]);
    assert_eq!(g.verdict, Verdict::NotMet, "{g:?}");
    // 한쪽이 대칭형 → 콘형 쌍이 아니다
    let (g, r) = g31(vec![nat_zip("a.zip", 1, Some("independent"), &["relay"]), nat_zip("b.zip", 2, Some("dependent"), &["relay"])]);
    assert_eq!((g.verdict, r.conn.cone_groups), (Verdict::Unknown, 0), "{g:?}");
    // 한쪽 zip만 → 상대 NAT을 모른다
    let (g, _) = g31(vec![nat_zip("a.zip", 1, Some("independent"), &["lan"])]);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.note.as_deref().unwrap().contains("참가자 모두의 zip"), "{g:?}");
    // 중단(결과 모름)이 섞이면 판정 불가
    let (g, _) = g31(vec![nat_zip("a.zip", 1, Some("independent"), &["lan"]), nat_zip("b.zip", 2, Some("independent"), &["aborted"])]);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    // 옛 zip(nat 없음)
    let (g, _) = g31(vec![nat_zip("a.zip", 1, None, &["lan"]), nat_zip("b.zip", 2, None, &["lan"])]);
    assert_eq!(g.verdict, Verdict::Unknown, "{g:?}");
    assert!(g.note.as_deref().unwrap().contains("format 3 이전"), "{g:?}");
}

/// 송신 큐 p99는 바뀐 줄에만 있다 — 실행 샘플마다 앞 값을 이어 써 분포를 낸다. 요약 표에 릴레이·NAT·송신 큐가 나온다.
#[test]
fn send_queue_carries_forward_and_summary_shows_values() {
    let mut text = String::new();
    for i in 0..10u64 {
        let mut v: serde_json::Value = serde_json::from_str(&line(i, 1, "wasapiExclusive", vec![])).unwrap();
        if i == 0 {
            v["sendQueueP99Ms"] = serde_json::json!(0.1);
        }
        if i == 8 {
            v["sendQueueP99Ms"] = serde_json::json!(0.9);
        }
        let rtt = if i < 3 { None } else { Some(30.0) };
        v["relay"] = serde_json::json!({"state": if i < 3 { "binding" } else { "bound" }, "addr": "203.0.x.x:5000", "rttMs": rtt, "txKbps": 0.0, "overBudget": false, "autoOpus": false});
        text += &(v.to_string() + "\n");
    }
    let r = report_v3("sq.zip", &text);
    let s = &r.files[0].sessions[0];
    assert_eq!(s.send_queue_p99_ms, Some(Dist { median: 0.1, p95: 0.9, max: 0.9 }));
    assert_eq!(s.relay_states, vec!["binding".to_string(), "bound".to_string()]);
    let md = markdown(&r);
    assert!(md.contains("| 송신 큐 p99 중앙 / p95 / 최대 (ms) | 0.10 / 0.90 / 0.90 |"), "{md}");
    assert!(md.contains("상태 binding → bound · RTT 중앙 / p95 / 최대 30.00 / 30.00 / 30.00"), "{md}");
    assert!(md.contains("| NAT 추정(STUN) | 없음(format 3 이전) |"), "{md}");
}
