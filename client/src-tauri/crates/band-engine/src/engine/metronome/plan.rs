//! 메트로놈 계획·상태 값 타입과 검증(계약 DTO), 숨은 출력 지연 선택(순수 계산). 스레드·락 없음(엔진 ⑥-3).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// BPM 범위.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 300.0;
/// 박자(마디당 박) 범위.
pub const MAX_BEATS_PER_BAR: u32 = 16;
/// 방장이 시작 시각을 정하지 않으면 지금 + 이만큼(µs). 다른 연주자에게 시그널링이 닿을 여유.
pub const DEFAULT_START_LEAD_US: f64 = 300_000.0;
/// 루프백 보정값 `offsetMs`로 받을 수 있는 범위(ms). 측정 오차로 음수가 나올 수 있다.
pub const MIN_LOOPBACK_OFFSET_MS: f64 = -50.0;
pub const MAX_LOOPBACK_OFFSET_MS: f64 = 200.0;
/// 루프백에서 구한 숨은 출력 지연 상한(µs).
const MAX_MEASURED_HIDDEN_OUT_US: f64 = 100_000.0;

/// `engine_set_metronome` 인자(설계 4절).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomeConfig {
    pub running: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    /// 박 0 시각(기준 피어 시계, µs). 방장(기준 피어 없음)이 비우면 지금 + 300ms.
    #[serde(default)]
    pub start_us: Option<f64>,
    /// 기준 피어(방장 엔진의 peerId). 없으면 내 엔진이 기준(방장).
    #[serde(default)]
    pub ref_peer_id: Option<String>,
}

/// 적용된 계획(`engine_set_metronome` 반환값, 방장이 시그널링으로 알리는 값).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomePlan {
    pub running: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub start_us: f64,
    /// 공유 문자열: 복제가 참조 수 증가 하나다(할당 없음) — 제어 쪽 `stats()`의 계획 락 구간과 워커의 계획 복사(#143 리뷰 L-4).
    /// 직렬화는 문자열 그대로(serde `rc`).
    pub ref_peer_id: Option<Arc<str>>,
}

impl Default for MetronomePlan {
    fn default() -> Self {
        Self { running: false, bpm: 120.0, beats_per_bar: 4, start_us: 0.0, ref_peer_id: None }
    }
}

impl MetronomePlan {
    /// 검증 후 계획으로. `now_us` = 내 엔진 시계.
    pub fn from_config(c: MetronomeConfig, now_us: f64) -> Result<Self, String> {
        if !c.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&c.bpm) {
            return Err(format!("BPM은 {MIN_BPM}..{MAX_BPM}이어야 합니다"));
        }
        if !(1..=MAX_BEATS_PER_BAR).contains(&c.beats_per_bar) {
            return Err(format!("박자는 1..{MAX_BEATS_PER_BAR}이어야 합니다"));
        }
        let ref_peer_id: Option<Arc<str>> = c.ref_peer_id.filter(|s| !s.is_empty()).map(Arc::from);
        let start_us = match (c.start_us, &ref_peer_id) {
            (Some(s), _) if s.is_finite() && s >= 0.0 => s,
            (Some(_), _) => return Err("startUs가 잘못되었습니다".into()),
            (None, None) => now_us + DEFAULT_START_LEAD_US,
            (None, Some(_)) => return Err("기준 피어를 따를 때는 startUs가 필요합니다".into()),
        };
        Ok(Self { running: c.running, bpm: c.bpm, beats_per_bar: c.beats_per_bar, start_us, ref_peer_id })
    }

    pub fn period_us(&self) -> f64 {
        60e6 / self.bpm
    }
}

/// 메트로놈 상태(`EngineStats.metronome`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomeStats {
    pub running: bool,
    /// 시간축이 잡혔다(방장이거나, 기준 피어와 시계 표본이 있음). false면 클릭을 내지 않는다.
    pub synced: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub start_us: f64,
    pub ref_peer_id: Option<String>,
    /// 기준 피어 시계 − 내 시계(ms). 방장이면 0.
    pub clock_offset_ms: Option<f64>,
    /// 시계 추정에 쓴 최소 RTT(ms).
    pub clock_rtt_ms: Option<f64>,
    /// 추정 시계 드리프트(ppm).
    pub drift_ppm: Option<f64>,
    /// 마지막으로 예약한 박 번호(0부터).
    pub beat: Option<u64>,
    pub clicks: u64,
    pub late_clicks: u64,
    /// 콜백 공백 뒤 제 시각이 지나 건너뛴 클릭 수.
    pub skipped_clicks: u64,
    /// 클릭 시각에 반영한 숨은 출력 지연(ms). 출처는 `output_latency_source`.
    /// null = 드라이버 보고도 루프백 측정도 없어 보정 안 됨(cpal WASAPI 등). UI가 "루프백 측정으로 보정"을 안내할 수 있다.
    pub output_latency_ms: Option<f64>,
    /// `output_latency_ms`의 출처(드라이버 보고 / 루프백 측정). null = 보정 안 됨.
    pub output_latency_source: Option<OutputLatencySource>,
    /// 기준 피어가 Pong에 시계를 싣지 않는 옛 엔진이다(PR #34 L4). 이 계획으로는 `synced`가 영원히 false다.
    pub ref_clock_unsupported: bool,
    /// 시계 추정 직선의 무게중심에서 지금까지의 외삽 거리(ms, PR #34 L6). 기울기 오차 × 이 거리가 오프셋 오차가 된다.
    pub clock_extrapolation_ms: Option<f64>,
}

/// 숨은 출력 지연 보정의 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputLatencySource {
    /// 드라이버 보고 출력 지연 − 콜백 크기(ASIO `ASIOGetLatencies`, 자체 WASAPI `GetStreamLatency`).
    Driver,
    /// 루프백 측정 보정값(`offsetMs`)에서 나눈 출력 쪽 몫(`measured_hidden_out_us`).
    Loopback,
}

/// 루프백 측정 보정값 `offset_ms`(= 실측 왕복 − 입력 버퍼 − 출력 버퍼 = 숨은 입력 + 숨은 출력)에서
/// 숨은 출력 지연(µs)을 나눈다(PR #34 M2(a)).
/// - 드라이버가 입력 쪽 지연만 알려 주면(`hidden_in_us`) 그것을 뺀 나머지.
/// - 아니면 반씩(입·출력 대칭 가정: 변환기·WASAPI 엔진 주기·USB 안전 여유는 보통 양방향에 비슷하게 있다).
///   비대칭이면 (숨은 출력 − 숨은 입력)/2만큼 남는다(설계 1.4절).
///
/// 0..100ms로 자른다.
pub fn measured_hidden_out_us(offset_ms: f64, hidden_in_us: Option<f64>) -> f64 {
    let total = offset_ms * 1000.0;
    let out = match hidden_in_us {
        Some(h) => total - h,
        None => total * 0.5,
    };
    if out.is_finite() {
        out.clamp(0.0, MAX_MEASURED_HIDDEN_OUT_US)
    } else {
        0.0
    }
}

/// 숨은 출력 지연 선택: 드라이버 보고가 있으면 그것(직접 출력 쪽 값), 없으면 루프백 측정에서 나눈 값.
pub fn choose_hidden_out(driver_us: Option<f64>, loopback_us: Option<f64>) -> Option<(f64, OutputLatencySource)> {
    driver_us
        .map(|h| (h, OutputLatencySource::Driver))
        .or_else(|| loopback_us.map(|h| (h, OutputLatencySource::Loopback)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation() {
        let c = |bpm: f64, b: u32, s: Option<f64>, r: Option<&str>| MetronomeConfig {
            running: true,
            bpm,
            beats_per_bar: b,
            start_us: s,
            ref_peer_id: r.map(String::from),
        };
        assert!(MetronomePlan::from_config(c(10.0, 4, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(f64::NAN, 4, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 0, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 4, None, Some("h")), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 4, Some(-1.0), None), 0.0).is_err());
        let p = MetronomePlan::from_config(c(120.0, 4, None, None), 1000.0).unwrap();
        assert_eq!(p.start_us, 1000.0 + DEFAULT_START_LEAD_US);
        let p = MetronomePlan::from_config(c(90.0, 3, Some(5.0), Some("")), 0.0).unwrap();
        assert_eq!(p.ref_peer_id, None);
    }

    /// 계약(`engine_set_metronome` 반환값)의 `refPeerId`는 문자열 또는 null이다 — `Arc<str>`를 문자열로 내는 것은 serde `rc`
    /// 피처 하나뿐이라 모양을 고정한다(#168 리뷰 L-2).
    #[test]
    fn plan_serializes_ref_peer_id_as_string() {
        let p = MetronomePlan { ref_peer_id: Some("host".into()), ..MetronomePlan::default() };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["refPeerId"], serde_json::json!("host"));
        assert_eq!(v, serde_json::json!({ "running": false, "bpm": 120.0, "beatsPerBar": 4, "startUs": 0.0, "refPeerId": "host" }));
        let none = serde_json::to_value(MetronomePlan::default()).unwrap();
        assert_eq!(none["refPeerId"], serde_json::Value::Null);
    }

    #[test]
    fn loopback_offset_splits_into_output_share() {
        // PR #34 M2(a): offset = 숨은 입력 + 숨은 출력
        assert_eq!(measured_hidden_out_us(20.0, None), 10_000.0, "입력 쪽 모르면 반");
        assert_eq!(measured_hidden_out_us(20.0, Some(4_000.0)), 16_000.0, "입력 쪽을 알면 뺀 나머지");
        assert_eq!(measured_hidden_out_us(-3.0, None), 0.0, "음수(측정 오차)는 0");
        assert_eq!(measured_hidden_out_us(20.0, Some(25_000.0)), 0.0);
        assert_eq!(measured_hidden_out_us(1000.0, None), MAX_MEASURED_HIDDEN_OUT_US);
        assert_eq!(measured_hidden_out_us(f64::NAN, None), 0.0);
        // 드라이버 보고가 우선, 없으면 루프백, 둘 다 없으면 보정 안 함
        assert_eq!(choose_hidden_out(Some(1.0), Some(2.0)), Some((1.0, OutputLatencySource::Driver)));
        assert_eq!(choose_hidden_out(None, Some(2.0)), Some((2.0, OutputLatencySource::Loopback)));
        assert_eq!(choose_hidden_out(None, None), None);
    }
}
