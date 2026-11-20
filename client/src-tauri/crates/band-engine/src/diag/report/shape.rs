//! 진단 타임라인의 손실 모양(P2-17h `lossShape`) 누적 — `examples/loss_shape_estimate.rs`(P2-17h)에서 옮겼다. 그 도구와
//! `examples/diag_report.rs`가 같이 쓴다.
//!
//! - `timeline.jsonl`: 피어 샘플의 `lossShape`는 "그 피어 항목 동안 누적"이라 (세션, 피어)마다 마지막 값을 쓴다. 같은 세션에서
//!   누적이 줄면(피어가 나갔다 다시 들어와 새 항목) 앞 구간을 따로 모아 더한다. 피어가 나간 뒤 내보내도 타임라인에는 남는다.
//!   timeline은 최근 60분만 남는다 — 세션이 길면 그보다 앞선 구간(앞선 재입장 항목 포함)은 합에서 빠진다.
//! - 프레임 길이(F)별로 따로 집계한다: 도착 여유 ≥F와 1F 늦춘 복사 추정은 F마다 뜻이 달라 섞으면 틀어진다. 한 항목 안에 상대가
//!   프리셋을 바꿔 `frameMs`가 바뀌면 그 시점 누적을 빼서 구간을 나눈다(바뀌기 직전·직후 두 샘플 사이 몫은 새 F 구간에 들어간다).
//! - `engine.json`: `lastStats.peers[].lossShape`(내보내기 순간의 피어만). 한 항목 안의 F 변경은 나눌 수 없다.

use std::collections::BTreeMap;

use band_dsp::jitter::{LossShapeStats, MARGIN_BUCKETS, RUN_BUCKETS};
use serde_json::Value;

/// 프레임 길이 키(µs). 도착 여유 ≥F와 1F 추정은 F마다 뜻이 달라 F가 다른 통계를 한 히스토그램에 섞지 않는다(#230 재리뷰 M1).
pub fn fkey(frame_ms: f32) -> u32 {
    (frame_ms.max(0.0) * 1000.0).round() as u32
}

/// 손실 모양 항목: (이름, 프레임 키) → 같은 F 구간들의 합.
pub type Shapes = BTreeMap<(String, u32), LossShapeStats>;

/// (세션, 피어, 프레임 키) → 같은 F 구간들의 합.
pub(crate) type SessionShapes = BTreeMap<(u64, String, u32), LossShapeStats>;

/// 두 누적 통계의 합. 같은 F끼리만 더한다(`frame_ms`는 0이 아닌 쪽).
pub(crate) fn add(a: &LossShapeStats, b: &LossShapeStats) -> LossShapeStats {
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

/// 누적 `a`에서 앞 구간 끝 누적 `b`를 뺀 구간 몫(`frame_ms`는 `a`의 것).
pub(crate) fn sub(a: &LossShapeStats, b: &LossShapeStats) -> LossShapeStats {
    let mut s = *a;
    for i in 0..RUN_BUCKETS {
        s.loss_runs[i] = a.loss_runs[i].saturating_sub(b.loss_runs[i]);
        s.late_runs[i] = a.late_runs[i].saturating_sub(b.late_runs[i]);
    }
    for i in 0..MARGIN_BUCKETS {
        s.arrival_margin[i] = a.arrival_margin[i].saturating_sub(b.arrival_margin[i]);
    }
    s.loss_long_pkts = a.loss_long_pkts.saturating_sub(b.loss_long_pkts);
    s.conceal_fec = a.conceal_fec.saturating_sub(b.conceal_fec);
    s.conceal_plc = a.conceal_plc.saturating_sub(b.conceal_plc);
    s.conceal_pcm = a.conceal_pcm.saturating_sub(b.conceal_pcm);
    s.conceal_underrun = a.conceal_underrun.saturating_sub(b.conceal_underrun);
    s
}

/// 누적 크기(줄었으면 새 피어 항목).
pub(crate) fn weight(s: &LossShapeStats) -> u64 {
    s.arrival_margin.iter().sum::<u64>() + s.conceal_fec + s.conceal_plc + s.conceal_pcm
}

pub(crate) fn is_empty(s: &LossShapeStats) -> bool {
    weight(s) == 0 && s.loss_runs.iter().all(|&n| n == 0) && s.late_runs.iter().all(|&n| n == 0)
}

/// 같은 키면 더한다. 빈 구간은 버린다.
fn insert<K: Ord>(m: &mut BTreeMap<K, LossShapeStats>, key: K, sh: LossShapeStats) {
    if is_empty(&sh) {
        return;
    }
    let e = m.entry(key).or_default();
    *e = add(e, &sh);
}

/// 샘플 한 줄(또는 `lastStats`)의 피어별 손실 모양. `lossShape`가 없는 피어(P2-17h 이전 빌드)는 건너뛴다.
pub(crate) fn peer_shapes(sample: &Value) -> Vec<(String, LossShapeStats)> {
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

/// (세션, 피어) 하나의 구간 추적: 끝난 구간들, 현재 구간 시작 시점 누적, 마지막 누적.
#[derive(Default)]
struct Track {
    done: Vec<LossShapeStats>,
    base: LossShapeStats,
    last: LossShapeStats,
}

/// 이미 읽은 타임라인 줄들 → (세션, 피어, F)별 누적 손실 모양.
pub(crate) fn from_lines<'a>(lines: impl IntoIterator<Item = &'a Value>) -> SessionShapes {
    let mut acc: BTreeMap<(u64, String), Track> = BTreeMap::new();
    for v in lines {
        let session = v.get("session").and_then(Value::as_u64).unwrap_or(0);
        for (peer, sh) in peer_shapes(v) {
            let e = acc.entry((session, peer)).or_default();
            if weight(&sh) < weight(&e.last) {
                // 누적이 줄었다 — 새 피어 항목(재입장). 앞 항목의 마지막 구간을 닫는다.
                e.done.push(sub(&e.last, &e.base));
                e.base = LossShapeStats::default();
            } else if fkey(sh.frame_ms) != fkey(e.last.frame_ms) && !is_empty(&e.last) {
                // 같은 항목 안에서 상대 프레임이 바뀌었다 — 그 시점 누적을 빼서 구간을 나눈다.
                e.done.push(sub(&e.last, &e.base));
                e.base = e.last;
            }
            e.last = sh;
        }
    }
    let mut out = SessionShapes::new();
    for ((session, peer), t) in acc {
        for seg in t.done.iter().copied().chain(std::iter::once(sub(&t.last, &t.base))) {
            insert(&mut out, (session, peer.clone(), fkey(seg.frame_ms)), seg);
        }
    }
    out
}

/// `timeline.jsonl` 텍스트 → (피어(세션 표시 포함), F)별 누적 손실 모양. JSON이 아닌 줄은 건너뛴다.
pub fn from_timeline(text: &str) -> Shapes {
    let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut out = Shapes::new();
    for ((session, peer, f), sh) in from_lines(&lines) {
        insert(&mut out, (format!("세션 {session} {peer}"), f), sh);
    }
    out
}

/// `engine.json` → `lastStats.peers[]`. 한 항목 안의 F 변경은 나눌 수 없다(마지막 F로 읽힌다).
pub fn from_engine_json(text: &str) -> Shapes {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let mut out = Shapes::new();
    for (peer, sh) in v.get("lastStats").map(peer_shapes).unwrap_or_default() {
        insert(&mut out, (peer, fkey(sh.frame_ms)), sh);
    }
    out
}

/// F별 합(항목 수와 함께). 다른 F는 더하지 않는다.
pub fn totals<'a>(items: impl IntoIterator<Item = &'a LossShapeStats>) -> BTreeMap<u32, (LossShapeStats, usize)> {
    let mut t: BTreeMap<u32, (LossShapeStats, usize)> = BTreeMap::new();
    for sh in items {
        let e = t.entry(fkey(sh.frame_ms)).or_default();
        e.0 = add(&e.0, sh);
        e.1 += 1;
    }
    t
}
