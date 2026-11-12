//! PCM16 설정에서 피어별 Opus 자동 전환 정책 (native-engine-design 3절 7·9).
//!
//! 설정 코덱이 PCM16일 때 워커는 아래 이유 중 하나에 해당하는 피어에게만 Opus로 보낸다(피어마다 전용 인코더).
//! 두 정책은 같은 복귀 규칙(`Hysteresis`: hold 10초, 플래핑이면 두 배씩 최대 120초, 60초 안정하면 10초)을 쓴다.
//! - `RelayLoad`(P1-4c, `RelayCodecPolicy`): 릴레이 대역 부하. 켜지면 **릴레이 경로 피어 모두**.
//! - `LossFeedback`(P1-4e, `LossCodecPolicy`): 그 피어가 Ping 평문으로 알려 온 **내 소리의 수신 손실률**. 피어마다 따로.
//!   직접 경로에서도 동작한다(송신 측은 상대가 겪는 손실을 이것 말고는 알 수 없다).
//!
//! ## 릴레이 부하 (P1-4c ②, relay-design 4절)
//!
//! PCM16 설정에서 릴레이 경유 피어가 늘면 릴레이 클라이언트당 상한(2 Mbps·2500pps)을 넘는다
//! (PCM16 mono 2.5ms ≈ 0.9 Mbps/피어). 워커는 이 정책이 켜져 있는 동안 **릴레이 경로 피어에게만**
//! Opus(같은 비트레이트 설정)로 보내고, 직접 경로 피어에게는 PCM16을 그대로 보낸다.
//!
//! - 켜기(즉시): PCM16 설정 && 릴레이 경로 피어 ≥ 1 && (예상 PCM 부하 ≥ 90% || 측정 `overBudget`).
//!   예상 부하는 피어 수·프레임·채널로 계산하므로 릴레이가 버리기 **전에** 바꾼다. 측정값은 릴레이 한도가
//!   예상보다 낮은 경우(운영 env로 낮춘 경우 등)를 잡는다 — 이때는 그 시점의 예상 부하를 "넘은 부하"로 기억한다.
//! - 끄기(히스테리시스): 예상 PCM 부하가 `min(80%, 0.85 × 넘은 부하)` 아래로 `hold` 동안 이어지면 PCM16으로
//!   돌아간다. Opus로 보내는 동안의 측정값은 PCM 부하를 말해 주지 않으므로 복귀 판단에는 예상만 쓴다.
//! - 재진입(측정으로 켜진 적이 있을 때): 복귀한 뒤 `LEARNED_TRIP_TTL` 동안은 그때 "넘은 부하"를 기억해,
//!   예상 PCM 부하가 그 값에 다시 닿으면 측정을 기다리지 않고 바로 켠다. 측정 창(1초) 동안 릴레이가 PCM을
//!   버리는 손실이 재진입마다 반복되지 않게 한다.
//! - 플래핑: 복귀한 지 30초 안에 다시 켜지면 `hold`를 두 배로(10 → 20 → 40 → 80 → 120초). PCM으로 60초 넘게
//!   안정하면 10초로 돌아간다.
//!
//! ## 수신 손실 피드백 (P1-4e)
//!
//! 받는 쪽은 Ping(500ms)마다 그 피어 스트림의 최근 5초 손실률·은닉률을 싣는다(설계 2절 "손실 보고").
//! - 켜기: 보고 손실 ≥ `LOSS_ENTER_PCT`(2%)가 `LOSS_ENTER_SUSTAIN`(1초, 보고 3번) 동안 이어진다. 보고 값 자체가
//!   5초 창이라 순간 손실 하나로는 켜지지 않는다(100ms 버스트 하나는 2.5ms 프레임에서 5초 창 2%에 닿을 수 있다 —
//!   그 정도 버스트가 있는 회선이면 PLC가 나은 쪽이라 켜지는 것이 맞다).
//! - 끄기: 보고 손실 < `LOSS_EXIT_PCT`(1%)가 `hold` 동안 이어진다. 보고가 `REPORT_STALE`(3초) 넘게 없으면
//!   (옛 엔진, 상대가 아직 재생 전) 판단 근거가 없는 것으로 보고 켜지 않는다. 켜져 있을 때 보고가 끊기면 그 원인이 손실
//!   자체일 수 있으므로(긴 단절로 상대 재생 멈춤) `REPORT_GIVE_UP`(30초) 동안은 복귀를 새로 시작하지 않고, 그 뒤
//!   "손실 없음" 쪽으로 센다(이미 1% 아래로 세던 타이머는 끊겨도 이어 간다).
//! - 손실률은 망에서 생기므로 코덱을 바꿔도 보고 값은 그대로다. 그래서 복귀는 망이 실제로 좋아졌을 때만 일어난다.
//! - 인코더의 예상 손실(`OPUS_SET_PACKET_LOSS_PERC`)은 보고 손실에 맞춘다(`loss_opus_perc`). 2.5ms 프레임은 CELT 전용이라
//!   in-band FEC(LBRR, SILK 전용)는 실제로 실리지 않는다. 이득은 Opus PLC와, 손실률이 높을수록 프레임 간 예측을 줄이는
//!   CELT 에너지 부호화(손실 뒤 회복이 빠름)다.

use std::time::{Duration, Instant};

use crate::engine::config::{HEADER_BYTES, TAG_BYTES};
use crate::net::types::{BUDGET_CLEAR, BUDGET_WARN, DATA_PREFIX_BYTES, RELAY_BUDGET_BPS, RELAY_BUDGET_PPS};

pub const HOLD_MIN: Duration = Duration::from_secs(10);
pub const HOLD_MAX: Duration = Duration::from_secs(120);
/// 복귀 뒤 이 시간 안에 다시 켜지면 플래핑으로 보고 `hold`를 늘린다.
pub const FLAP_WINDOW: Duration = Duration::from_secs(30);
/// PCM으로 이만큼 안정하면 `hold`를 최소로 되돌린다.
pub const STABLE_RESET: Duration = Duration::from_secs(60);
/// 측정으로 켜진 "넘은 부하"를 복귀 뒤 이만큼 기억한다(재진입 때 측정 없이 바로 켠다).
/// 낮은 한도는 대개 운영 설정이라 오래 유지되지만, 일시적 원인이었을 수도 있어 무기한 기억하지는 않는다.
pub const LEARNED_TRIP_TTL: Duration = Duration::from_secs(300);
/// 측정으로 켜진 경우, 복귀 문턱 = 넘은 부하 × 이 값(측정 한도가 예상보다 낮을 때).
const MEASURED_EXIT_RATIO: f32 = 0.85;

/// 손실 피드백: 보고 손실이 이 값(%) 이상으로 `LOSS_ENTER_SUSTAIN` 이어지면 Opus로.
pub const LOSS_ENTER_PCT: f32 = 2.0;
pub const LOSS_ENTER_SUSTAIN: Duration = Duration::from_secs(1);
/// 손실 피드백: 보고 손실이 이 값(%) 아래로 `hold` 이어지면 PCM16으로.
pub const LOSS_EXIT_PCT: f32 = 1.0;
/// 이보다 오래된 보고는 없는 것으로 본다(Ping 500ms × 6).
pub const REPORT_STALE: Duration = Duration::from_secs(3);
/// 켜진 동안 보고가 이만큼 없으면 근거 없음으로 보고 복귀 쪽으로 센다(그 전에는 복귀를 시작하지 않는다).
pub const REPORT_GIVE_UP: Duration = Duration::from_secs(30);
/// 릴레이 부하로만 켜졌을 때의 인코더 예상 손실(%) — 설정 Opus(1.3절)와 같다.
pub const DEFAULT_OPUS_LOSS_PERC: u8 = 5;

/// 릴레이 경로 피어 `relay_peers`명에게 PCM16을 보낼 때의 예상 부하(릴레이 상한 대비 비율, bps·pps 중 큰 쪽).
/// RelayData 기준(접두부 8 + 엔진 헤더 18 + PCM 페이로드 + 태그 16), 제어 패킷(초당 몇 개)은 뺀다.
pub fn pcm_relay_load(frame_samples: usize, channels: usize, relay_peers: usize) -> f32 {
    if relay_peers == 0 || frame_samples == 0 {
        return 0.0;
    }
    let pps = 48_000.0 / frame_samples as f32 * relay_peers as f32;
    let bytes = (DATA_PREFIX_BYTES + HEADER_BYTES + frame_samples * channels * 2 + TAG_BYTES) as f32;
    (bytes * 8.0 * pps / RELAY_BUDGET_BPS).max(pps / RELAY_BUDGET_PPS)
}

/// 보고 손실(%)에 맞춘 인코더 예상 손실(%): 올림, 2..=25. 보고가 없으면 기본값.
pub fn loss_opus_perc(reported_loss_pct: Option<f32>) -> u8 {
    match reported_loss_pct {
        Some(l) if l.is_finite() => (l.ceil() as i32).clamp(2, 25) as u8,
        _ => DEFAULT_OPUS_LOSS_PERC,
    }
}

/// 자동 전환의 이유(`PeerStats.txCodecReason`). 둘 다 해당하면 `RelayLoad`로 보인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodecReason {
    RelayLoad,
    LossFeedback,
}

/// 이 피어에게 이번 프레임을 자동 전환 Opus로 보낼 이유. `relay_on` = 릴레이 부하 정책 켜짐,
/// `on_relay_path` = 이 피어에게 오디오를 릴레이로 보냄, `loss_on` = 이 피어의 손실 피드백 켜짐.
/// 둘 다 해당하면 `RelayLoad`(설계 3절 7·9). 워커의 인코드 여부와 송신 시 이유 표시가 같은 규칙을 쓴다.
pub fn auto_reason(relay_on: bool, on_relay_path: bool, loss_on: bool) -> Option<CodecReason> {
    if relay_on && on_relay_path {
        Some(CodecReason::RelayLoad)
    } else if loss_on {
        Some(CodecReason::LossFeedback)
    } else {
        None
    }
}

/// 피어 전용 인코더의 예상 손실(%). 손실 피드백이면 보고 손실에 맞추되 릴레이 경로면 기본값 아래로 내리지 않고,
/// 릴레이 부하로만 켜졌거나 꺼져 있으면 기본값(설정 Opus와 같음).
pub fn auto_opus_perc(loss_on: bool, on_relay_path: bool, reported_loss_pct: Option<f32>) -> u8 {
    if loss_on {
        loss_opus_perc(reported_loss_pct).max(if on_relay_path { DEFAULT_OPUS_LOSS_PERC } else { 0 })
    } else {
        DEFAULT_OPUS_LOSS_PERC
    }
}

/// 복귀 hold와 플래핑 규칙(두 정책 공통): 복귀한 지 `FLAP_WINDOW` 안에 다시 켜지면 hold를 두 배로(최대 `HOLD_MAX`),
/// 꺼진 채 `STABLE_RESET` 지나면 `HOLD_MIN`으로.
#[derive(Debug, Clone)]
pub struct Hysteresis {
    hold: Duration,
    last_exit: Option<Instant>,
}

impl Default for Hysteresis {
    fn default() -> Self {
        Self { hold: HOLD_MIN, last_exit: None }
    }
}

impl Hysteresis {
    pub fn hold(&self) -> Duration {
        self.hold
    }

    /// 꺼진 상태의 점검마다.
    fn idle(&mut self, now: Instant) {
        if self.since_exit(now).is_some_and(|d| d >= STABLE_RESET) {
            self.hold = HOLD_MIN;
        }
    }

    fn since_exit(&self, now: Instant) -> Option<Duration> {
        self.last_exit.map(|t| now.duration_since(t))
    }

    fn enter(&mut self, now: Instant) {
        if self.since_exit(now).is_some_and(|d| d < FLAP_WINDOW) {
            self.hold = (self.hold * 2).min(HOLD_MAX);
        }
    }

    fn exit(&mut self, now: Instant) {
        self.last_exit = Some(now);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolicyInputs {
    /// 설정 코덱이 PCM16(이미 Opus면 정책은 늘 꺼져 있다).
    pub pcm_configured: bool,
    /// 지금 오디오를 릴레이로 보내는 피어 수.
    pub relay_peers: usize,
    /// 릴레이 클라이언트가 잰 송신량이 상한의 90%를 넘음(`RelayStats.overBudget`).
    pub measured_over: bool,
    /// `pcm_relay_load`(지금 릴레이 피어 수 기준).
    pub predicted_pcm_load: f32,
}

#[derive(Debug, Clone)]
pub struct RelayCodecPolicy {
    active: bool,
    /// 켜질 때의 예상 PCM 부하(측정으로 켜졌으면 실제 한도가 그보다 낮다는 뜻).
    trip_load: f32,
    below_since: Option<Instant>,
    hy: Hysteresis,
    /// 측정으로 켜졌던 "넘은 부하"(예상 PCM 부하). 복귀 뒤 `LEARNED_TRIP_TTL` 동안 재진입 문턱으로 쓴다.
    learned_trip: Option<f32>,
    /// 켜진 횟수(진단용).
    pub switches: u64,
}

impl Default for RelayCodecPolicy {
    fn default() -> Self {
        Self { active: false, trip_load: BUDGET_WARN, below_since: None, hy: Hysteresis::default(), learned_trip: None, switches: 0 }
    }
}

impl RelayCodecPolicy {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn hold(&self) -> Duration {
        self.hy.hold()
    }

    /// 복귀 문턱(예상 PCM 부하가 이 아래로 `hold` 동안 이어져야 PCM16으로 돌아간다).
    pub fn exit_threshold(&self) -> f32 {
        BUDGET_CLEAR.min(self.trip_load * MEASURED_EXIT_RATIO)
    }

    /// 상태 갱신. 이번 호출에서 꺼짐 → 켜짐으로 바뀌었으면 `Some(true)`, 켜짐 → 꺼짐이면 `Some(false)`.
    pub fn update(&mut self, now: Instant, i: PolicyInputs) -> Option<bool> {
        if !i.pcm_configured {
            let was = self.active;
            self.active = false;
            self.below_since = None;
            return was.then_some(false);
        }
        if !self.active {
            self.hy.idle(now);
            if self.hy.since_exit(now).is_some_and(|d| d >= LEARNED_TRIP_TTL) {
                self.learned_trip = None;
            }
            let predicted = i.predicted_pcm_load >= BUDGET_WARN;
            // 같은 피어 수·프레임이면 같은 값이 나오므로 부동소수 오차만 허용한다
            let learned = self.learned_trip.filter(|&t| i.predicted_pcm_load >= t * 0.999);
            if i.relay_peers > 0 && (predicted || learned.is_some() || i.measured_over) {
                self.trip_load = if predicted {
                    BUDGET_WARN
                } else if let Some(t) = learned {
                    // 기억한 한도로 재진입(측정 없이)
                    t
                } else {
                    // 측정으로 켜졌으면(예상은 아직 90% 미만) 그때의 예상 부하가 실제 한도에 닿은 부하다
                    let t = i.predicted_pcm_load.max(0.05);
                    self.learned_trip = Some(t);
                    t
                };
                self.hy.enter(now);
                self.active = true;
                self.below_since = None;
                self.switches += 1;
                return Some(true);
            }
            return None;
        }
        if i.predicted_pcm_load < self.exit_threshold() {
            let since = *self.below_since.get_or_insert(now);
            if now.duration_since(since) >= self.hy.hold() {
                self.active = false;
                self.below_since = None;
                self.hy.exit(now);
                return Some(false);
            }
        } else {
            self.below_since = None;
        }
        None
    }
}

/// 피어 하나의 손실 피드백 정책(P1-4e). 입력은 그 피어가 마지막으로 보낸 보고(손실 %, 받은 시각).
#[derive(Debug, Clone, Default)]
pub struct LossCodecPolicy {
    active: bool,
    above_since: Option<Instant>,
    below_since: Option<Instant>,
    hy: Hysteresis,
    /// 켜진 횟수(진단용).
    pub switches: u64,
}

impl LossCodecPolicy {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn hold(&self) -> Duration {
        self.hy.hold()
    }

    /// 상태 갱신. `report = Some((손실 %, 받은 시각))`. 바뀌었으면 `Some(켜짐)`.
    /// `pcm_configured = false`(설정이 Opus)면 늘 꺼져 있다.
    pub fn update(&mut self, now: Instant, pcm_configured: bool, report: Option<(f32, Instant)>) -> Option<bool> {
        if !pcm_configured {
            let was = self.active;
            self.active = false;
            self.above_since = None;
            self.below_since = None;
            return was.then_some(false);
        }
        let loss = report.filter(|(l, at)| l.is_finite() && now.saturating_duration_since(*at) <= REPORT_STALE).map(|(l, _)| l);
        if !self.active {
            self.hy.idle(now);
            match loss {
                Some(l) if l >= LOSS_ENTER_PCT => {
                    let since = *self.above_since.get_or_insert(now);
                    if now.duration_since(since) >= LOSS_ENTER_SUSTAIN {
                        self.hy.enter(now);
                        self.active = true;
                        self.above_since = None;
                        self.below_since = None;
                        self.switches += 1;
                        return Some(true);
                    }
                }
                _ => self.above_since = None,
            }
            return None;
        }
        // 보고가 끊긴 원인은 손실 자체일 수도 있다(긴 단절로 상대 지터 버퍼가 재생을 벗어남). 그래서 끊긴 지
        // `REPORT_GIVE_UP` 전까지는 복귀를 새로 시작하지 않는다(이미 1% 아래로 세던 타이머는 이어 간다). 그보다 오래
        // 없으면(옛 엔진으로 교체, 상대 재생 멈춤) 근거 없음 → 복귀 쪽으로 센다(#66 리뷰 L3).
        let gave_up = report.is_none_or(|(_, at)| now.saturating_duration_since(at) >= REPORT_GIVE_UP);
        if loss.is_none() && !gave_up && self.below_since.is_none() {
            return None;
        }
        if loss.is_none_or(|l| l < LOSS_EXIT_PCT) {
            let since = *self.below_since.get_or_insert(now);
            if now.duration_since(since) >= self.hy.hold() {
                self.active = false;
                self.below_since = None;
                self.hy.exit(now);
                return Some(false);
            }
        } else {
            self.below_since = None;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(relay_peers: usize, measured_over: bool) -> PolicyInputs {
        PolicyInputs { pcm_configured: true, relay_peers, measured_over, predicted_pcm_load: pcm_relay_load(120, 1, relay_peers) }
    }

    #[test]
    fn predicted_load_matches_design_numbers() {
        // relay-design 4절: PCM16 mono 2.5ms ≈ 0.9 Mbps/피어, stereo 2.5ms ≈ 1.67 Mbps/피어
        assert!((pcm_relay_load(120, 1, 1) * RELAY_BUDGET_BPS - 902_400.0).abs() < 1.0);
        assert!((pcm_relay_load(120, 2, 1) * RELAY_BUDGET_BPS - 1_670_400.0).abs() < 1.0);
        assert!(pcm_relay_load(120, 1, 2) >= BUDGET_WARN, "mono 2.5ms 피어 2명 = 90%");
        assert!(pcm_relay_load(120, 1, 1) < BUDGET_CLEAR);
        assert!(pcm_relay_load(240, 1, 2) < BUDGET_WARN, "mono 5ms 피어 2명 ≈ 1.67 Mbps");
        assert!(pcm_relay_load(240, 1, 3) >= BUDGET_WARN, "mono 5ms 피어 3명 ≈ 2.5 Mbps");
        assert!(pcm_relay_load(240, 2, 2) >= BUDGET_WARN, "stereo 5ms 피어 2명 ≈ 3.2 Mbps");
        assert_eq!(pcm_relay_load(120, 1, 0), 0.0);
    }

    #[test]
    fn switches_on_prediction_and_returns_after_hold() {
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        assert_eq!(p.update(t0, inputs(1, false)), None);
        assert!(!p.active());
        assert_eq!(p.update(t0, inputs(2, false)), Some(true), "릴레이 피어 2명(90%) → 즉시 Opus");
        // 부하가 줄어도 hold(10초) 동안은 유지
        assert_eq!(p.update(t0 + Duration::from_secs(1), inputs(1, false)), None);
        assert_eq!(p.update(t0 + Duration::from_secs(10), inputs(1, false)), None);
        assert!(p.active());
        assert_eq!(p.update(t0 + Duration::from_secs(11), inputs(1, false)), Some(false));
        assert!(!p.active());
    }

    #[test]
    fn exit_timer_restarts_when_load_rises_again() {
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        p.update(t0, inputs(2, false));
        p.update(t0 + Duration::from_secs(1), inputs(1, false));
        p.update(t0 + Duration::from_secs(5), inputs(2, false)); // 다시 90%: 타이머 리셋
        assert_eq!(p.update(t0 + Duration::from_secs(12), inputs(1, false)), None);
        assert_eq!(p.update(t0 + Duration::from_secs(22), inputs(1, false)), Some(false));
    }

    #[test]
    fn measured_over_budget_switches_and_needs_lower_load_to_return() {
        // 릴레이 한도가 예상보다 낮다(예상 45%에서 측정 초과) → 복귀는 45% × 0.85 아래에서만
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        assert_eq!(p.update(t0, inputs(1, true)), Some(true));
        let thr = p.exit_threshold();
        assert!((thr - pcm_relay_load(120, 1, 1) * 0.85).abs() < 1e-6);
        assert_eq!(p.update(t0 + Duration::from_secs(30), inputs(1, false)), None, "같은 부하로는 복귀하지 않는다");
        assert!(p.active());
        assert_eq!(p.update(t0 + Duration::from_secs(31), inputs(0, false)), None);
        assert_eq!(p.update(t0 + Duration::from_secs(41), inputs(0, false)), Some(false), "릴레이 피어가 없으면 복귀");
        // 복귀 뒤 같은 부하로 재진입: 측정(overBudget)을 기다리지 않고 바로 켠다(1초 폐기 반복 방지)
        let t1 = t0 + Duration::from_secs(100);
        assert_eq!(p.update(t1, inputs(1, false)), Some(true), "기억한 한도로 즉시 재진입");
        assert!((p.exit_threshold() - thr).abs() < 1e-6, "복귀 문턱도 기억한 한도 기준");
    }

    #[test]
    fn learned_trip_expires_after_ttl() {
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        assert_eq!(p.update(t0, inputs(1, true)), Some(true));
        p.update(t0 + Duration::from_secs(1), inputs(0, false));
        let exit = t0 + Duration::from_secs(12);
        assert_eq!(p.update(exit, inputs(0, false)), Some(false));
        // 피어 0명이면 기억이 있어도 켜지 않는다
        assert_eq!(p.update(exit + Duration::from_secs(1), inputs(0, false)), None);
        // 기억 만료 뒤에는 예상(45%)만으로 켜지지 않고 다시 측정을 기다린다
        assert_eq!(p.update(exit + LEARNED_TRIP_TTL, inputs(1, false)), None);
        assert!(!p.active());
        assert_eq!(p.update(exit + LEARNED_TRIP_TTL, inputs(1, true)), Some(true), "측정 초과면 다시 켜진다");
    }

    #[test]
    fn no_switch_without_relay_peers_or_when_opus_configured() {
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        assert_eq!(p.update(t0, inputs(0, true)), None, "릴레이 피어 0 → 측정 초과여도 끈 채로");
        let opus = PolicyInputs { pcm_configured: false, ..inputs(3, true) };
        assert_eq!(p.update(t0, opus), None);
        p.update(t0, inputs(2, false));
        assert_eq!(p.update(t0, opus), Some(false), "설정이 Opus면 정책은 꺼진다");
    }

    #[test]
    fn flapping_doubles_hold_and_stability_resets_it() {
        let t0 = Instant::now();
        let mut p = RelayCodecPolicy::default();
        let mut t = t0;
        let mut holds = vec![];
        for _ in 0..5 {
            assert_eq!(p.update(t, inputs(2, false)), Some(true));
            holds.push(p.hold().as_secs());
            p.update(t + Duration::from_millis(1), inputs(1, false));
            t += p.hold() + Duration::from_millis(2);
            assert_eq!(p.update(t, inputs(1, false)), Some(false));
            t += Duration::from_secs(5); // 5초 뒤 다시 초과(플래핑)
        }
        assert_eq!(holds, vec![10, 20, 40, 80, 120]);
        t += STABLE_RESET;
        p.update(t, inputs(1, false));
        assert_eq!(p.update(t, inputs(2, false)), Some(true));
        assert_eq!(p.hold(), HOLD_MIN, "60초 안정 뒤 최소로");
    }

    fn rep(loss: f32, at: Instant) -> Option<(f32, Instant)> {
        Some((loss, at))
    }

    /// 500ms마다 같은 보고를 받으며 `secs`초 동안 갱신. 처음 바뀐 시각(시작부터 ms)과 값을 돌려준다(바뀌면 멈춤).
    fn run_reports(p: &mut LossCodecPolicy, t0: Instant, from_ms: u64, secs: u64, loss: Option<f32>) -> Vec<(u64, bool)> {
        let mut out = vec![];
        let mut ms = from_ms;
        while ms < from_ms + secs * 1000 {
            let now = t0 + Duration::from_millis(ms);
            // 보고는 500ms마다 오고, 점검은 200ms마다다(워커)
            let at = t0 + Duration::from_millis(ms / 500 * 500);
            if let Some(c) = p.update(now, true, loss.and_then(|l| rep(l, at))) {
                out.push((ms, c));
                break; // 바뀐 뒤의 idle(안정 리셋)은 호출자가 정한다
            }
            ms += 200;
        }
        out
    }

    #[test]
    fn loss_feedback_enters_after_sustain_and_exits_after_hold() {
        let t0 = Instant::now();
        let mut p = LossCodecPolicy::default();
        assert!(run_reports(&mut p, t0, 0, 5, Some(1.5)).is_empty(), "2% 미만은 켜지지 않는다");
        let on = run_reports(&mut p, t0, 5000, 3, Some(2.5));
        assert_eq!(on, vec![(6000, true)], "2% 이상 1초 지속 → Opus");
        // 1~2% 사이: 유지(복귀 문턱 1%)
        assert!(run_reports(&mut p, t0, 8000, 30, Some(1.5)).is_empty());
        assert!(p.active());
        let off = run_reports(&mut p, t0, 38_000, 15, Some(0.3));
        assert_eq!(off, vec![(48_000, false)], "1% 미만이 hold(10초) 이어지면 PCM16");
    }

    #[test]
    fn loss_feedback_ignores_short_spikes_and_stale_reports() {
        let t0 = Instant::now();
        let mut p = LossCodecPolicy::default();
        // 2% 이상이 0.8초만 → 켜지지 않음
        assert!(run_reports(&mut p, t0, 0, 1, Some(3.0)).is_empty());
        assert!(run_reports(&mut p, t0, 1000, 2, Some(0.5)).is_empty());
        // 오래된 보고(3초 넘음)는 무시: 같은 높은 값이 계속 남아 있어도 켜지 않는다
        let old = t0;
        for ms in (3000..10_000).step_by(200) {
            assert_eq!(p.update(t0 + Duration::from_millis(ms + 3500), true, rep(9.0, old)), None);
        }
        assert!(!p.active());
        // 설정이 Opus면 늘 꺼짐
        let mut q = LossCodecPolicy::default();
        run_reports(&mut q, t0, 0, 3, Some(5.0));
        assert!(q.active());
        assert_eq!(q.update(t0 + Duration::from_secs(4), false, rep(5.0, t0 + Duration::from_secs(4))), Some(false));
    }

    #[test]
    fn loss_feedback_returns_when_reports_stop() {
        let t0 = Instant::now();
        let mut p = LossCodecPolicy::default();
        run_reports(&mut p, t0, 0, 3, Some(4.0));
        assert!(p.active());
        // 보고가 끊김(상대가 옛 엔진으로 바뀜, 또는 심한 손실로 상대 재생이 멈춤): 30초 동안은 복귀를 시작하지 않고
        // (손실이 원인일 수 있음), 그 뒤 "근거 없음"으로 세어 hold 뒤 복귀
        let last = t0 + Duration::from_millis(2500);
        let mut off = None;
        for ms in (3000..60_000).step_by(200) {
            if p.update(t0 + Duration::from_millis(ms), true, rep(4.0, last)).is_some() {
                off = Some(ms);
                break;
            }
        }
        assert_eq!(off, Some(42_600), "마지막 보고(2.5초) + 30초(32.6초 점검) + hold 10초");
        // 보고 자체가 없는(None) 피어는 바로 복귀 쪽으로 센다
        let mut q = LossCodecPolicy::default();
        run_reports(&mut q, t0, 0, 3, Some(4.0));
        assert!(q.active());
        assert_eq!(q.update(t0 + Duration::from_secs(4), true, None), None);
        assert_eq!(q.update(t0 + Duration::from_secs(14), true, None), Some(false));
    }

    /// 1% 아래로 세던 중에 보고가 끊기면 그 타이머는 이어 간다(복귀를 막지 않음). 1~2% 사이로 머물다 끊기면 시작하지 않는다.
    #[test]
    fn loss_feedback_stale_reports_keep_running_exit_timer_only() {
        let t0 = Instant::now();
        let mut p = LossCodecPolicy::default();
        run_reports(&mut p, t0, 0, 3, Some(4.0));
        assert!(p.active());
        // 0.3% 보고 4초(타이머 시작), 그 뒤 끊김 → 시작 + 10초에 복귀
        let start = t0 + Duration::from_secs(3);
        assert_eq!(p.update(start, true, rep(0.3, start)), None);
        let last = start + Duration::from_secs(4);
        assert_eq!(p.update(last, true, rep(0.3, last)), None);
        assert_eq!(p.update(start + Duration::from_secs(9), true, rep(0.3, last)), None);
        assert_eq!(p.update(start + Duration::from_secs(10), true, rep(0.3, last)), Some(false));
    }

    #[test]
    fn loss_feedback_flapping_doubles_hold() {
        let t0 = Instant::now();
        let mut p = LossCodecPolicy::default();
        let mut ms = 0;
        let mut holds = vec![];
        for _ in 0..4 {
            let on = run_reports(&mut p, t0, ms, 3, Some(3.0));
            assert_eq!(on.len(), 1);
            holds.push(p.hold().as_secs());
            ms += 3000;
            let off = run_reports(&mut p, t0, ms, 130, Some(0.2));
            assert_eq!(off.len(), 1);
            ms = off[0].0 + 5000; // 5초 뒤 다시 손실(플래핑)
        }
        assert_eq!(holds, vec![10, 20, 40, 80]);
    }

    /// 두 이유가 겹칠 때(#66 리뷰 L4): 릴레이 경로면 `RelayLoad`로 보이고, 예상 손실은 보고 기반과 기본값(5) 중 큰 쪽.
    #[test]
    fn overlapping_reasons_show_relay_load_and_keep_default_perc() {
        use CodecReason::*;
        // (relay_on, 릴레이 경로, 손실 피드백) → 이유
        assert_eq!(auto_reason(true, true, true), Some(RelayLoad));
        assert_eq!(auto_reason(true, true, false), Some(RelayLoad));
        assert_eq!(auto_reason(true, false, true), Some(LossFeedback), "직접 경로 피어는 릴레이 부하 대상이 아니다");
        assert_eq!(auto_reason(false, true, true), Some(LossFeedback));
        assert_eq!(auto_reason(true, false, false), None);
        assert_eq!(auto_reason(false, true, false), None);
        assert_eq!(auto_reason(false, false, false), None);
        // 예상 손실
        assert_eq!(auto_opus_perc(true, true, Some(2.5)), DEFAULT_OPUS_LOSS_PERC, "겹치면 max(3, 5)");
        assert_eq!(auto_opus_perc(true, true, Some(9.2)), 10);
        assert_eq!(auto_opus_perc(true, false, Some(2.5)), 3, "직접 경로 손실 피드백은 보고 기반");
        assert_eq!(auto_opus_perc(true, false, None), DEFAULT_OPUS_LOSS_PERC);
        assert_eq!(auto_opus_perc(false, true, Some(9.0)), DEFAULT_OPUS_LOSS_PERC, "릴레이 부하만이면 기본값");
    }

    #[test]
    fn opus_perc_follows_reported_loss() {
        assert_eq!(loss_opus_perc(None), DEFAULT_OPUS_LOSS_PERC);
        assert_eq!(loss_opus_perc(Some(0.4)), 2);
        assert_eq!(loss_opus_perc(Some(2.1)), 3);
        assert_eq!(loss_opus_perc(Some(7.0)), 7);
        assert_eq!(loss_opus_perc(Some(60.0)), 25);
        assert_eq!(loss_opus_perc(Some(f32::NAN)), DEFAULT_OPUS_LOSS_PERC);
    }
}
