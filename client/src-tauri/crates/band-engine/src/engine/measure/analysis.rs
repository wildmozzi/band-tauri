//! 측정 신호 처리·판정(순수 계산): 처프·FFT 상호상관·피크 검출, 루프백·에코 분석, 판정과 메시지. 측정 스레드도 워커도
//! 모른다 — `loopback`·`echo`가 이것을 쓴다(한 방향).

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::Serialize;

use band_dsp::config::INTERNAL_RATE;

// ============================================================================
// 신호 처리
// ============================================================================

/// 선형 처프(양끝 Hann 페이드). `amp`는 최대 진폭.
pub fn chirp(rate: u32, dur_s: f64, f0: f64, f1: f64, amp: f32, fade_s: f64) -> Vec<f32> {
    let r = rate.max(1) as f64;
    let n = (dur_s * r).round().max(2.0) as usize;
    let fade = ((fade_s * r).round() as usize).min(n / 2);
    let k = (f1 - f0) / dur_s;
    (0..n)
        .map(|i| {
            let t = i as f64 / r;
            let ph = 2.0 * std::f64::consts::PI * (f0 * t + 0.5 * k * t * t);
            let w = if i < fade {
                0.5 - 0.5 * (std::f64::consts::PI * i as f64 / fade as f64).cos()
            } else if i >= n - fade {
                0.5 - 0.5 * (std::f64::consts::PI * (n - 1 - i) as f64 / fade as f64).cos()
            } else {
                1.0
            };
            (ph.sin() * w) as f32 * amp
        })
        .collect()
}

/// 상호상관 `c[l] = Σ x[n + l]·t[n]`, l = 0..=x.len() − t.len() (FFT). `x`가 `t`보다 짧으면 빈 벡터.
pub fn xcorr(x: &[f32], t: &[f32]) -> Vec<f32> {
    if t.is_empty() || x.len() < t.len() {
        return Vec::new();
    }
    let n = (x.len() + t.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut a: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    a.resize(n, Complex::new(0.0, 0.0));
    let mut b: Vec<Complex<f32>> = t.iter().map(|&v| Complex::new(v, 0.0)).collect();
    b.resize(n, Complex::new(0.0, 0.0));
    fwd.process(&mut a);
    fwd.process(&mut b);
    for (p, q) in a.iter_mut().zip(b.iter()) {
        *p *= q.conj();
    }
    inv.process(&mut a);
    let scale = 1.0 / n as f32;
    a[..=x.len() - t.len()].iter().map(|c| c.re * scale).collect()
}

/// 상관 피크 하나.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// `x`에서 템플릿 시작이 맞는 위치(샘플, 포물선 보간).
    pub lag: f64,
    /// 피크 |c| / 피크 주변을 뺀 상관 RMS. 무신호·잡음이면 작다.
    pub ratio: f32,
    /// 정규화 상관계수 |c| / (‖t‖·‖x 구간‖). 0..1.
    pub coeff: f32,
    /// 극성이 뒤집혀 도착했다(케이블/장치 위상 반전).
    pub inverted: bool,
}

/// 루프백 판정 문턱: 피크/잡음 비, 상관계수.
pub const MIN_RATIO: f32 = 10.0;
pub const MIN_COEFF: f32 = 0.2;

/// `c`(`xcorr` 결과)의 `[lo, hi)`에서 가장 큰 |c|를 찾는다. 잡음 RMS는 같은 범위에서 피크 앞 `guard_before`,
/// 뒤 `guard_after` 샘플을 뺀 나머지로 잰다(잔향·부엽 제외).
pub fn detect_in(c: &[f32], x: &[f32], t: &[f32], lo: usize, hi: usize, guard_before: usize, guard_after: usize) -> Option<Detection> {
    let hi = hi.min(c.len());
    if lo >= hi {
        return None;
    }
    let (k, peak) = c[lo..hi]
        .iter()
        .enumerate()
        .map(|(i, v)| (lo + i, *v))
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    let mag = peak.abs();
    // 포물선 보간(|c| 기준)
    let mut lag = k as f64;
    if k > 0 && k + 1 < c.len() {
        let (y0, y1, y2) = (c[k - 1].abs() as f64, mag as f64, c[k + 1].abs() as f64);
        let den = y0 - 2.0 * y1 + y2;
        if den.abs() > 1e-12 {
            lag += (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5);
        }
    }
    let (ex_lo, ex_hi) = (k.saturating_sub(guard_before), k + guard_after);
    let (mut sum, mut cnt) = (0.0f64, 0usize);
    for (i, v) in c[lo..hi].iter().enumerate() {
        let i = lo + i;
        if i < ex_lo || i > ex_hi {
            sum += (*v as f64) * (*v as f64);
            cnt += 1;
        }
    }
    let noise = if cnt > 0 { (sum / cnt as f64).sqrt() as f32 } else { 0.0 };
    let ratio = if noise > 0.0 {
        mag / noise
    } else if mag > 1e-9 {
        f32::INFINITY
    } else {
        0.0
    };
    let tn: f32 = t.iter().map(|v| v * v).sum::<f32>().sqrt();
    let xn: f32 = x.get(k..(k + t.len()).min(x.len())).map_or(0.0, |s| s.iter().map(|v| v * v).sum::<f32>().sqrt());
    let coeff = if tn > 0.0 && xn > 0.0 { (mag / (tn * xn)).min(1.0) } else { 0.0 };
    Some(Detection { lag, ratio, coeff, inverted: peak < 0.0 })
}

/// (중앙값, 최소, 최대, 표준편차). 빈 입력이면 None.
pub fn summarize(v: &[f64]) -> Option<(f64, f64, f64, f64)> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    let median = median_sorted(&s);
    let mean = s.iter().sum::<f64>() / n as f64;
    let var = s.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    Some((median, s[0], s[n - 1], var.sqrt()))
}

/// 측정 판정.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MeasureVerdict {
    /// 모든(과반) 반복에서 검출, 흩어짐 ≤ 1ms.
    Ok,
    /// 신호 미검출(케이블 미연결, 입력 채널 오선택, 음소거, 게인 부족, 상대 미응답).
    NoSignal,
    /// 입력이 클리핑됐다(게인/출력 볼륨 과다). 값은 참고용.
    Clipped,
    /// 검출이 과반이 안 되거나 반복 간 차이가 크다.
    Unstable,
}

/// 루프백: 반복 간 차이(최대 − 최소)가 이보다 크면 불안정.
pub const UNSTABLE_SPREAD_MS: f64 = 1.0;
/// 에코: 클릭마다 입력 버스트 위상(≤ 입력 버퍼)과 지터 버퍼 보정(데드밴드 ≤ 프레임/2)이 달라 깨끗한 회선에서도
/// 2~3ms 흩어진다. 이보다 크면 불안정. 흩어짐은 단일 이상치(`single_outlier`)를 뺀 값으로 잰다.
pub const ECHO_UNSTABLE_SPREAD_MS: f64 = 5.0;
/// 에코 이상치 제외: 검출이 이만큼 이상일 때만 1개를 뺄 수 있다(뺀 뒤에도 4개 이상이 남아 중앙값·흩어짐이 의미 있게).
pub const ECHO_OUTLIER_MIN_RUNS: usize = 5;

/// 정렬된 비어 있지 않은 슬라이스의 중앙값.
fn median_sorted(s: &[f64]) -> f64 {
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// 단일 이상치 찾기(에코, 상하위 1개 제외). 검출값 `v`가 `ECHO_OUTLIER_MIN_RUNS`개 이상이고 전체 흩어짐(최대 −
/// 최소)이 `ECHO_UNSTABLE_SPREAD_MS`를 넘을 때, 최솟값과 최댓값 중 빼면 흩어짐이 더 작아지는 쪽 하나를 빼 본다.
/// 남은 값의 흩어짐이 상한 안이면 그 인덱스(`v` 기준)를 돌려준다. 그래서
/// - 깨끗한 측정(흩어짐 ≤ 상한)에서는 아무것도 빼지 않는다(판정을 바꾸지 않는 제외는 하지 않음).
/// - 많아야 1개만 뺀다. 둘 이상 튀거나 전체가 넓게 흩어지면 하나를 빼도 상한을 넘으므로 그대로 불안정이다.
///
/// MAD(3σ) 기준은 표본 6~8개에서 너무 엄격했다: 실시간 측정에서 나머지 흩어짐 2.9ms·한 값만 2.75ms 낮게 튄
/// `[65.05, 59.39, 63.96, 62.14, 64.66, 62.23]`이 3·1.4826·MAD = 4.85ms 기준을 못 넘어 불안정으로 남았다.
pub fn single_outlier(v: &[f64]) -> Option<usize> {
    if v.len() < ECHO_OUTLIER_MIN_RUNS {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if s[n - 1] - s[0] <= ECHO_UNSTABLE_SPREAD_MS {
        return None;
    }
    // 최솟값을 빼면 흩어짐 = s[n-1] − s[1], 최댓값을 빼면 s[n-2] − s[0].
    let (drop_low, rest_spread) = if s[n - 1] - s[1] <= s[n - 2] - s[0] { (true, s[n - 1] - s[1]) } else { (false, s[n - 2] - s[0]) };
    if rest_spread > ECHO_UNSTABLE_SPREAD_MS {
        return None;
    }
    let target = if drop_low { s[0] } else { s[n - 1] };
    v.iter().position(|&x| x == target)
}

/// 에코 클릭 결과 요약(판정 포함, 상대 보고값 검사 전).
#[derive(Debug, Clone, PartialEq)]
pub struct EchoSummary {
    pub verdict: MeasureVerdict,
    /// 보낸 클릭 수.
    pub runs: usize,
    /// 검출된 클릭 수.
    pub detected: usize,
    /// 통계·판정에 쓴 클릭 수(= 검출 − 제외).
    pub used: usize,
    /// 이상치로 뺀 클릭의 인덱스(클릭 순서 기준).
    pub outlier: Option<usize>,
    /// 쓴 값의 (중앙값, 최소, 최대, 표준편차).
    pub stats: Option<(f64, f64, f64, f64)>,
    /// 판정에 쓴 흩어짐(쓴 값의 최대 − 최소).
    pub spread: Option<f64>,
}

/// 클릭별 왕복(None = 미검출)을 요약·판정한다. 단일 이상치(`single_outlier`)는 빼고 본다. 과반 조건도 뺀 뒤의
/// 개수(`used`)로 본다 — 이상치는 유효한 측정이 아니다.
pub fn echo_summary(per: &[Option<f64>]) -> EchoSummary {
    let idx: Vec<usize> = per.iter().enumerate().filter_map(|(i, v)| v.map(|_| i)).collect();
    let vals: Vec<f64> = per.iter().flatten().copied().collect();
    let out = single_outlier(&vals);
    let kept: Vec<f64> = vals.iter().enumerate().filter(|&(i, _)| Some(i) != out).map(|(_, &x)| x).collect();
    let stats = summarize(&kept);
    let spread = stats.map(|(_, lo, hi, _)| hi - lo);
    EchoSummary {
        verdict: verdict_with(per.len(), kept.len(), false, spread, ECHO_UNSTABLE_SPREAD_MS),
        runs: per.len(),
        detected: vals.len(),
        used: kept.len(),
        outlier: out.map(|k| idx[k]),
        stats,
        spread,
    }
}

/// 에코 판정 메시지(UI에 그대로 보인다). `Unstable`은 원인(검출 부족 / 흩어짐)을 나눠 다시 재라고 안내한다.
pub fn echo_message(s: &EchoSummary, one_way: Option<f64>, peer_bogus_delay_us: Option<u32>) -> String {
    let excl = if s.outlier.is_some() { ", 튄 값 1개 제외" } else { "" };
    if let Some(d) = peer_bogus_delay_us {
        return format!(
            "상대가 보고한 처리 지연({:.1}ms)이 비정상이라 {:.0}ms로 잘랐습니다. 상대 앱 버전을 확인하세요",
            d as f64 / 1000.0,
            MAX_PEER_ECHO_DELAY_US as f64 / 1000.0
        );
    }
    match s.verdict {
        // 에코는 클리핑 판정을 하지 않는다(verdict_with에 clipped=false).
        MeasureVerdict::Ok | MeasureVerdict::Clipped => format!(
            "네트워크 포함 왕복 {:.1}ms, 편도 추정 {:.1}ms(클릭 {}/{}개{excl})",
            s.stats.map_or(0.0, |t| t.0),
            one_way.unwrap_or(0.0),
            s.detected,
            s.runs
        ),
        MeasureVerdict::NoSignal => "돌아온 클릭을 찾지 못했습니다. 상대 연결 상태(손실)를 확인하고 다시 재세요".into(),
        MeasureVerdict::Unstable if s.used * 2 <= s.runs => format!(
            "결과가 불안정합니다: 돌아온 클릭이 절반 이하입니다(검출 {}/{}개{excl}). 손실이 많을 수 있습니다. 연결 상태를 확인하고 다시 재세요",
            s.detected, s.runs
        ),
        MeasureVerdict::Unstable => format!(
            "결과가 불안정합니다: 클릭 간 흩어짐 {:.1}ms > {:.0}ms(검출 {}/{}개{excl}). 회선 지터가 큽니다. 잠시 뒤 다시 재고, 계속되면 유선 연결로 바꾸거나 다른 네트워크 사용을 줄이세요",
            s.spread.unwrap_or(0.0),
            ECHO_UNSTABLE_SPREAD_MS,
            s.detected,
            s.runs
        ),
    }
}

pub(super) fn verdict_of(runs: usize, detected: usize, clipped: bool, spread: Option<f64>) -> MeasureVerdict {
    verdict_with(runs, detected, clipped, spread, UNSTABLE_SPREAD_MS)
}

fn verdict_with(runs: usize, detected: usize, clipped: bool, spread: Option<f64>, max_spread: f64) -> MeasureVerdict {
    if detected == 0 {
        if clipped {
            MeasureVerdict::Clipped
        } else {
            MeasureVerdict::NoSignal
        }
    } else if clipped {
        MeasureVerdict::Clipped
    } else if detected * 2 <= runs || spread.is_some_and(|s| s > max_spread) {
        MeasureVerdict::Unstable
    } else {
        MeasureVerdict::Ok
    }
}

pub(super) const CANCELLED: &str = "측정이 취소되었습니다";

/// 이보다 긴 왕복은 찾지 않는다(장치 안전 여유 포함 넉넉히).
pub const MAX_LOOPBACK_MS: f64 = 400.0;
/// 이 진폭 이상이면 클리핑으로 본다.
pub const CLIP_LEVEL: f32 = 0.99;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopbackRun {
    /// 검출되면 왕복 지연(ms).
    pub round_trip_ms: Option<f64>,
    pub peak_ratio: f32,
    pub correlation: f32,
    pub clipped: bool,
    pub inverted: bool,
    /// 녹음 최대 진폭(0..1).
    pub input_peak: f32,
}

pub(super) fn ms(frames: u32, rate: u32) -> f64 {
    frames as f64 * 1000.0 / rate.max(1) as f64
}

/// 녹음 한 번 분석. `cap[k]`는 입력 프레임 `cap_start + k`다.
#[allow(clippy::too_many_arguments)]
pub fn analyze_loopback(
    cap: &[f32],
    stim: &[f32],
    rate: f64,
    cap_start: u64,
    stim_start: u64,
    e_in: f64,
    e_out: f64,
    b_in: f64,
    b_out: f64,
) -> LoopbackRun {
    let input_peak = cap.iter().fold(0f32, |m, v| m.max(v.abs()));
    let clipped = input_peak >= CLIP_LEVEL;
    let c = xcorr(cap, stim);
    let det = detect_in(&c, cap, stim, 0, c.len(), (0.002 * rate) as usize, stim.len());
    let mut run = LoopbackRun {
        round_trip_ms: None,
        peak_ratio: det.map_or(0.0, |d| d.ratio.min(1e6)),
        correlation: det.map_or(0.0, |d| d.coeff),
        clipped,
        inverted: det.is_some_and(|d| d.inverted),
        input_peak,
    };
    let Some(d) = det.filter(|d| d.ratio >= MIN_RATIO && d.coeff >= MIN_COEFF) else {
        return run;
    };
    if !e_in.is_finite() || !e_out.is_finite() {
        return run;
    }
    let i = cap_start as f64 + d.lag;
    let capture_t = e_in + i / rate;
    let write_t = e_out + (stim_start as f64 + b_out) / rate;
    let rtl = (capture_t - write_t + b_in / rate) * 1000.0;
    if rtl > 0.0 && rtl < MAX_LOOPBACK_MS + 100.0 {
        run.round_trip_ms = Some(rtl);
    }
    run
}

/// 클릭 템플릿(48kHz): 5ms 처프 500Hz → 9kHz, Hann 페이드 1ms, 진폭 0.5. 코덱(Opus)을 지나도 상관 피크가 날카롭다.
pub fn click_template() -> Vec<f32> {
    chirp(INTERNAL_RATE, 0.005, 500.0, 9_000.0, 0.5, 0.001)
}

/// 이보다 긴 에코 왕복은 찾지 않는다(클릭 간격보다 짧아야 클릭이 섞이지 않는다).
pub const MAX_ECHO_MS: f64 = 280.0;
/// 상대가 보고하는 되돌리기 처리 지연 상한(µs). 넘으면 잘라 쓰고 불안정으로 표시한다.
pub const MAX_PEER_ECHO_DELAY_US: u32 = 50_000;
/// 에코 판정 문턱(코덱을 지나므로 루프백보다 낮게).
pub const ECHO_MIN_RATIO: f32 = 8.0;
pub const ECHO_MIN_COEFF: f32 = 0.3;
/// 돌아온 클릭 검출: 송신 시각마다 `[송신 + 1ms, 송신 + MAX_ECHO_MS]` 창에서 상관 피크를 찾는다.
/// 반환: 클릭마다 왕복(ms), 못 찾으면 None.
pub fn analyze_echo(emitted_us: &[f64], arrivals: &[f32], arrival_us: &[f64]) -> Vec<Option<f64>> {
    let tmpl = click_template();
    let c = xcorr(arrivals, &tmpl);
    let step = 1e6 / INTERNAL_RATE as f64;
    let guard = (0.002 * INTERNAL_RATE as f64) as usize;
    emitted_us
        .iter()
        .map(|&e| {
            let lo = arrival_us.partition_point(|&t| t < e + 1000.0);
            let hi = arrival_us.partition_point(|&t| t < e + MAX_ECHO_MS * 1000.0);
            let d = detect_in(&c, arrivals, &tmpl, lo, hi, guard, guard)?;
            if d.ratio < ECHO_MIN_RATIO || d.coeff < ECHO_MIN_COEFF {
                return None;
            }
            let k = d.lag.floor().max(0.0) as usize;
            let t = arrival_us.get(k)? + (d.lag - k as f64) * step;
            Some((t - e) / 1000.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xcorr_finds_known_offset_with_subsample_precision() {
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let mut x = vec![0f32; 20_000];
        for (i, v) in stim.iter().enumerate() {
            x[1234 + i] = *v * 0.5;
        }
        let c = xcorr(&x, &stim);
        let d = detect_in(&c, &x, &stim, 0, c.len(), 96, stim.len()).unwrap();
        assert!((d.lag - 1234.0).abs() < 0.05, "{d:?}");
        assert!(d.ratio > 100.0 && d.coeff > 0.99 && !d.inverted, "{d:?}");
        // 극성 반전
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        let c = xcorr(&neg, &stim);
        let d = detect_in(&c, &neg, &stim, 0, c.len(), 96, stim.len()).unwrap();
        assert!(d.inverted && (d.lag - 1234.0).abs() < 0.05);
    }

    #[test]
    fn silence_and_noise_are_not_detected() {
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let zeros = vec![0f32; 30_000];
        let run = analyze_loopback(&zeros, &stim, 48_000.0, 0, 0, 0.0, 0.0, 128.0, 128.0);
        assert!(run.round_trip_ms.is_none() && !run.clipped, "{run:?}");
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let noise: Vec<f32> = (0..30_000).map(|_| rng.gen_range(-0.3f32..0.3)).collect();
        let run = analyze_loopback(&noise, &stim, 48_000.0, 0, 0, 0.0, 0.0, 128.0, 128.0);
        assert!(run.round_trip_ms.is_none(), "잡음만으로 검출하면 안 됨: {run:?}");
        assert!(run.peak_ratio < MIN_RATIO || run.correlation < MIN_COEFF);
    }

    #[test]
    fn loopback_math_matches_time_model() {
        // 입력·출력 원점이 같고(e_in = e_out + B_out/r 관계를 직접 만든다) 자극을 7.5ms 뒤 캡처했다.
        let r = 48_000.0;
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let mut cap = vec![0f32; 30_000];
        // 쓰기 시각(J=1000) = e_out + (1000 + 128)/r. 캡처 위치 I = cap_start(500) + k.
        // e_in = 0, e_out = 0 → write = 1128/r. 캡처 시각 = (500 + k)/r = write + 7.5ms → k = 1128 + 360 − 500
        let k = 1128 + 360 - 500;
        for (i, v) in stim.iter().enumerate() {
            cap[k + i] = *v;
        }
        let run = analyze_loopback(&cap, &stim, r, 500, 1000, 0.0, 0.0, 128.0, 128.0);
        let rt = run.round_trip_ms.unwrap();
        assert!((rt - (7.5 + 128.0 / 48.0)).abs() < 0.01, "{rt}");
        // 클리핑 판정
        let clipped: Vec<f32> = cap.iter().map(|v| (v * 4.0).clamp(-1.0, 1.0)).collect();
        let run = analyze_loopback(&clipped, &stim, r, 500, 1000, 0.0, 0.0, 128.0, 128.0);
        assert!(run.clipped);
    }

    /// 잡음 섞인 루프백 녹음(SNR 약 15dB)에서도 계산된 왕복이 ±0.1ms 안이다(실시간 테스트의 결정적 짝).
    #[test]
    fn noisy_loopback_detects_within_tolerance() {
        use rand::{Rng, SeedableRng};
        let r = 48_000.0;
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let mut cap: Vec<f32> = (0..30_000).map(|_| rng.gen_range(-0.05f32..0.05)).collect();
        let k = 1128 + 360 - 500;
        for (i, v) in stim.iter().enumerate() {
            cap[k + i] += *v;
        }
        let run = analyze_loopback(&cap, &stim, r, 500, 1000, 0.0, 0.0, 128.0, 128.0);
        let rt = run.round_trip_ms.expect("검출");
        assert!((rt - (7.5 + 128.0 / 48.0)).abs() < 0.1, "{rt}");
        assert!(!run.clipped && run.peak_ratio >= MIN_RATIO);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict_of(5, 0, false, None), MeasureVerdict::NoSignal);
        assert_eq!(verdict_of(5, 0, true, None), MeasureVerdict::Clipped);
        assert_eq!(verdict_of(5, 5, false, Some(0.2)), MeasureVerdict::Ok);
        assert_eq!(verdict_of(5, 5, false, Some(1.5)), MeasureVerdict::Unstable);
        assert_eq!(verdict_of(6, 3, false, Some(0.1)), MeasureVerdict::Unstable);
        assert_eq!(summarize(&[3.0, 1.0, 2.0]).unwrap().0, 2.0);
        assert_eq!(summarize(&[1.0, 2.0, 3.0, 4.0]).unwrap().0, 2.5);
    }

    fn some(v: &[f64]) -> Vec<Option<f64>> {
        v.iter().map(|&x| Some(x)).collect()
    }

    /// 한 회차만 튄 측정(PR #71 CI: 6회 중 1회 ~4ms 일찍, 나머지 흩어짐 1~3ms)은 그 1개를 빼고 정상이다.
    #[test]
    fn echo_single_outlier_is_excluded() {
        // 호스티드 CI 실패 모양: 나머지 흩어짐 3ms, 한 회차 4ms 이른 값 → 전체 흩어짐 7ms(예전 판정 Unstable)
        let per = some(&[30.0, 30.6, 31.2, 26.0, 31.8, 33.0]);
        let s = echo_summary(&per);
        assert_eq!(s.verdict, MeasureVerdict::Ok, "{s:?}");
        assert_eq!((s.detected, s.used, s.outlier), (6, 5, Some(3)));
        assert!((s.spread.unwrap() - 3.0).abs() < 1e-9, "{s:?}");
        assert!((s.stats.unwrap().0 - 31.2).abs() < 1e-9, "중앙값은 남은 값 기준: {s:?}");
        // 늦게 튄 값(위쪽), 미검출이 섞여도 인덱스는 클릭 순서 기준
        let mut per = some(&[40.0, 41.0, 40.5, 41.5, 40.2, 52.0, 40.8]);
        per.insert(1, None);
        let s = echo_summary(&per);
        assert_eq!(s.verdict, MeasureVerdict::Ok, "{s:?}");
        assert_eq!((s.runs, s.detected, s.used, s.outlier), (8, 7, 6, Some(6)));
        assert!(per[6] == Some(52.0));
        let m = echo_message(&s, Some(20.0), None);
        assert!(m.contains("클릭 7/8개, 튄 값 1개 제외"), "{m}");
        // 실시간 측정(P1-13, 편도 25ms 프록시)에서 그대로 나온 모양: 나머지 2.9ms, 한 값만 2.75ms 낮음 → 전체 5.66ms
        let s = echo_summary(&some(&[65.05, 59.39, 63.96, 62.14, 64.66, 62.23]));
        assert_eq!((s.verdict, s.outlier), (MeasureVerdict::Ok, Some(1)), "{s:?}");
        assert!((s.spread.unwrap() - 2.91).abs() < 1e-9, "{s:?}");
    }

    /// 실제로 흔들리는 회선(분산 큼, 또는 2개 이상 튐)은 여전히 불안정이고, 안내 문구가 다시 재라고 말한다.
    #[test]
    fn echo_real_instability_stays_unstable() {
        // 넓게 고른 분산: 하나를 빼도 흩어짐 > 5ms
        let s = echo_summary(&some(&[15.0, 18.0, 21.0, 24.0, 27.0, 20.0, 23.0, 17.0]));
        assert_eq!(s.verdict, MeasureVerdict::Unstable, "{s:?}");
        assert_eq!(s.outlier, None, "고른 분산에서 이상치를 고르면 안 됨: {s:?}");
        let m = echo_message(&s, None, None);
        assert!(m.contains("흩어짐") && m.contains("다시 재"), "{m}");
        // 두 회차가 양쪽으로 튐: 하나만 빼므로 남은 흩어짐이 크다
        let s = echo_summary(&some(&[20.0, 20.5, 21.0, 21.2, 20.8, 21.4, 14.0, 28.0]));
        assert_eq!(s.verdict, MeasureVerdict::Unstable, "{s:?}");
        assert!(s.spread.unwrap() > ECHO_UNSTABLE_SPREAD_MS, "{s:?}");
        // 같은 쪽으로 두 회차가 튐(이봉): 이상치 하나로 볼 수 없다
        let s = echo_summary(&some(&[20.0, 20.5, 21.0, 20.2, 20.8, 27.0, 27.5]));
        assert_eq!(s.verdict, MeasureVerdict::Unstable, "{s:?}");
        // 검출이 적으면(5개 미만) 빼지 않는다
        let s = echo_summary(&[Some(20.0), Some(21.0), Some(20.5), Some(30.0), None, None]);
        assert_eq!((s.outlier, s.verdict), (None, MeasureVerdict::Unstable), "{s:?}");
        // 과반 조건은 뺀 뒤의 개수로: 8개 중 5개 검출, 1개 튐 → 사용 4개 ≤ 절반
        let s = echo_summary(&[Some(20.0), Some(20.4), Some(20.8), Some(21.0), Some(35.0), None, None, None]);
        assert_eq!((s.used, s.verdict), (4, MeasureVerdict::Unstable), "{s:?}");
        let m = echo_message(&s, None, None);
        assert!(m.contains("절반 이하") && m.contains("다시 재"), "{m}");
    }

    /// 깨끗한 측정은 아무것도 빼지 않는다(전체 흩어짐이 상한 안이면 다른 값과 2ms 떨어진 값도 그대로 둔다).
    #[test]
    fn echo_clean_run_excludes_nothing() {
        let s = echo_summary(&some(&[30.0, 30.1, 30.05, 30.0, 30.1, 32.0, 30.05, 30.0]));
        assert_eq!((s.outlier, s.used, s.verdict), (None, 8, MeasureVerdict::Ok), "{s:?}");
        assert!(!echo_message(&s, Some(15.0), None).contains("제외"));
        let s = echo_summary(&[None; 8]);
        assert_eq!((s.verdict, s.used, s.stats), (MeasureVerdict::NoSignal, 0, None));
        assert!(single_outlier(&[]).is_none());
    }
}
