//! 장시간 하네스(P1-6): 6피어 풀메시 60분을 가상 시간으로 돌려 GOAL 1·2절 수치를 자동 판정한다.
//!
//! 모델은 `tune_sim::run_multi`(피어 6 → 지터 버퍼 → PeerFeed(워커) → OutputRt(콜백), 결정적)를 그대로 쓴다.
//! 이 엔진 하나가 원격 스트림 6개를 받는다(6인 풀메시에서 한 사람이 받는 것은 5개 — 1개 더 얹어 여유 있게 검증).
//! 엔진은 원격 스트림마다 독립된 지터 버퍼를 쓰므로 수신 쪽 하나가 풀메시 전체의 수신 경로를 대표한다. 스트림마다 다른 장치 드리프트(±200ppm), 무작위·버스트 손실(0.5~2%), 지터(분포·계단 변화), 와이파이 스파이크,
//! UDP 5초 단절로 흔든다. 측정만 하고 엔진 로직은 바꾸지 않는다.
//!
//! WS 재접속: `performSession.connectionLost`는 엔진을 유지하고(UDP 메시 계속), `welcome(resumed)`는 descriptor만 다시
//! 알린다. 즉 30초 이내 재접속은 엔진(지터·워커·콜백)에 아무 입력도 주지 않으므로 이 하네스의 대상이 아니다.
//! 세션 재개 실패(새 clientId)는 엔진 재시작이라 "끊김 없음" 범위 밖이다. UDP 단절 5초는 `net::peer`의 LOST_AFTER(5s)에
//! Reconnecting으로 바뀌지만 FAIL_AFTER(15s) 전이라 피어가 제거되지 않는다 → 지터 버퍼가 5초 공백을 겪는 것과 같다.
//!
//! 리포트: `SIM_REPORT_DIR`(없으면 OS 임시 폴더/band-tauri-sim)에 `sim-report-<태그>.md`와 `.json`.
//! - 짧은 버전(5분)은 기본 `cargo test`에서 돈다.
//! - 60분 전체: `cargo test --lib long_sim_6peers_60min -- --ignored --nocapture` (CI 야간 잡 `long-sim`).

use super::tune_sim::{run_multi, JitDist, MultiOut, MultiSim, NetPeer, PeerOut, Sample};
use serde_json::{json, Value};
use std::fmt::Write as _;

const SEC: u64 = 1_000_000;
/// 정착 시간(s): 이 뒤부터 지표를 잰다.
const SETTLE_S: f64 = 10.0;
/// GOAL 1절: bufferMs가 목표 ±3ms 밖에 1분 이상 머무르지 않음.
const BAND_MS: f32 = 3.0;
const BAND_MAX_S: f64 = 60.0;
/// 연속 구간 히스테리시스: 띠 안으로 이만큼(s) 이하 잠깐 들어왔다 나가면 같은 구간으로 이어 센다
/// (경계를 오가는 피어가 연속 판정을 피해 가지 않도록, PR #52 리뷰 M2).
const BAND_GAP_S: f64 = 1.0;
/// GOAL 1절 보조 판정: 띠 밖 총 시간 비율 상한(%). 60분 중 6분 — 연속 판정만으로는 절반 넘게 띠 밖인 피어가 PASS가 된다.
const BAND_PCT_MAX: f64 = 10.0;
/// GOAL 2절: 손실 1%에서 은닉 ≤ 1.5%.
const CONCEAL_GOAL_PCT: f64 = 1.5;
/// 언더런 은닉 예산(%): GOAL 2절(손실 1% → 은닉 ≤ 1.5%)에서 손실 몫을 뺀 0.5%p. 손실 은닉은 손실을 따라가므로
/// 지터 버퍼가 줄일 수 있는 것은 언더런 몫이다(P1-6a의 측정 기준).
const UNDERRUN_MAX_PCT: f64 = 0.5;
/// GOAL 2절: UDP 5초 단절 후 자동 복구 — 복구 시간 ≤ 5초.
const RECOVER_MAX_S: f64 = 5.0;
/// 지연 누적: 같은 망 조건의 첫 창과 마지막 창 bufferMs 중앙값 차이 상한(ms).
const ACCUM_MAX_MS: f32 = 1.0;
/// 은닉 비율에서 빼는 구간: 단절 시작부터 끝 + 이만큼(s).
const OUTAGE_EXCL_S: f64 = 10.0;

#[derive(Clone, Copy, PartialEq)]
enum LossRule {
    /// 무작위 손실 ≤ 1%: GOAL 2절 한도 1.5%.
    Goal,
    /// 버스트 손실·2%: 실제 손실률 + 0.5%p(GOAL 밖 회선, 은닉이 손실을 크게 넘지 않는지만 본다).
    ActualPlus,
}

/// 알려진 미달이 있을 수 있는 판정 지표.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Metric {
    /// 평시 은닉 비율(손실 + 언더런, %).
    Conceal,
    /// 평시 언더런 은닉 비율(%).
    Underrun,
    /// ±3ms 띠 밖 총 시간 비율(%).
    BandPct,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::Conceal => "은닉",
            Metric::Underrun => "언더런 은닉",
            Metric::BandPct => "±3ms 밖 비율",
        }
    }
}

/// 알려진 GOAL 미달: 원인 분석을 마치고 별도 백로그로 넘긴 항목. 리포트에는 FAIL(알려진)로 남기고 테스트는 통과시키되
/// 기준값(실행 길이별 측정값) ± `tol` 안에 있어야 한다(래칫, PR #52 리뷰 M3).
/// - 기준값 + tol 초과: 악화 → 실패.
/// - 기준값 − tol 미만: 개선 → 실패("기준값을 새 측정값으로 낮춰라"). 상한이 오래 남아 회귀를 숨기지 않게 한다.
/// - 한도 안으로 들어옴: 해소 → 실패("known 지정을 지워라").
///
/// 시뮬레이션은 결정적이라 같은 코드면 같은 값이 나온다. tol은 엔진 변경을 감지하는 문턱이다.
#[derive(Clone, Copy)]
struct Known {
    metric: Metric,
    /// 5분 버전 기준값(None = 이 길이에서는 미달 아님, 한도 안이어야 한다).
    short: Option<f64>,
    /// 60분 버전 기준값.
    long: Option<f64>,
    tol: f64,
    why: &'static str,
}

impl Known {
    fn base(&self, seconds: f64) -> Option<f64> {
        if seconds <= 300.0 {
            self.short
        } else {
            self.long
        }
    }
}

// 기준값 = P1-6 측정값(2026-11-11, 손실/언더런 분리·히스테리시스 반영 후). 고치는 PR에서 갱신하거나 지운다.
const TOL_PCT: f64 = 0.3;
const TOL_BAND: f64 = 5.0;
/// #1: 지수(꼬리 긴) 지터에서 목표(분위수 0.95 + 여유)를 넘는 늦은 패킷이 그대로 은닉된다. 손실 0일 때도 약 0.9% (P1-6b).
const TAIL: &str = "꼬리 긴 지터의 늦은 패킷 은닉(손실 0에서도 ~0.9%, P1-6b)";
/// #2·#5: 손실된 패킷 k의 은닉 마감에 k+1이 아직 없으면 언더런 은닉(seq 유지) 뒤 k+1 도착 시 k를 또 은닉 — 손실 1건에 2프레임 (P1-6a).
const DOUBLE: &str = "손실 직후 언더런 은닉 + 손실 은닉 이중 계산(P1-6a)";
// #1은 언더런 몫이 0.2%로 예산 안이다 — 초과분은 손실 몫(1.75%, 실제 손실 0.5%)의 늦은 패킷이다.
const KNOWN_1: &[Known] = &[
    Known { metric: Metric::Conceal, short: Some(1.93), long: Some(1.95), tol: TOL_PCT, why: TAIL },
    // 5분 버전만 10% 경계를 살짝 넘는다(60분 7.1%).
    Known { metric: Metric::BandPct, short: Some(10.76), long: None, tol: TOL_BAND, why: "꼬리 긴 지터·계단 구간의 목표 흔들림(5분 버전만, P1-6b)" },
];
const KNOWN_2: &[Known] = &[
    Known { metric: Metric::Conceal, short: Some(2.16), long: Some(2.21), tol: TOL_PCT, why: DOUBLE },
    Known { metric: Metric::Underrun, short: Some(0.70), long: Some(0.71), tol: TOL_PCT, why: DOUBLE },
    Known { metric: Metric::BandPct, short: Some(50.4), long: Some(53.9), tol: TOL_BAND, why: "이중 은닉으로 깊이 +1F가 잦아 bufferMs가 목표 +3ms 근처에 머문다(P1-6a)" },
];
const KNOWN_5: &[Known] = &[
    Known { metric: Metric::Conceal, short: Some(2.75), long: Some(2.69), tol: TOL_PCT, why: DOUBLE },
    Known { metric: Metric::Underrun, short: Some(0.75), long: Some(0.70), tol: TOL_PCT, why: DOUBLE },
    Known { metric: Metric::BandPct, short: Some(30.6), long: Some(28.9), tol: TOL_BAND, why: "이중 은닉으로 깊이 +1F가 잦다(P1-6a)" },
];

struct Spec {
    np: NetPeer,
    rule: LossRule,
    known: &'static [Known],
}

/// 6피어 시나리오. 사건은 길이 비율로 놓아 5분 버전과 60분 버전이 같은 모양이 된다.
fn scenario(seconds: f64) -> Vec<Spec> {
    let at = |f: f64| (seconds * f * 1e6) as u64;
    let mut v = Vec::new();
    // #0 기준 회선: 손실 1%(무작위), 지터 1ms, +170ppm.
    let mut np = NetPeer::new(240, 1001);
    np.drift_ppm = 170.0;
    np.net_jitter_us = 1000;
    np.loss = 0.01;
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #1 RTT 계단: 지수 지터 2ms·손실 0.5%·−200ppm, 30%~60% 구간에 +8ms·지터 4ms.
    let mut np = NetPeer::new(240, 1002);
    np.in_buf = 96;
    np.drift_ppm = -200.0;
    np.net_jitter_us = 2000;
    np.dist = JitDist::Exp;
    np.loss = 0.005;
    np.steps = vec![(at(0.30), 8000, 4000), (at(0.60), 0, 2000)];
    v.push(Spec { np, rule: LossRule::Goal, known: KNOWN_1 });
    // #2 버스트 손실: 평균 3패킷 버스트(나쁨 진입 0.4%) + 무작위 0.3% ≈ 1.5%, +200ppm, 장치 버퍼 256.
    let mut np = NetPeer::new(240, 1003);
    np.in_buf = 256;
    np.drift_ppm = 200.0;
    np.net_jitter_us = 3000;
    np.loss = 0.003;
    np.burst = Some((0.004, 1.0 / 3.0));
    v.push(Spec { np, rule: LossRule::ActualPlus, known: KNOWN_2 });
    // #3 UDP 5초 단절 두 번(50%, 85%), 2.5ms 프레임, 손실 1%, 지터 5ms, −120ppm.
    let mut np = NetPeer::new(120, 1004);
    np.drift_ppm = -120.0;
    np.net_jitter_us = 5000;
    np.loss = 0.01;
    np.outages = vec![(at(0.50), 5 * SEC), (at(0.85), 5 * SEC)];
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #4 와이파이 스파이크(0.3초 +30ms)가 길이의 1/8마다, 손실 0.5%, +60ppm, 장치 버퍼 144.
    let mut np = NetPeer::new(240, 1005);
    np.in_buf = 144;
    np.drift_ppm = 60.0;
    np.net_jitter_us = 1000;
    np.loss = 0.005;
    np.spikes = (1..8).map(|k| (at(k as f64 / 8.0 + 0.01), 300_000, 30_000)).collect();
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #5 나쁜 회선: 손실 2%(무작위 1.5% + 버스트), 25%에 5초 단절, 70~80%에 지터 8ms, −160ppm, 장치 버퍼 480(10ms 몰아 보냄).
    let mut np = NetPeer::new(240, 1006);
    np.in_buf = 480;
    np.drift_ppm = -160.0;
    np.net_jitter_us = 2000;
    np.loss = 0.015;
    np.burst = Some((0.0025, 0.5));
    np.outages = vec![(at(0.25), 5 * SEC)];
    np.steps = vec![(at(0.70), 0, 8000), (at(0.80), 0, 2000)];
    v.push(Spec { np, rule: LossRule::ActualPlus, known: KNOWN_5 });
    v
}

fn describe(np: &NetPeer) -> String {
    let mut s = format!(
        "F {:.1}ms, 장치 {}샘플, 드리프트 {:+}ppm, 지터 {:?} {:.1}ms, 손실 {:.1}%",
        np.frame as f64 / 48.0,
        np.in_buf,
        np.drift_ppm,
        np.dist,
        np.net_jitter_us as f64 / 1000.0,
        np.loss * 100.0
    );
    if let Some((g, b)) = np.burst {
        let _ = write!(s, " + 버스트(진입 {:.2}%, 평균 {:.1}패킷)", g * 100.0, 1.0 / b);
    }
    for (t, len) in &np.outages {
        let _ = write!(s, ", 단절 {:.0}s@{:.0}s", *len as f64 / 1e6, *t as f64 / 1e6);
    }
    for (t, extra, j) in &np.steps {
        let _ = write!(s, ", 계단@{:.0}s(+{:.0}ms, 지터 {:.1}ms)", *t as f64 / 1e6, *extra as f64 / 1000.0, *j as f64 / 1000.0);
    }
    if !np.spikes.is_empty() {
        let _ = write!(s, ", 스파이크 {}회(0.3s +30ms)", np.spikes.len());
    }
    s
}

fn quantile(mut v: Vec<f32>, q: f64) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = ((v.len() as f64 * q).ceil() as usize).clamp(1, v.len()) - 1;
    v[k]
}

struct PeerResult {
    desc: String,
    frame_ms: f32,
    buf_med: f32,
    buf_p95: f32,
    tgt_med: f32,
    p95_jit: f32,
    /// ±3ms 밖(재생 안 함 포함) 최장 연속 시간(히스테리시스 BAND_GAP_S)과 그 시작(s), 총 시간(s), 총 비율(%).
    band_max_s: f64,
    band_max_at: f64,
    band_total_s: f64,
    band_pct: f64,
    /// 평시(단절·스파이크 구간과 계단 직후 10초 제외) 은닉 비율 — 판정 기준. 손실 몫 + 언더런 몫.
    conceal_pct: f64,
    conceal_loss_pct: f64,
    conceal_underrun_pct: f64,
    /// 단절 구간만 뺀 전체 은닉 비율(계단·스파이크 포함, 참고).
    conceal_all: f64,
    conceal_limit: f64,
    loss_pct: f64,
    rule: LossRule,
    known: &'static [Known],
    /// 단절별 (끝 시각 s, 복구 시간 s).
    recoveries: Vec<(f64, Option<f64>)>,
    buf_first: f32,
    buf_last: f32,
    off_first: f32,
    off_last: f32,
    rebuffers: u64,
    late_dropped: u64,
    corrections: u64,
    max_corr_cents: f32,
}

impl PeerResult {
    fn band_ok(&self) -> bool {
        self.band_max_s < BAND_MAX_S
    }
    fn recover_ok(&self) -> bool {
        self.recoveries.iter().all(|r| r.1.is_some_and(|s| s <= RECOVER_MAX_S))
    }
    fn accum(&self) -> f32 {
        self.buf_last - self.buf_first
    }
    fn accum_ok(&self) -> bool {
        self.accum().abs() <= ACCUM_MAX_MS
    }
    fn goal_delay_ok(&self) -> bool {
        self.buf_first <= self.frame_ms + self.p95_jit
    }
    fn value(&self, m: Metric) -> f64 {
        match m {
            Metric::Conceal => self.conceal_pct,
            Metric::Underrun => self.conceal_underrun_pct,
            Metric::BandPct => self.band_pct,
        }
    }
    fn limit(&self, m: Metric) -> f64 {
        match m {
            Metric::Conceal => self.conceal_limit,
            Metric::Underrun => UNDERRUN_MAX_PCT,
            Metric::BandPct => BAND_PCT_MAX,
        }
    }
    fn ok(&self, m: Metric) -> bool {
        self.value(m) <= self.limit(m)
    }
}

/// 지표 하나의 판정.
#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Pass,
    /// 알려진 미달, 기준값 ± tol 안.
    Known,
    Fail,
}

fn analyze(np: &NetPeer, rule: LossRule, known: &'static [Known], p: &PeerOut, seconds: f64) -> PeerResult {
    let steady: Vec<&Sample> = p.samples.iter().filter(|s| s.t >= SETTLE_S).collect();
    let playing_buf: Vec<f32> = steady.iter().filter(|s| s.playing).map(|s| s.buf).collect();
    let buf_med = quantile(playing_buf.clone(), 0.5);
    let buf_p95 = quantile(playing_buf, 0.95);
    let tgt_med = quantile(steady.iter().filter(|s| s.playing).map(|s| s.tgt).collect(), 0.5);

    // ±3ms 밖 연속 구간(재생 멈춤 = 밖). 띠 안 복귀가 BAND_GAP_S 이하면 같은 구간으로 이어 센다.
    let (mut run_start, mut last_out) = (None::<f64>, f64::NEG_INFINITY);
    let (mut band_max_s, mut band_max_at, mut band_total_s) = (0.0f64, 0.0f64, 0.0f64);
    for s in &steady {
        let out = !s.playing || (s.buf - s.tgt).abs() > BAND_MS;
        if out {
            band_total_s += 0.1;
            // 표본 간격 0.1s: 사이에 낀 띠 안 표본이 BAND_GAP_S 이하(≤ 10개)면 이어 센다.
            if run_start.is_none() || s.t - last_out > BAND_GAP_S + 0.1 + 1e-6 {
                run_start = Some(s.t);
            }
            last_out = s.t;
            let st = run_start.unwrap_or(s.t);
            if s.t - st + 0.1 > band_max_s {
                band_max_s = s.t - st + 0.1;
                band_max_at = st;
            }
        }
    }
    let band_pct = band_total_s * 100.0 / (steady.len() as f64 * 0.1).max(0.1);

    // 사건 구간: 단절(시작 ~ 끝 + 10초), 계단(시작 후 10초), 스파이크(시작 ~ 끝 + 10초).
    let outage_x: Vec<(f64, f64)> = np.outages.iter().map(|&(s, l)| (s as f64 / 1e6, (s + l) as f64 / 1e6 + OUTAGE_EXCL_S)).collect();
    let mut event_x = outage_x.clone();
    event_x.extend(np.steps.iter().map(|&(s, _, _)| (s as f64 / 1e6, s as f64 / 1e6 + OUTAGE_EXCL_S)));
    event_x.extend(np.spikes.iter().map(|&(s, l, _)| (s as f64 / 1e6, (s + l) as f64 / 1e6 + OUTAGE_EXCL_S)));
    // 은닉 비율(정착 후, 구간 제외): (전체, 손실 몫, 언더런 몫) %.
    let conceal_excl = |x: &[(f64, f64)]| {
        let (mut conc, mut conc_loss, mut prod) = (0u64, 0u64, 0u64);
        for w in steady.windows(2) {
            let (a, b) = (w[0], w[1]);
            if x.iter().any(|&(x0, x1)| b.t > x0 && a.t < x1) {
                continue;
            }
            conc += b.concealed - a.concealed;
            conc_loss += b.concealed_loss - a.concealed_loss;
            prod += b.produced - a.produced;
        }
        let pct = |n: u64| n as f64 * 100.0 / prod.max(1) as f64;
        (pct(conc), pct(conc_loss), pct(conc - conc_loss))
    };
    let conceal_all = conceal_excl(&outage_x).0;
    let (conceal_pct, conceal_loss_pct, conceal_underrun_pct) = conceal_excl(&event_x);
    let net_sent = (p.tx_sent - p.tx_outage).max(1);
    let loss_pct = p.tx_lost as f64 * 100.0 / net_sent as f64;
    let conceal_limit = match rule {
        LossRule::Goal => CONCEAL_GOAL_PCT,
        LossRule::ActualPlus => loss_pct + 0.5,
    };

    // 복구: 단절 끝 이후 처음으로 재생 중 + ±3ms 안이 1초 이어지는 시각.
    let recoveries = np
        .outages
        .iter()
        .map(|&(s, l)| {
            let e = (s + l) as f64 / 1e6;
            let ok = |x: &Sample| x.playing && (x.buf - x.tgt).abs() <= BAND_MS;
            let after: Vec<&Sample> = p.samples.iter().filter(|x| x.t >= e && x.t < e + 30.0).collect();
            let rec = after.iter().enumerate().find(|(k, x)| ok(x) && after[*k..].iter().take_while(|y| y.t <= x.t + 1.0).all(|y| ok(y))).map(|(_, x)| x.t - e);
            (e, rec)
        })
        .collect();

    // 지연 누적: 처음·마지막 창의 bufferMs 중앙값 비교. 창 안의 사건 구간(event_x)은 빼서 같은 망 조건만 비교한다
    // (5분 버전은 마지막 창에 #3 단절·#4 스파이크가 들어온다, PR #52 리뷰 M4).
    let win = accum_win(seconds);
    let quiet = |s: &Sample| !event_x.iter().any(|&(x0, x1)| s.t >= x0 && s.t < x1);
    let in_win = |a: f64, b: f64, f: &dyn Fn(&Sample) -> f32| quantile(steady.iter().filter(|s| s.playing && s.t >= a && s.t < b && quiet(s)).map(|s| f(s)).collect(), 0.5);
    let buf_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.buf);
    let buf_last = in_win(seconds - win, seconds + 1.0, &|s| s.buf);
    let off_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.buf - s.tgt);
    let off_last = in_win(seconds - win, seconds + 1.0, &|s| s.buf - s.tgt);
    let p95_jit = p.median_p95(SETTLE_S, SETTLE_S + win);

    PeerResult {
        desc: describe(np),
        frame_ms: np.frame as f32 / 48.0,
        buf_med,
        buf_p95,
        tgt_med,
        p95_jit,
        band_max_s,
        band_max_at,
        band_total_s,
        band_pct,
        conceal_pct,
        conceal_loss_pct,
        conceal_underrun_pct,
        conceal_all,
        conceal_limit,
        loss_pct,
        rule,
        known,
        recoveries,
        buf_first,
        buf_last,
        off_first,
        off_last,
        rebuffers: p.js.rebuffers,
        late_dropped: p.js.late_dropped,
        corrections: p.corrections(),
        max_corr_cents: p.js.max_corr_cents,
    }
}

/// 지연 누적 비교 창 길이(s).
fn accum_win(seconds: f64) -> f64 {
    ((seconds - SETTLE_S) / 6.0).clamp(30.0, 300.0)
}

fn pf(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

struct Report {
    md: String,
    json: Value,
    fails: Vec<String>,
}

/// 래칫 판정: 값 v, 한도 lim, 알려진 미달 기준값 base(± tol).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ratchet {
    Pass,
    /// 알려진 미달, 기준값 ± tol 안(테스트 통과).
    Known,
    /// 새 미달.
    New,
    /// 기준값 + tol 초과.
    Worse,
    /// 기준값 − tol 미만(기준값을 낮춰라).
    Improved,
    /// 한도 안으로 들어옴(지정을 지워라).
    Resolved,
}

fn ratchet(v: f64, lim: f64, base: Option<f64>, tol: f64) -> Ratchet {
    match (v <= lim, base) {
        (true, None) => Ratchet::Pass,
        (true, Some(_)) => Ratchet::Resolved,
        (false, None) => Ratchet::New,
        (false, Some(b)) if v > b + tol => Ratchet::Worse,
        (false, Some(b)) if v < b - tol => Ratchet::Improved,
        (false, Some(_)) => Ratchet::Known,
    }
}

/// 알려진 미달이 있을 수 있는 지표 판정. fails/known/resolved에 문구를 쌓는다.
fn judge_metric(i: usize, r: &PeerResult, m: Metric, seconds: f64, fails: &mut Vec<String>, known: &mut Vec<String>, resolved: &mut Vec<String>) -> Verdict {
    let (v, lim) = (r.value(m), r.limit(m));
    let k = r.known.iter().find(|k| k.metric == m && k.base(seconds).is_some());
    let base = k.and_then(|k| k.base(seconds));
    let tol = k.map_or(0.0, |k| k.tol);
    let name = m.name();
    match ratchet(v, lim, base, tol) {
        Ratchet::Pass => Verdict::Pass,
        Ratchet::Known => {
            known.push(format!("#{i} {name} {v:.2}% > {lim:.2}% — {} (기준값 {:.2}% ± {tol:.1}%p)", k.map_or("", |k| k.why), base.unwrap_or(f64::NAN)));
            Verdict::Known
        }
        Ratchet::New => {
            fails.push(format!("#{i} {name} {v:.2}% > {lim:.2}%"));
            Verdict::Fail
        }
        Ratchet::Worse => {
            fails.push(format!("#{i} {name} {v:.2}% — 알려진 미달이 기준값 {:.2}% + {tol:.1}%p를 넘어 악화", base.unwrap_or(f64::NAN)));
            Verdict::Fail
        }
        Ratchet::Improved | Ratchet::Resolved => {
            let msg = if v <= lim {
                format!("#{i} {name} {v:.2}% ≤ {lim:.2}% — 알려진 미달 해소, 시나리오의 known 지정({m:?})을 지워라")
            } else {
                format!("#{i} {name} {v:.2}% — 기준값 {:.2}%보다 {tol:.1}%p 넘게 개선, known 기준값({m:?})을 새 측정값으로 낮춰라(래칫)", base.unwrap_or(f64::NAN))
            };
            resolved.push(msg.clone());
            fails.push(msg);
            Verdict::Fail
        }
    }
}

fn build_report(tag: &str, seconds: f64, specs: &[Spec], out: &MultiOut, res: &[PeerResult], wall_s: f64) -> Report {
    // fails: 테스트 실패(새 미달·악화·래칫). known: 알려진 미달(기준값 안). resolved: 알려진 미달이 풀리거나 크게 개선(실패에도 포함).
    let (mut fails, mut known, mut resolved) = (Vec::new(), Vec::new(), Vec::new());
    let mut peer_verdict = Vec::new();
    let metrics = [Metric::Conceal, Metric::Underrun, Metric::BandPct];
    for (i, r) in res.iter().enumerate() {
        let mut vs: Vec<Verdict> = metrics.iter().map(|&m| judge_metric(i, r, m, seconds, &mut fails, &mut known, &mut resolved)).collect();
        if !r.band_ok() {
            fails.push(format!("#{i} ±{BAND_MS}ms 밖 {:.1}s 연속(@{:.0}s)", r.band_max_s, r.band_max_at));
            vs.push(Verdict::Fail);
        }
        if !r.recover_ok() {
            fails.push(format!("#{i} 단절 복구 {:?}", r.recoveries));
            vs.push(Verdict::Fail);
        }
        if !r.accum_ok() {
            fails.push(format!("#{i} 지연 누적 {:+.2}ms", r.accum()));
            vs.push(Verdict::Fail);
        }
        peer_verdict.push(if vs.contains(&Verdict::Fail) {
            "FAIL"
        } else if vs.contains(&Verdict::Known) {
            "FAIL(알려진)"
        } else {
            "PASS"
        });
    }
    if out.xruns > 0 {
        fails.push(format!("xrun {}회", out.xruns));
    }
    let worst = |f: &dyn Fn(&PeerResult) -> f64| res.iter().map(f).fold(f64::NEG_INFINITY, f64::max);
    let band_worst = worst(&|r| r.band_max_s);
    let band_pct_worst = worst(&|r| r.band_pct);
    let under_worst = worst(&|r| r.conceal_underrun_pct);
    let goal_conc: Vec<&PeerResult> = res.iter().filter(|r| r.rule == LossRule::Goal).collect();
    let conc_worst = goal_conc.iter().map(|r| r.conceal_pct).fold(0.0, f64::max);
    let other_ok = res.iter().filter(|r| r.rule == LossRule::ActualPlus).all(|r| r.ok(Metric::Conceal));
    let rec_all: Vec<Option<f64>> = res.iter().flat_map(|r| r.recoveries.iter().map(|x| x.1)).collect();
    let rec_worst = rec_all.iter().map(|r| r.unwrap_or(f64::INFINITY)).fold(0.0, f64::max);
    let accum_worst = worst(&|r| r.accum().abs() as f64);
    let overall = fails.is_empty();
    let goal_all = overall && known.is_empty();
    let all_ok = |m: Metric| res.iter().all(|r| r.ok(m));

    let mut md = String::new();
    let _ = writeln!(md, "# 장시간 시뮬레이션 리포트 — {tag}\n");
    let _ = writeln!(
        md,
        "가상 시간 {:.0}분, 6피어(이 엔진이 받는 원격 스트림 6개), 결정적(시드 고정), 실행 {:.0}초. 정착 {SETTLE_S:.0}초 뒤부터 측정.\n\n\
         - GOAL 전 항목: **{}**{}\n- 회귀 판정(테스트): **{}** — 실패 {}건(새 미달·악화·래칫), 알려진 미달 {}건(기준값 안)\n",
        seconds / 60.0,
        wall_s,
        pf(goal_all),
        if goal_all { String::new() } else { format!(" (미달 {}건)", fails.len() + known.len()) },
        pf(overall),
        fails.len(),
        known.len()
    );
    let _ = writeln!(md, "## GOAL 기준 판정\n");
    let _ = writeln!(md, "| 기준 (GOAL) | 목표 | 결과(최악 피어) | 판정 |\n|---|---|---|---|");
    let _ = writeln!(md, "| 1절 지연 누적: bufferMs가 목표 ±3ms 밖에 머문 최장 연속 시간(띠 안 복귀 ≤ {BAND_GAP_S:.0}s는 이어 셈) | < {BAND_MAX_S:.0}s | {band_worst:.1}s | {} |", pf(res.iter().all(|r| r.band_ok())));
    let _ = writeln!(md, "| 1절 지연 누적: ±3ms 밖 총 시간 비율 | ≤ {BAND_PCT_MAX:.0}% | {band_pct_worst:.1}% | {} |", pf(all_ok(Metric::BandPct)));
    let _ = writeln!(md, "| 1절 지연 누적: 처음/마지막 창 bufferMs 중앙값 차이(사건 구간 제외) | ≤ {ACCUM_MAX_MS:.1}ms | {accum_worst:.2}ms | {} |", pf(res.iter().all(|r| r.accum_ok())));
    let _ = writeln!(md, "| 2절 은닉 비율(무작위 손실 ≤ 1% 회선, 평시) | ≤ {CONCEAL_GOAL_PCT}% | {conc_worst:.2}% | {} |", pf(goal_conc.iter().all(|r| r.ok(Metric::Conceal))));
    let _ = writeln!(md, "| 2절 은닉 비율(버스트·2% 회선, 평시): 실제 손실 + 0.5%p 이내 | 피어별 | 아래 표 | {} |", pf(other_ok));
    let _ = writeln!(md, "| 2절 언더런 은닉 비율(평시, 전 회선): 은닉 한도 − 손실 몫 | ≤ {UNDERRUN_MAX_PCT}% | {under_worst:.2}% | {} |", pf(all_ok(Metric::Underrun)));
    let _ = writeln!(md, "| 2절 UDP 5초 단절 후 복구 시간 ({}회) | ≤ {RECOVER_MAX_S:.0}s | {} | {} |", rec_all.len(), if rec_worst.is_finite() { format!("{rec_worst:.1}s") } else { "복구 안 됨".into() }, pf(res.iter().all(|r| r.recover_ok())));
    let _ = writeln!(md, "| 콜백 xrun(시뮬 워커 기상 지연 기준) | 0 | {} | {} |", out.xruns, pf(out.xruns == 0));
    let _ = writeln!(md, "\n## 피어별\n");
    let _ = writeln!(md, "| # | 회선 | bufferMs 중앙값 / p95 | 목표 중앙값 | ±3ms 밖 최장 / 총 (비율) | 은닉 평시 (한도) = 손실 + 언더런 / 사건 포함 | 실제 손실 | 단절 복구 | 처음→마지막 창 bufferMs (목표 대비) | 판정 |\n|---|---|---|---|---|---|---|---|---|---|");
    for (i, r) in res.iter().enumerate() {
        let rec = if r.recoveries.is_empty() {
            "—".to_string()
        } else {
            r.recoveries.iter().map(|(_, x)| x.map_or("실패".into(), |s| format!("{s:.1}s"))).collect::<Vec<_>>().join(", ")
        };
        let _ = writeln!(
            md,
            "| {i} | {} | {:.2} / {:.2} | {:.2} | {:.1}s / {:.1}s ({:.1}%) | {:.2}% ({:.2}%) = {:.2} + {:.2} / {:.2}% | {:.2}% | {rec} | {:.2} → {:.2} ({:+.2} → {:+.2}) | {} |",
            r.desc,
            r.buf_med,
            r.buf_p95,
            r.tgt_med,
            r.band_max_s,
            r.band_total_s,
            r.band_pct,
            r.conceal_pct,
            r.conceal_limit,
            r.conceal_loss_pct,
            r.conceal_underrun_pct,
            r.conceal_all,
            r.loss_pct,
            r.buf_first,
            r.buf_last,
            r.off_first,
            r.off_last,
            peer_verdict[i]
        );
    }
    let _ = writeln!(md, "\n## 참고(판정 외)\n");
    let _ = writeln!(md, "| # | 첫 창 bufferMs vs 1F + p95 지터 (GOAL 1절 지터 버퍼 추가 지연) | 재버퍼 | 늦은 패킷 버림 | 샘플 보정 횟수 | 최대 보정 피치 |\n|---|---|---|---|---|---|");
    for (i, r) in res.iter().enumerate() {
        let _ = writeln!(
            md,
            "| {i} | {:.2} vs {:.2} ({}) | {} | {} | {} | {:.1}센트 |",
            r.buf_first,
            r.frame_ms + r.p95_jit,
            if r.goal_delay_ok() { "충족" } else { "초과" },
            r.rebuffers,
            r.late_dropped,
            r.corrections,
            r.max_corr_cents
        );
    }
    let _ = writeln!(
        md,
        "\n- 정의: \"목표\" = 엔진 `PeerStats.targetMs`(지터 추정 + 소비자 여유 + 삽입 문턱). 재생이 멈춘 표본(단절 중 재버퍼)은 ±3ms 밖으로 센다.\n\
         - ±3ms 판정: GOAL 문구(\"1분 이상 머무르지 않음\")를 (1) 띠 안 복귀 {BAND_GAP_S:.0}s 이하를 무시한 최장 연속 < {BAND_MAX_S:.0}s와 (2) 띠 밖 총 비율 ≤ {BAND_PCT_MAX:.0}%(60분 중 6분)로 본다. 경계를 오가는 피어가 연속 판정을 피해 가지 않게 하기 위함.\n\
         - 은닉 평시: 단절·스파이크 구간(끝 + 10초까지)과 계단 직후 10초를 뺀 구간. 계단 뒤 새 조건의 정상 상태는 평시로 센다.\n\
         - 은닉 = 손실 몫(마감에 그 패킷이 없고 뒤 패킷은 와 있어 seq를 넘긴 은닉 — 실제 손실 + 너무 늦게 온 패킷, 실제 Opus는 FEC/PLC) + 언더런 몫(마감에 뒤 패킷도 없음, seq 유지·깊이 +1F). 손실 몫 − 실제 손실 ≈ 늦은 패킷 몫. 언더런 예산 {UNDERRUN_MAX_PCT}% = GOAL 2절 한도 1.5% − 손실 1%.\n\
         - 복구 시간: 단절 끝(패킷 재개)부터 재생 중 + 목표 ±3ms 안이 1초 이어지기 시작한 시각까지.\n\
         - 지연 누적: 처음 창과 마지막 창(각 {:.0}s)에서 사건 구간을 뺀 bufferMs 중앙값 차이(같은 망 조건).\n\
         - 알려진 미달: 기준값 ± 허용폭 안이면 테스트 통과(리포트는 FAIL(알려진)). 악화·크게 개선·해소는 모두 테스트 실패 — 개선되면 기준값을 낮추거나 지정을 지운다(래칫).\n\
         - 모델 한계: 코덱은 PCM16(Opus PLC/FEC 미모델 — 은닉 프레임 수는 같지만 FEC 복원은 반영 안 됨), CPU·실장비 xrun은 측정 대상 아님(GOAL 2절 CPU ≤ 15%는 [사람] 확인).\n\
         - 복구 시간은 지터 버퍼 복구만 잰다(0.1s는 표본 간격 하한). UDP 경로 재수립(`net::peer` Reconnecting → keepalive·홀펀칭·NAT 매핑 만료)은 미모델 — 실네트워크 [사람].\n\
         - WS 재접속(30초 이내, 세션 재개)은 엔진을 유지하고 descriptor만 재공지하므로 엔진 입력이 없다 → 시뮬레이션 대상 아님.\n",
        accum_win(seconds)
    );
    for (title, list) in [("테스트 실패(새 미달·악화·래칫)", &fails), ("알려진 미달(백로그, 기준값 안)", &known), ("알려진 미달 해소·개선(기준값 갱신 필요)", &resolved)] {
        if !list.is_empty() {
            let _ = writeln!(md, "\n## {title}\n");
            for f in list {
                let _ = writeln!(md, "- {f}");
            }
        }
    }

    let peers: Vec<Value> = res
        .iter()
        .zip(specs)
        .enumerate()
        .map(|(i, (r, s))| {
            json!({
                "index": i,
                "line": r.desc,
                "frameMs": r.frame_ms,
                "driftPpm": s.np.drift_ppm,
                "bufferMs": { "median": r.buf_med, "p95": r.buf_p95 },
                "targetMsMedian": r.tgt_med,
                "outOfBand": { "maxRunS": r.band_max_s, "maxRunAtS": r.band_max_at, "totalS": r.band_total_s, "pct": r.band_pct, "pass": r.band_ok() && r.ok(Metric::BandPct) },
                "conceal": {
                    "pct": r.conceal_pct, "lossPct": r.conceal_loss_pct, "underrunPct": r.conceal_underrun_pct,
                    "pctWithEvents": r.conceal_all, "limitPct": r.conceal_limit, "underrunLimitPct": UNDERRUN_MAX_PCT,
                    "actualLossPct": r.loss_pct, "pass": r.ok(Metric::Conceal) && r.ok(Metric::Underrun),
                },
                "recoveries": r.recoveries.iter().map(|(e, x)| json!({ "outageEndS": e, "recoverS": x })).collect::<Vec<_>>(),
                "recoverPass": r.recover_ok(),
                "accumulation": { "firstWindowBufferMs": r.buf_first, "lastWindowBufferMs": r.buf_last, "deltaMs": r.accum(), "offsetFirstMs": r.off_first, "offsetLastMs": r.off_last, "pass": r.accum_ok() },
                "goalDelay": { "bufferMs": r.buf_first, "oneFramePlusP95Ms": r.frame_ms + r.p95_jit, "within": r.goal_delay_ok() },
                "known": r.known.iter().filter(|k| k.base(seconds).is_some()).map(|k| json!({ "metric": format!("{:?}", k.metric), "base": k.base(seconds), "tol": k.tol, "why": k.why })).collect::<Vec<_>>(),
                "verdict": peer_verdict[i],
                "rebuffers": r.rebuffers,
                "lateDropped": r.late_dropped,
                "corrections": r.corrections,
                "maxCorrCents": r.max_corr_cents,
            })
        })
        .collect();
    let json = json!({
        "tag": tag,
        "simSeconds": seconds,
        "wallSeconds": wall_s,
        "pass": overall,
        "goalAllPass": goal_all,
        "xruns": out.xruns,
        "criteria": {
            "bandMs": BAND_MS, "bandMaxS": BAND_MAX_S, "bandGapS": BAND_GAP_S, "bandPctMax": BAND_PCT_MAX,
            "concealGoalPct": CONCEAL_GOAL_PCT, "underrunMaxPct": UNDERRUN_MAX_PCT,
            "recoverMaxS": RECOVER_MAX_S, "accumMaxMs": ACCUM_MAX_MS,
        },
        "fails": fails,
        "knownShortfalls": known,
        "resolved": resolved,
        "peers": peers,
    });
    Report { md, json, fails }
}

fn run_and_report(tag: &str, seconds: f64) {
    let specs = scenario(seconds);
    let mut sim = MultiSim::new(seconds, specs.iter().map(|s| s.np.clone()).collect());
    sim.settle_us = (SETTLE_S * 1e6) as u64;
    sim.seed = 6;
    let t0 = std::time::Instant::now();
    let out = run_multi(&sim);
    let wall_s = t0.elapsed().as_secs_f64();
    let res: Vec<PeerResult> = specs.iter().zip(&out.peers).map(|(s, p)| analyze(&s.np, s.rule, s.known, p, seconds)).collect();
    let rep = build_report(tag, seconds, &specs, &out, &res, wall_s);
    let dir = std::env::var_os("SIM_REPORT_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("band-tauri-sim"));
    std::fs::create_dir_all(&dir).unwrap();
    let md_path = dir.join(format!("sim-report-{tag}.md"));
    std::fs::write(&md_path, &rep.md).unwrap();
    std::fs::write(dir.join(format!("sim-report-{tag}.json")), serde_json::to_string_pretty(&rep.json).unwrap()).unwrap();
    println!("{}\n리포트: {}", rep.md, md_path.display());
    assert!(rep.fails.is_empty(), "GOAL 미달 {}건: {:?} (리포트 {})", rep.fails.len(), rep.fails, md_path.display());
}

/// 6피어 5분(가상): 기본 `cargo test`에서 도는 짧은 버전. 60분 버전과 같은 시나리오 모양(사건 위치는 길이 비율).
#[test]
fn long_sim_6peers_5min() {
    run_and_report("short-5min", 300.0);
}

/// 6피어 60분(가상, 디버그 빌드 수 분). CI 야간 잡(`long-sim`)과 수동 실행용.
#[test]
#[ignore = "long: 6피어 60분 가상 시간(디버그 로컬 약 8분) — 야간 CI long-sim 잡에서 실행"]
fn long_sim_6peers_60min() {
    run_and_report("long-60min", 3600.0);
}

/// 래칫: 알려진 미달은 기준값 ± tol 안에서만 통과하고, 악화·크게 개선·해소는 모두 실패로 분류된다.
#[test]
fn long_sim_ratchet() {
    assert_eq!(ratchet(1.0, 1.5, None, 0.3), Ratchet::Pass);
    assert_eq!(ratchet(2.0, 1.5, None, 0.3), Ratchet::New);
    assert_eq!(ratchet(2.1, 1.5, Some(2.0), 0.3), Ratchet::Known);
    assert_eq!(ratchet(2.4, 1.5, Some(2.0), 0.3), Ratchet::Worse);
    assert_eq!(ratchet(1.6, 1.5, Some(2.0), 0.3), Ratchet::Improved);
    assert_eq!(ratchet(1.4, 1.5, Some(2.0), 0.3), Ratchet::Resolved);
}

/// 하네스 자체 검증: 버스트 손실 모델의 실제 손실률과 단절 패킷 집계가 설정과 맞는지(짧게).
#[test]
fn long_sim_harness_models() {
    let mut np = NetPeer::new(240, 9);
    np.burst = Some((0.01, 0.5));
    np.outages = vec![(5 * SEC, 2 * SEC)];
    let out = run_multi(&MultiSim::new(20.0, vec![np]));
    let p = &out.peers[0];
    // 나쁨 상태 비율 = 0.01 / (0.01 + 0.5) ≈ 1.96%
    let loss = p.tx_lost as f64 / (p.tx_sent - p.tx_outage) as f64;
    assert!((0.012..0.03).contains(&loss), "버스트 손실률 {loss}");
    // 2초 × 200패킷/s
    assert!((390..=410).contains(&p.tx_outage), "단절 패킷 {}", p.tx_outage);
    // 단절 뒤 다시 재생한다.
    assert!(p.samples.iter().any(|s| s.t > 10.0 && s.playing));
    assert!(p.samples.iter().any(|s| (5.5..7.0).contains(&s.t) && !s.playing), "단절 중 재버퍼(재생 멈춤)");
}
