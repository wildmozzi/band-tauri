//! 릴레이 경로 Opus 자동 전환 정책 (P1-4c ②, relay-design 4절·native-engine-design 3절 7).
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

use std::time::{Duration, Instant};

use crate::engine::config::{HEADER_BYTES, TAG_BYTES};
use crate::net::relay::{BUDGET_CLEAR, BUDGET_WARN, DATA_PREFIX_BYTES, RELAY_BUDGET_BPS, RELAY_BUDGET_PPS};

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
    hold: Duration,
    last_exit: Option<Instant>,
    /// 측정으로 켜졌던 "넘은 부하"(예상 PCM 부하). 복귀 뒤 `LEARNED_TRIP_TTL` 동안 재진입 문턱으로 쓴다.
    learned_trip: Option<f32>,
    /// 켜진 횟수(진단용).
    pub switches: u64,
}

impl Default for RelayCodecPolicy {
    fn default() -> Self {
        Self { active: false, trip_load: BUDGET_WARN, below_since: None, hold: HOLD_MIN, last_exit: None, learned_trip: None, switches: 0 }
    }
}

impl RelayCodecPolicy {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn hold(&self) -> Duration {
        self.hold
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
            if self.last_exit.is_some_and(|t| now.duration_since(t) >= STABLE_RESET) {
                self.hold = HOLD_MIN;
            }
            if self.last_exit.is_some_and(|t| now.duration_since(t) >= LEARNED_TRIP_TTL) {
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
                if self.last_exit.is_some_and(|t| now.duration_since(t) < FLAP_WINDOW) {
                    self.hold = (self.hold * 2).min(HOLD_MAX);
                }
                self.active = true;
                self.below_since = None;
                self.switches += 1;
                return Some(true);
            }
            return None;
        }
        if i.predicted_pcm_load < self.exit_threshold() {
            let since = *self.below_since.get_or_insert(now);
            if now.duration_since(since) >= self.hold {
                self.active = false;
                self.below_since = None;
                self.last_exit = Some(now);
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
}
