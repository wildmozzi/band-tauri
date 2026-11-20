//! 보고서 → 사람이 읽는 Markdown(표). 숫자는 `Report`(= `--json`)와 같은 값이다.

use std::fmt::Write;

use super::{fmt_s, pct_range, silent_text, ConnAgg, Dist, FileReport, GoalRow, PeerReport, Report, SessionReport};

fn none() -> String {
    "없음".into()
}

fn ms(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.2}ms")).unwrap_or_else(none)
}

fn ms0(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.0}ms")).unwrap_or_else(none)
}

fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.2}%")).unwrap_or_else(none)
}

fn dist(d: &Option<Dist>) -> String {
    d.as_ref().map(|d| format!("{:.2} / {:.2} / {:.2}", d.median, d.p95, d.max)).unwrap_or_else(none)
}

fn opt<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref().map(|v| v.to_string()).unwrap_or_else(none)
}

/// 표 칸 안의 `|`를 막는다.
fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn split3(c: [u64; 3]) -> String {
    let n: u64 = c.iter().sum();
    if n == 0 {
        return "표본 0".into();
    }
    let p = |x: u64| x as f64 * 100.0 / n as f64;
    format!("P {:.1}% · E {:.1}% · LP-E {:.1}% (표본 {n})", p(c[0]), p(c[1]), p(c[2]))
}

fn goal_table(out: &mut String, rows: &[GoalRow]) {
    let _ = writeln!(out, "| GOAL | 지표 | 목표 | 측정 | 판정 | 비고 |\n|---|---|---|---|---|---|");
    for r in rows {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | **{}** | {} |",
            r.id,
            cell(&r.name),
            cell(&r.target),
            cell(&r.measured),
            r.verdict.label(),
            cell(r.note.as_deref().unwrap_or(""))
        );
    }
}

fn session_summary(out: &mut String, f: &FileReport, s: &SessionReport) {
    let kind = match s.network {
        Some(true) => "합주",
        Some(false) => "테스트 모드",
        None => "종류 없음",
    };
    let _ = writeln!(out, "### [{}] 세션 {} ({kind})\n", f.source, s.session);
    let _ = writeln!(out, "| 항목 | 값 |\n|---|---|");
    let _ = writeln!(out, "| 앱 버전 · 진단 형식 | {} · format {} |", opt(&f.app_version), opt(&f.format));
    let _ = writeln!(out, "| OS | {} |", cell(&opt(&f.os)));
    let _ = writeln!(out, "| 내보낸 시각 | {} |", opt(&f.exported_at));
    let _ = writeln!(out, "| 시작 – 끝 (길이) | {} – {} ({}, 실행 샘플 {}) |", opt(&s.start), opt(&s.end), fmt_s(s.duration_s), s.running_samples);
    let _ = writeln!(out, "| 백엔드 | {} |", opt(&s.backend));
    let _ = writeln!(
        out,
        "| 장치 버퍼 · 레이트 · 주기 | {} · {}Hz · {} |",
        opt(&s.device_buffer),
        opt(&s.sample_rate),
        ms(s.device_period_ms)
    );
    match &s.config {
        Some(c) => {
            let _ = writeln!(out, "| 드라이버(입력 / 출력) | {} / {} |", c.input_driver, c.output_driver);
            let _ = writeln!(
                out,
                "| 프리셋 · 프레임 · 코덱 | {} · {}ms · {} {}ch · 앱 버퍼 {} · 지터 프로필 {} |",
                c.preset,
                c.frame_ms,
                c.codec,
                c.channels,
                c.buffer_size,
                opt(&c.jitter_profile)
            );
        }
        None => {
            let _ = writeln!(out, "| 드라이버 · 프리셋 · 프레임 | 없음(engine.json 설정은 마지막 세션 것만 있다) |");
        }
    }
    let _ = writeln!(out, "| 처리 지연 est 중앙 / p95 / 최대 (ms) | {} |", dist(&s.est_latency_ms));
    let _ = writeln!(out, "| 송신 큐 p99 중앙 / p95 / 최대 (ms) | {} |", dist(&s.send_queue_p99_ms));
    let nat = match (&s.nat, s.nat_responses) {
        (Some(m), r) => {
            let label = match m.as_str() {
                "independent" => "매핑 독립(콘형)",
                "dependent" => "매핑 목적지별(대칭형)",
                "open" => "NAT 없음",
                _ => "모름",
            };
            format!("{m} — {label}, STUN 응답 {}", opt(&r))
        }
        (None, _) => "없음(format 3 이전)".into(),
    };
    let _ = writeln!(out, "| NAT 추정(STUN) | {nat} |");
    let relay = if s.relay_states.is_empty() {
        "안 씀".to_string()
    } else {
        format!("상태 {} · RTT 중앙 / p95 / 최대 {}", s.relay_states.join(" → "), dist(&s.relay_rtt_ms))
    };
    let _ = writeln!(out, "| 릴레이(relay.state · rttMs) | {relay} |");
    let cp = match &s.core_placement {
        None => none(),
        Some(p) => {
            let mut t = format!("워커 {}", split3(p.worker));
            if let Some(r) = p.net_recv {
                let _ = write!(t, " / 수신 {}", split3(r));
            }
            if p.legacy_lines > 0 {
                let _ = write!(t, " (옛 비율 형식 {}줄 제외)", p.legacy_lines);
            }
            t
        }
    };
    let _ = writeln!(out, "| 코어 배치(corePlacement) | {cp} |");
    let tp = s.thread_policy.as_ref().map(|v| v.to_string()).unwrap_or_else(none);
    let _ = writeln!(out, "| threadPolicy | {} |", cell(&tp));
    let steps = if s.xrun_steps.is_empty() {
        String::new()
    } else {
        let mut v: Vec<String> = s.xrun_steps.iter().take(12).map(|x| format!("{} +{}", fmt_s(x.at_s), x.add)).collect();
        if s.xrun_steps.len() > 12 {
            v.push(format!("…외 {}번", s.xrun_steps.len() - 12));
        }
        format!(" — 증가 시점 {}", v.join(", "))
    };
    let _ = writeln!(out, "| xrun(세션 누적) · 스트림 오류 | {} · {}{steps} |", opt(&s.xruns), opt(&s.stream_errors));
    let _ = writeln!(out);
}

fn peer_table(out: &mut String, peers: &[PeerReport]) {
    if peers.is_empty() {
        let _ = writeln!(out, "피어 없음.\n");
        return;
    }
    let _ = writeln!(
        out,
        "| 피어 | 경로(연결 샘플) | 내 송신 코덱(연결 샘플) | RTT 중앙 | bufferMs 중앙 / p95 / 최대 | 목표 중앙 | ±3ms 밖 1분+ 구간 (최장, 밖 %, 기록, 틈) | 은닉 % 하한–상한(상한 = 소리 없는 시간을 은닉으로 봄) ÷ 재생 프레임 (FEC / PLC / PCM · 언더런) | 소리 없는 시간(연결 중 무수신·리버퍼) | 손실 % (복구 후 lossShape 하한–상한 · 엔진 평균) | 연결 / 첫 소리(피어 추가 후) / 입장 후 첫 소리 |\n|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for p in peers {
        let paths = if p.paths.is_empty() { none() } else { p.paths.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ") };
        let band = p
            .band
            .as_ref()
            .map(|b| format!("{} ({:.0}s, {:.1}%, 기록 {}, 틈 {})", b.over_limit, b.longest_s, b.outside_pct, fmt_s(b.covered_s), b.gaps))
            .unwrap_or_else(|| "없음(targetMs 미기록)".into());
        let conceal = p
            .conceal
            .as_ref()
            .map(|c| match c.pct {
                Some(_) => format!("{} ({} / {} / {} · {})", pct_range(c.pct, c.pct_hi), pct(c.fec_pct), pct(c.plc_pct), pct(c.pcm_pct), pct(c.underrun_pct)),
                None => format!("분모 없음(은닉 {}프레임, produced 미기록)", c.concealed),
            })
            .unwrap_or_else(none);
        let silent = p.conceal.as_ref().map(silent_text).unwrap_or_else(none);
        let loss = format!("{} · {}", p.conceal.as_ref().map(|c| pct_range(c.loss_pct, c.loss_pct_hi)).unwrap_or_else(none), pct(p.loss_pct_engine_mean));
        let list = |v: &[f64]| if v.is_empty() { none() } else { v.iter().map(|x| format!("{x:.0}")).collect::<Vec<_>>().join(", ") + "ms" };
        let mut first_audio = list(&p.first_audio_ms);
        if p.no_audio_entries > 0 {
            let _ = write!(first_audio, " · 소리 못 받음 {}", p.no_audio_entries);
        }
        let entries = if p.entries > 1 { format!(" (항목 {})", p.entries) } else { String::new() };
        let tx = if p.tx_codec.is_empty() { none() } else { p.tx_codec.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ") };
        let mut entry_audio = list(&p.entry_first_audio_ms);
        if p.no_lead_entries > 0 {
            let _ = write!(entry_audio, " · 입장 값 없음 {}(뒤에 온 연주자·재추가 또는 옛 형식)", p.no_lead_entries);
        }
        let _ = writeln!(
            out,
            "| {}{entries} | {paths} | {} | {} | {} | {} | {band} | {conceal} | {silent} | {loss} | {} / {} / {} |",
            p.peer,
            cell(&tx),
            ms(p.rtt_ms.as_ref().map(|d| d.median)),
            dist(&p.buffer_ms),
            ms(p.target_ms.as_ref().map(|d| d.median)),
            list(&p.connect_ms),
            first_audio,
            entry_audio,
        );
    }
    let _ = writeln!(out);
    let shapes: Vec<_> = peers.iter().flat_map(|p| p.loss_shape.iter().map(move |s| (p, s))).collect();
    if shapes.is_empty() {
        let _ = writeln!(out, "손실 모양: 없음(lossShape는 P2-17h부터).\n");
        return;
    }
    let _ = writeln!(out, "손실 모양(`loss_shape_estimate`와 같은 누적, 연속 버킷 1·2·3–4·5–8·9–16·17+ 패킷):\n");
    for (p, s) in shapes {
        let _ = writeln!(
            out,
            "- {} F {}ms: 손실 연속 {:?} · 마감 놓침 연속 {:?} · 추정 손실 {:.0} · 마감 놓침 {:.0} · 도착 여유 ≥F {} · 1F 늦춘 복사·FEC 손실 대비 {}",
            p.peer,
            s.frame_ms,
            s.loss_runs,
            s.late_runs,
            s.lost,
            s.late,
            s.margin_ge_f.map(|v| format!("{:.1}%", v * 100.0)).unwrap_or_else(none),
            s.dup_1f_of_loss.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(none),
        );
    }
    let _ = writeln!(out);
}

fn conn_rows(out: &mut String, s: &SessionReport) {
    if s.conn.is_empty() {
        let _ = writeln!(out, "연결 기록(conn): 없음.\n");
        return;
    }
    let _ = writeln!(out, "| 시각(세션 기준) | 피어 | 단계 | 결과 | 소요 |\n|---|---|---|---|---|");
    for c in &s.conn {
        let _ = writeln!(out, "| {} | {} | {} | {} | {} |", fmt_s(c.at_s), c.peer, c.phase, c.result, ms0(c.ms));
    }
    let relay = s.conn.iter().any(|c| c.result == "relay");
    let _ = writeln!(out, "\n릴레이 폴백: {}\n", if relay { "있음" } else { "없음" });
    if s.pending > 0 {
        let _ = writeln!(out, "내보낼 때 아직 연결 중인 시도(미완, 결과 없음): {}\n", s.pending);
    }
}

fn conn_agg(out: &mut String, a: &ConnAgg) {
    let _ = writeln!(out, "| 결과 경로 | connect | connect 중앙 ms | reconnect | reconnect 중앙 ms |\n|---|---|---|---|---|");
    for r in &a.rows {
        let _ = writeln!(out, "| {} | {} | {} | {} | {} |", r.result, r.connect, ms0(r.connect_median_ms), r.reconnect, ms0(r.reconnect_median_ms));
    }
    let _ = writeln!(
        out,
        "\n시도 {} (connect + reconnect + 미완) · 성공 {} ({}) = 직접 {} ({}) + 릴레이 {} · 실패 {} · 중단 {} · 미완 {} · recover {} · switch {} · 릴레이로 붙은 세션 {}/{}\n",
        a.attempts,
        a.success,
        pct(a.success_pct),
        a.direct,
        pct(a.direct_pct),
        a.relay,
        a.failed,
        a.aborted,
        a.pending,
        a.recover,
        a.switch,
        a.sessions_with_relay,
        a.sessions
    );
    let _ = writeln!(out, "- 성공률 분모에는 중단(결과 전 퇴장·엔진 정지)이 든다(`net::connlog` — 빼면 위로 치우친다). recover·switch는 시도가 아니다.");
    if a.pending > 0 {
        let _ = writeln!(
            out,
            "- 미완 {}회: 내보낼 때 아직 연결 중이라 결과가 없는 시도. 분모에 넣고 성공으로 세지 않았다 — 모두 성공했어도 98% 아래일 때만 미달로 가른다.",
            a.pending
        );
    }
    let _ = writeln!(
        out,
        "- 시도는 기록 기준이다: 한 합주의 두 사람 zip을 함께 주면 같은 연결이 양쪽에서 한 번씩 세어진다. 98% 문턱은 실제 시도 하한 {}회(시간이 겹치는 다른 사람 기록은 큰 쪽만, 같은 사람이 여러 번 내보낸 같은 세션은 합침)로 가른다.",
        a.attempts_lower
    );
    if a.unknown_results > 0 {
        let _ = writeln!(out, "- 결과를 모르는 시도 {}회(새 형식)는 위 수에서 뺐다.", a.unknown_results);
    }
    if !a.duplicate_files.is_empty() {
        let _ = writeln!(out, "- 같은 zip이 두 번 들어와 집계에서 뺐다: {}", a.duplicate_files.join(", "));
    }
    let nats = a.nat_sessions.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ");
    let _ = writeln!(
        out,
        "- NAT 추정(세션 수): {} · 내보낸 사람 {} · 짝 확인된 쌍 {} · 콘형 쌍 {}(쌍들 합 실제 시도 하한 {}회 · 양쪽 기록: 직접 {} · 릴레이/실패 {} · 중단/미완 {}). 짝 = 같은 시간에 서로를 피어로 본 두 사람의 zip(selfPeer로 맞춤, 같은 PC가 여러 번 내보낸 zip은 한 사람), 콘형 쌍 = 그 둘이 모두 independent·open.",
        if nats.is_empty() { none() } else { nats },
        a.endpoints,
        a.pairs,
        a.cone_pairs,
        a.cone_attempts,
        a.cone_direct,
        a.cone_nondirect,
        a.cone_unresolved
    );
    let _ = writeln!(out);
}

/// 보고서 → Markdown.
pub(crate) fn markdown(r: &Report) -> String {
    let mut out = String::new();
    let n_sessions: usize = r.files.iter().map(|f| f.sessions.len()).sum();
    let _ = writeln!(out, "# 진단 보고서 — zip {}개, 세션 {n_sessions}개\n", r.files.len());
    for f in &r.files {
        let _ = writeln!(out, "## {}\n", f.source);
        if let Some(d) = &f.duplicate_of {
            let _ = writeln!(out, "- 주의: {d}와 타임라인이 똑같다(같은 zip) — 연결 집계에서 뺐다.");
        }
        if f.bad_lines > 0 {
            let _ = writeln!(out, "- 주의: JSON이 아닌 타임라인 줄 {}개를 건너뛰었다.", f.bad_lines);
        }
        if f.timeline_dropped.is_some_and(|d| d > 0) {
            let _ = writeln!(out, "- 주의: 타임라인이 60분을 넘어 앞 샘플 {}개가 버려졌다(세션 앞부분 누락).", f.timeline_dropped.unwrap_or(0));
        }
        if f.sessions.is_empty() {
            let _ = writeln!(out, "타임라인 샘플 없음.\n");
        }
        for s in &f.sessions {
            session_summary(&mut out, f, s);
            let _ = writeln!(out, "#### 피어\n");
            peer_table(&mut out, &s.peers);
            let _ = writeln!(out, "#### 연결\n");
            conn_rows(&mut out, s);
            let _ = writeln!(out, "#### GOAL 판정 (1·2절)\n");
            goal_table(&mut out, &s.goal);
            let _ = writeln!(out);
        }
    }
    let _ = writeln!(out, "## 연결 성공률 집계 — 세션 {}개\n", r.conn.sessions);
    conn_agg(&mut out, &r.conn);
    let _ = writeln!(out, "## GOAL 판정 (3절, 모든 zip 합)\n");
    goal_table(&mut out, &r.goal);
    let _ = writeln!(
        out,
        "\n판정: 충족·미달은 진단 수치로 기준을 가른 것이고, 판정 불가는 데이터가 없거나 세션이 짧아 가를 수 없는 것이다. 청감·회선 종류(유선/Wi-Fi/NAT)는 기록표로 확인한다."
    );
    out
}
