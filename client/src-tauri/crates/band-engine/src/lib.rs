//! band 실시간 합주 엔진(엔진 R-1 ⑩-2, `docs/overhaul/engine-stage5-10-scope.md` 7.2·7.4절).
//!
//! 앱(`band`: Tauri 커맨드·`lib.rs`·`main.rs`)과 결정적 코어(`band-dsp`) 사이의 크레이트다. 장치 I/O·RT 경로·워커·네트워크·
//! 녹음·메트로놈·진단·제어 평면(`manager`)이 여기 있다. tauri에 의존하지 않는다.
//!
//! 장치 백엔드는 피처 뒤에 있다(기본 켬): `backend-cpal`(`engine::device::cpal`, `engine::device::select`의 `SystemBackend`),
//! `backend-wasapi`(Windows에서 `engine::wasapi`). 하네스·시험은 `default-features = false`로 백엔드 없이 쓴다(가짜 장치,
//! `test-support`).

// ⑨b-2(engine-stage5-10-scope.md 6.3절): 크레이트 밖에서 닿지 않는 `pub`을 잡는다. `test-support` 구성에서만 켠다 — 시험만 쓰는
// 항목(`testing`이 다시 내보내는 것)은 제품 구성에서는 밖에서 닿지 않지만 `pub`이어야 해서(가시성은 피처로 가를 수 없다).
// CI의 `cargo clippy --all-targets`(자기 자신 개발 의존이 `test-support`를 켠다)가 `-D warnings`로 지킨다.
#![cfg_attr(feature = "test-support", warn(unreachable_pub))]

pub mod diag;
pub mod engine;
pub mod manager;
pub mod net;
pub mod settings;
pub mod sys;
/// 통합 테스트·예제·측정 도구용 내부 재노출(⑨a). 제품 빌드(피처 없음)에는 없다.
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
