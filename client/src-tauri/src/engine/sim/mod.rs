//! 가상 시간 결정적 하네스(테스트 전용, `cfg(test)`). 실제 지터 버퍼·워커 공급(`PeerFeed`)·출력 콜백 경로를
//! 가상 시계로 돌려 GOAL 수치를 재고, 같은 시드면 같은 숫자를 낸다(구조 정리 PR의 동작 불변 증거).
//!
//! - `tune`: 피어 N → 지터 버퍼 → `PeerFeed` → `OutputRt` 다중 피어 모델(`run_multi`)과 튜닝 스윕.
//! - `long`: 6피어 풀메시 장시간 하네스(5분 기본 테스트, 60분은 `--ignored` 야간 잡), 리포트 Markdown·JSON.
//! - `loss_codec`: 수신 손실 피드백 → 피어별 Opus 전환 시뮬레이션(P1-4e).
//! - `alloc`: 계수 할당자. `run_multi`가 워커 단계 호출의 할당·해제를 세어 0임을 단언한다(엔진 ⑧ 후속).

pub(crate) mod alloc;
mod long;
mod loss_codec;
mod tune;
