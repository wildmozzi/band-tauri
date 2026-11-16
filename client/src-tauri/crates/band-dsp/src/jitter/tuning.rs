//! 사용자 '저지연 ↔ 안정' 프로필과 지터 버퍼 설정.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterSettings {
    /// 0이면 자동.
    pub min_ms: f32,
    pub max_ms: f32,
    /// 저지연(0) ↔ 안정(1) 프로필. 0.5 = 균형(기본). `JitterTuning::from_profile`.
    pub profile: f32,
}

/// 사용자 '저지연 ↔ 안정' 프로필(0..1)에서 나온 목표 계산 계수(P1-5, 설계 문서 1.2절).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterTuning {
    /// 지연 분위수: 0 → 0.90, 0.5 → 0.95, 1 → 0.99.
    pub quantile: f64,
    /// 목표에 더하는 여유(프레임 배수): 0.5 이하 0, 1 → 0.5F.
    pub pad_frames: f64,
    /// 짧은 창 추정을 내리기 전 유지 시간(ms): 1s / 2s / 4s(균형 = 이전 고정값 2s). 반복 스파이크는 긴 창이 맡는다.
    pub hold_ms: f64,
    /// 버스트 상향 유지 시간(ms, 마지막 버스트 패킷 이후): 0.5s / 2s / 4s(6초 간격 스파이크를 이어 붙이지 않게, M4).
    pub burst_hold_ms: f64,
    /// 긴 창 블록 수(1초 블록): 5 / 12 / 20. 순위는 `max(3, ceil(블록 수/6))`(3 / 3 / 4) — 안정 쪽에서도 창 안 블록의
    /// 1/6 이상이 높아야 덮는다(PR #20 M4: 30블록·3순위에서는 스파이크 3번이 30초 동안 목표를 붙잡아 0.5→1 구간이 절벽).
    pub long_blocks: usize,
    /// 피어 적응 여유(언더런) 유지·상한 배율(워커 `PeerFeed`): 0.5 / 1 / 2.
    pub margin_scale: f64,
    /// 긴 창(반복 스파이크) 초과분 반영 비율: 0.25 이하 0, 0.5 → 1/6, 1 → 1/2(선형). 스파이크 시작은 어차피 은닉되므로
    /// 전부 덮어도 은닉 이득이 작고(M4 시나리오 0.03%p) 지연은 스파이크 높이만큼 는다.
    pub long_weight: f64,
    /// 버스트 판정 문턱 배율(`max(F/2, 1ms)`에 곱함): 1.5 / 1 / 0.5. 저지연은 분위수가 낮아 꼬리 패킷이 문턱을 자주
    /// 넘으므로 문턱을 높여 진짜 스파이크에만 반응한다.
    pub burst_scale: f64,
    /// 은닉 목표(비율, P1-6b): 0 → 3%, 0.5 → 1.3%, 1 → 0.6%. 늦은 패킷 예산 = 이 값 − 실제 손실률(은닉 목표의
    /// 1/4 ~ 3/4로 제한). 마감을 놓친 뒤 도착한 패킷 비율이 예산을 넘으면 목표에 깊이를 더한다(`tail_extra`).
    /// 균형 1.3%는 GOAL 2절(손실 1%에서 은닉 ≤ 1.5%)에 0.2%p 여유를 둔 값이다.
    pub conceal_target: f64,
}

impl JitterTuning {
    pub fn from_profile(profile: f32) -> Self {
        let p = if profile.is_finite() { profile.clamp(0.0, 1.0) as f64 } else { 0.5 };
        let quantile = if p <= 0.5 { 0.90 + 0.1 * p } else { 0.95 + 0.08 * (p - 0.5) };
        Self {
            quantile,
            pad_frames: (p - 0.5).max(0.0),
            hold_ms: if p <= 0.5 { 1000.0 + 2000.0 * p } else { 2000.0 + 4000.0 * (p - 0.5) },
            burst_hold_ms: if p <= 0.5 { 500.0 + 3000.0 * p } else { 2000.0 + 4000.0 * (p - 0.5) },
            long_blocks: (5.0 + 15.0 * p).round() as usize,
            margin_scale: if p <= 0.5 { 0.5 + p } else { 1.0 + 2.0 * (p - 0.5) },
            burst_scale: 1.5 - p,
            conceal_target: if p <= 0.5 { 0.030 - 0.034 * p } else { 0.013 - 0.014 * (p - 0.5) },
            long_weight: ((p - 0.25) / 0.75).clamp(0.0, 1.0) * 0.5,
        }
    }
}
