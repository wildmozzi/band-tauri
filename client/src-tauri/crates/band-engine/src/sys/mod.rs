//! 플랫폼 기초(계층 0): 스레드 우선순위, CPU 계측, 벽시계, (시험 구성) 할당 계수. 다른 모듈을 쓰지 않는다.

/// 시험 바이너리의 계수 할당자(엔진 ⑧ 후속). 워커 루프가 시험 구성에서 반복마다 쓰므로 계층 0에 둔다(`testing::alloc`이 다시 내보냄).
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod alloc;
pub(crate) mod cpu;
pub(crate) mod prio;
pub mod time;
