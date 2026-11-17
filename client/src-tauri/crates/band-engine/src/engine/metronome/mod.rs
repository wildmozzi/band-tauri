//! 공유 메트로놈(P2-3, 설계 1.4절).
//!
//! - 시간축: 기준 피어(방장) 엔진의 시계. 박 k의 시각 = `start_us + k·60/bpm`(기준 시계 µs).
//!   방장 엔진은 기준 피어 없음(오프셋 0), 다른 연주자는 `net::clock::ClockSync`로 추정한 오프셋으로 내 시계로 바꾼다.
//! - 정렬: 모든 연주자의 **귀에 같은 절대 시각**에 클릭이 들린다(설계 1.4절의 근거). 클릭 프레임은
//!   출력 콜백 시각 추정(`DeviceClock`)과 장치 출력 버퍼로 "귀 도착 시각"을 맞춘다.
//! - RT 경로: 출력 콜백은 미리 계산한 48kHz 파형(`ClickRt`)과 원자값 하나(`ClickShared::next` = 다음 클릭
//!   프레임 + 강박 비트)만 쓴다. 할당·락·시스템콜·로그 없음. 워커(`BeatScheduler`)가 다음 클릭을 미리 올린다.
//! - 클릭은 장치 출력에만 섞는다. 믹스 탭(관람 방송·녹음)과 측정 자극에는 들어가지 않는다.

// RT 클릭(출력 콜백) — `rt.rs`가 이 노드만 쓴다.
pub(crate) mod click;
// 계약 DTO·검증 — 보고(`engine::report`)·어댑터가 이 노드만 쓴다.
pub mod plan;
pub(crate) mod sched;

pub use click::{ClickRt, ClickShared};
pub(crate) use plan::{choose_hidden_out, MAX_LOOPBACK_OFFSET_MS, MIN_LOOPBACK_OFFSET_MS};
pub use plan::{MetronomePlan, OutputLatencySource};
pub use plan::MetronomeConfig;
pub use plan::measured_hidden_out_us;
pub(crate) use sched::{hidden_out_latency_us, MetroReport, MetronomeCtl, MetronomeRt};
pub use sched::OutClock;
pub use sched::BeatScheduler;
