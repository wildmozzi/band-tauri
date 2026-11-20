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
    assert_eq!(g("3-1"), Verdict::Unknown); // NAT 종류 없음
    assert_eq!(g("3-2"), Verdict::Unknown); // 시도 1회 < 50
    assert_eq!(g("3-3"), Verdict::Unknown); // 180.5ms지만 입장 → 피어 추가 구간을 몰라 충족이라 할 수 없다

    let md = markdown(&r);
    assert!(md.contains("| 1-1 |") && md.contains("**충족**") && md.contains("균형 · 5ms"), "{md}");
    assert!(md.contains("50 +2") || md.contains("0:50 +2"), "{md}");
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["files"][0]["sessions"][0]["peers"][0]["bufferMs"]["median"], 5.5);
    assert_eq!(json["goal"][2]["verdict"], "unknown");
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
    assert_eq!(conceal_verdict(1.2, 1.0, z).0, Verdict::Met);
    assert_eq!(conceal_verdict(1.4, 2.0, Some(30)).0, Verdict::Met); // 복구 후 손실 ≥ 1%면 회선 손실도 ≥ 1%
    assert_eq!(conceal_verdict(1.6, 1.0, z).0, Verdict::NotMet);
    assert_eq!(conceal_verdict(2.0, 0.1, z).0, Verdict::NotMet);
    assert_eq!(conceal_verdict(0.1, 0.0, z).0, Verdict::Unknown);
    assert_eq!(conceal_verdict(3.0, 2.5, z).0, Verdict::Unknown);
    // 사본·재전송이 손실을 메웠거나(버린 사본 > 0) 기록이 없으면 회선 손실이 1%보다 컸을 수 있다 → 미달이라 할 수 없다
    assert_eq!(conceal_verdict(2.0, 0.5, Some(12)).0, Verdict::Unknown);
    assert_eq!(conceal_verdict(2.0, 0.5, None).0, Verdict::Unknown);
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
