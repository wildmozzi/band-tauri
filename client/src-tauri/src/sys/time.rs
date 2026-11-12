//! 벽시계(계층 0). 엔진·net 안의 시각은 단조 µs다(`docs/overhaul/engine-architecture.md` 12절) — 이 함수들은
//! 밖과 맞추는 값(녹음 이름, 진단 타임라인, 릴레이 토큰 만료)에만 쓴다.

use std::time::{SystemTime, UNIX_EPOCH};

/// 지금 Unix 시각(ms). 시계가 1970년 전이면 0.
pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 지금 Unix 시각(초). 시계가 1970년 전이면 0.
pub fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
