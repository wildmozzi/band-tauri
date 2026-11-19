//! 진단 내보내기의 손실 모양(P2-17h `lossShape`)으로 복사·FEC 효과를 추정한다([사람] 실합주 수집 → P2-17b 결정).
//!
//! ```text
//! cd client/src-tauri
//! cargo run -p band-engine --example loss_shape_estimate -- <진단.zip | timeline.jsonl | engine.json> [...]
//! ```
//!
//! - zip이면 안의 `timeline.jsonl`(없으면 `engine.json`)을 읽는다.
//! - `timeline.jsonl`: 피어 샘플의 `lossShape`는 "그 피어 항목 동안 누적"이라 (세션, 피어)마다 마지막 값을 쓴다. 같은 세션에서
//!   누적이 줄면(피어가 나갔다 다시 들어와 새 항목) 앞 구간을 따로 모아 더한다. 피어가 나간 뒤 내보내도 타임라인에는 남는다.
//! - `engine.json`: `lastStats.peers[].lossShape`(내보내기 순간의 피어만 — 이미 나간 피어는 없다).
//!
//! 출력: 피어마다(+ 전체 합) 손실·마감 놓침 연속 분포, 도착 여유 ≥ F 비율, 바로 복사 / 1F 늦춘 복사·FEC 추정(손실 대비 /
//! 은닉 원인 대비). 식은 `LossShapeStats::estimate`(band-dsp `jitter/shape.rs` 머리말)와 같다. 연속 길이는 256패킷 창을 벗어나야
//! 확정되므로 내보내기 직전 1초쯤의 손실은 빠진다 — 끝내기 전 1초 이상 정상 연주한 뒤 내보낸다.

use std::collections::BTreeMap;
use std::io::Read;

use band_dsp::jitter::{LossShapeStats, MARGIN_BUCKETS, RUN_BUCKETS};
use serde_json::Value;

/// 두 누적 통계의 합(`frame_ms`는 나중 것).
fn add(a: &LossShapeStats, b: &LossShapeStats) -> LossShapeStats {
    let mut s = *a;
    s.frame_ms = if b.frame_ms > 0.0 { b.frame_ms } else { a.frame_ms };
    for i in 0..RUN_BUCKETS {
        s.loss_runs[i] += b.loss_runs[i];
        s.late_runs[i] += b.late_runs[i];
    }
    for i in 0..MARGIN_BUCKETS {
        s.arrival_margin[i] += b.arrival_margin[i];
    }
    s.loss_long_pkts += b.loss_long_pkts;
    s.conceal_fec += b.conceal_fec;
    s.conceal_plc += b.conceal_plc;
    s.conceal_pcm += b.conceal_pcm;
    s.conceal_underrun += b.conceal_underrun;
    s
}

/// 누적 크기(줄었으면 새 피어 항목).
fn weight(s: &LossShapeStats) -> u64 {
    s.arrival_margin.iter().sum::<u64>() + s.conceal_fec + s.conceal_plc + s.conceal_pcm
}

fn peer_shapes(sample: &Value) -> Vec<(String, LossShapeStats)> {
    let Some(peers) = sample.get("peers").and_then(Value::as_array) else {
        return Vec::new();
    };
    peers
        .iter()
        .filter_map(|p| {
            let sh = serde_json::from_value::<LossShapeStats>(p.get("lossShape")?.clone()).ok()?;
            let name = p.get("peer").or_else(|| p.get("peerId")).and_then(Value::as_str).unwrap_or("?").to_string();
            Some((name, sh))
        })
        .collect()
}

/// `timeline.jsonl` → 피어(세션 표시 포함)별 누적 손실 모양.
fn from_timeline(text: &str) -> BTreeMap<String, LossShapeStats> {
    // (세션, 피어) → (지난 구간 합, 현재 구간 마지막 값)
    let mut acc: BTreeMap<(u64, String), (LossShapeStats, LossShapeStats)> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let session = v.get("session").and_then(Value::as_u64).unwrap_or(0);
        for (peer, sh) in peer_shapes(&v) {
            let e = acc.entry((session, peer)).or_default();
            if weight(&sh) < weight(&e.1) {
                e.0 = add(&e.0, &e.1);
            }
            e.1 = sh;
        }
    }
    acc.into_iter().map(|((session, peer), (done, last))| (format!("세션 {session} {peer}"), add(&done, &last))).collect()
}

/// `engine.json` → `lastStats.peers[]`.
fn from_engine_json(text: &str) -> BTreeMap<String, LossShapeStats> {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    v.get("lastStats").map(peer_shapes).unwrap_or_default().into_iter().collect()
}

fn load(path: &str) -> Result<BTreeMap<String, LossShapeStats>, String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("{path}: {e}"))?;
        let mut read = |name: &str| -> Option<String> {
            let mut s = String::new();
            z.by_name(name).ok()?.read_to_string(&mut s).ok()?;
            Some(s)
        };
        if let Some(t) = read("timeline.jsonl").map(|t| from_timeline(&t)).filter(|m| !m.is_empty()) {
            return Ok(t);
        }
        return Ok(read("engine.json").map(|t| from_engine_json(&t)).unwrap_or_default());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(if lower.ends_with(".jsonl") { from_timeline(&text) } else { from_engine_json(&text) })
}

fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "-".into())
}

fn report(name: &str, sh: &LossShapeStats) -> String {
    let e = sh.estimate();
    let total: u64 = sh.arrival_margin.iter().sum();
    let f = sh.frame_ms as f64;
    let ge_f = match e.margin_ge_f {
        Some(v) => format!("{:.1}%", v * 100.0),
        None => format!("- (F {f:.3}ms가 버킷 경계 2.5·5·10ms가 아님)"),
    };
    format!(
        "{name}\n  F {f}ms · 손실 연속 {:?}(17+ 패킷 {}) · 마감 놓침 연속 {:?}\n  도착 여유 표본 {total} · <0 {:.1}% · ≥F {ge_f}\n  은닉: FEC {} PLC {} PCM {} (언더런 {})\n  추정 손실 {:.0} · 마감 놓침 {:.0} · 연속 {}\n  바로 복사: 손실 대비 {} / 은닉 원인 대비 {}\n  1F 늦춘 복사·FEC: 손실 대비 {} / 은닉 원인 대비 {}\n",
        sh.loss_runs,
        sh.loss_long_pkts,
        sh.late_runs,
        sh.arrival_margin[0] as f64 * 100.0 / total.max(1) as f64,
        sh.conceal_fec,
        sh.conceal_plc,
        sh.conceal_pcm,
        sh.conceal_underrun,
        e.lost,
        e.late,
        e.runs,
        pct(Some(e.dup_now_of_loss)),
        pct(Some(e.dup_now_of_causes)),
        pct(e.dup_1f_of_loss),
        pct(e.dup_1f_of_causes),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("사용법: cargo run -p band-engine --example loss_shape_estimate -- <진단.zip | timeline.jsonl | engine.json> [...]");
        std::process::exit(2);
    }
    let mut all = LossShapeStats::default();
    let mut n = 0;
    for path in &args {
        match load(path) {
            Ok(m) if m.is_empty() => eprintln!("{path}: lossShape가 있는 피어 샘플이 없습니다(P2-17h 이전 빌드이거나 합주 기록 없음)"),
            Ok(m) => {
                for (name, sh) in &m {
                    print!("{}", report(&format!("[{path}] {name}"), sh));
                    all = add(&all, sh);
                    n += 1;
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    if n > 1 {
        print!("{}", report(&format!("전체 {n}개 피어 항목 합"), &all));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"t":1,"session":3,"peers":[{"peer":"id-aaaa","lossShape":{"frameMs":5.0,"lossRuns":[4,1,0,0,0,0],"lateRuns":[2,0,0,0,0,0],"arrivalMargin":[2,0,0,0,0,0,0,50,48,0],"concealFec":0,"concealPlc":0,"concealPcm":8,"concealUnderrun":1}}]}"#;

    #[test]
    fn timeline_takes_last_and_sums_reentries() {
        // 같은 세션: 누적이 자라다(마지막 값) → 줄면(재입장) 앞 구간을 더한다. 옛 기록(lossLongPkts 없음)도 읽는다.
        let grown = LINE.replace("[4,1,0,0,0,0]", "[6,1,0,0,0,0]").replace("[2,0,0,0,0,0,0,50,48,0]", "[2,0,0,0,0,0,0,60,48,0]");
        let reentry = LINE.replace("[2,0,0,0,0,0,0,50,48,0]", "[0,0,0,0,0,0,0,5,5,0]");
        let text = format!("{LINE}\n{grown}\n{reentry}\nnot json\n");
        let m = from_timeline(&text);
        let sh = m.get("세션 3 id-aaaa").expect("피어");
        assert_eq!(sh.loss_runs, [10, 2, 0, 0, 0, 0]);
        assert_eq!(sh.arrival_margin[7], 65);
        let e = sh.estimate();
        assert_eq!((e.lost, e.runs), (14.0, 12));
        assert!(e.dup_1f_of_loss.is_some());
    }

    #[test]
    fn engine_json_last_stats() {
        let json = format!(r#"{{"config":null,"lastStats":{LINE},"devices":[]}}"#);
        let m = from_engine_json(&json);
        assert_eq!(m.len(), 1);
        let r = report("x", m.values().next().unwrap());
        assert!(r.contains("≥F 98.0%") && r.contains("1F 늦춘 복사·FEC"), "{r}");
    }

    #[test]
    fn reads_zip() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("loss_shape_estimate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diag.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        w.start_file("timeline.jsonl", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(LINE.as_bytes()).unwrap();
        w.finish().unwrap();
        let m = load(path.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(m.get("세션 3 id-aaaa").map(|s| s.loss_runs[0]), Some(4));
    }
}
