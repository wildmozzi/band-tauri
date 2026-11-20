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
        p.loss_shape = band_dsp::jitter::LossShapeStats {
            frame_ms: 5.0,
            loss_runs: [i / 2, 0, 0, 0, 0, 0],
            conceal_plc: i,
            arrival_margin: [0, 0, 0, 0, 0, 0, 0, 200 * i, 0, 0],
            ..Default::default()
        };
        let conn_events = if i == 0 { events.clone() } else { Vec::new() };
        let config = band_dsp::config::EngineConfig { frame_ms: 5.0, jitter_min_ms: Some(0.0), jitter_max_ms: Some(80.0), ..Default::default() };
        d.record(T0 + i * 1000, Some(EngineSnapshot { stats, config, network: true, session, conn_events }));
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
    assert_eq!((f.format, f.app_version.as_deref()), (Some(2), Some("9.9.9")));
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
    assert!(p.band.is_none(), "targetMs는 타임라인에 없다");
    let cc = p.conceal.as_ref().unwrap();
    // 119초 = 23800프레임, 은닉 119 → 0.5%, 확정 손실 59 → 0.248%
    assert!((cc.frames - 23_800.0).abs() < 1e-6);
    assert!((cc.pct - 0.5).abs() < 1e-9, "{}", cc.pct);
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
    assert_eq!(v("1-4"), vec![Verdict::Unknown]); // targetMs 없음
    assert_eq!(v("2-1"), vec![Verdict::Unknown]); // 은닉 0.5% ≤ 1.5%지만 손실 0.25% — 1% 회선이 아니다
    assert_eq!(v("2-2"), vec![Verdict::NotMet]); // 독점에서 xrun 2

    assert_eq!((r.conn.attempts, r.conn.success, r.conn.direct), (1, 1, 1));
    let g = |id: &str| goal_of(&r.goal, id)[0].verdict;
    assert_eq!(g("3-1"), Verdict::Unknown); // NAT 종류 없음
    assert_eq!(g("3-2"), Verdict::Unknown); // 시도 1회 < 50
    assert_eq!(g("3-3"), Verdict::Met); // 180.5ms

    let md = markdown(&r);
    assert!(md.contains("| 1-1 |") && md.contains("**충족**") && md.contains("균형 · 5ms"), "{md}");
    assert!(md.contains("50 +2") || md.contains("0:50 +2"), "{md}");
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["files"][0]["sessions"][0]["peers"][0]["bufferMs"]["median"], 5.5);
    assert_eq!(json["goal"][2]["verdict"], "met");
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
    // 프레임 길이를 알 수 없다(lossShape·remoteFrameMs·설정 없음) → 은닉 % 없음
    assert!(p.conceal.is_none() && p.loss_shape.is_empty() && p.first_audio_ms.is_empty());
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
    assert_eq!(conceal_verdict(1.2, 1.0).0, Verdict::Met);
    assert_eq!(conceal_verdict(1.4, 2.0).0, Verdict::Met);
    assert_eq!(conceal_verdict(1.6, 1.0).0, Verdict::NotMet);
    assert_eq!(conceal_verdict(2.0, 0.1).0, Verdict::NotMet);
    assert_eq!(conceal_verdict(0.1, 0.0).0, Verdict::Unknown);
    assert_eq!(conceal_verdict(3.0, 2.5).0, Verdict::Unknown);
}
