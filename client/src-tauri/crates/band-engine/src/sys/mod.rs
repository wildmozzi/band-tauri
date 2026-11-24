//! 플랫폼 기초(계층 0): 스레드 우선순위, CPU 계측, 벽시계, (시험 구성) 할당 계수. 다른 모듈을 쓰지 않는다.

/// 시험 바이너리의 계수 할당자(엔진 ⑧ 후속). 워커 루프가 시험 구성에서 반복마다 쓰므로 계층 0에 둔다(`testing::alloc`이 다시 내보냄).
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod alloc;
pub(crate) mod cpu;
// 파일 바꿔 넣기(임시 파일 + sync + rename): 설정·진단 내보내기.
pub(crate) mod fs;
// 조건부 고정(P 코어 정책 3안)의 판단 — 순수 상태 기계와 원자값(P1-21 3단계 ②). OS 호출은 `placement`가 한다.
pub(crate) mod pin_adapt;
// 하이브리드 CPU 코어 배치 기록·스레드 정책(P1-21).
pub(crate) mod placement;
pub(crate) mod prio;
pub mod time;
