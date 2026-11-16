//! band 실시간 합주 엔진(엔진 R-1 ⑩-2, `docs/overhaul/engine-stage5-10-scope.md` 7.2·7.4절).
//!
//! 앱(`band`: Tauri 커맨드·`lib.rs`·`main.rs`)과 결정적 코어(`band-dsp`) 사이의 크레이트다. 장치 I/O·RT 경로·워커·네트워크·
//! 녹음·메트로놈·진단·제어 평면(`manager`)이 여기 있다. tauri에 의존하지 않는다.
//!
//! 장치 백엔드는 피처 뒤에 있다(기본 켬): `backend-cpal`(`engine::device::cpal`, `engine::device::select`의 `SystemBackend`),
//! `backend-wasapi`(Windows에서 `engine::wasapi`). 하네스·시험은 `default-features = false`로 백엔드 없이 쓴다(가짜 장치,
//! `test-support`).

pub mod diag;
pub mod engine;
pub mod manager;
pub mod net;
pub mod settings;
pub mod sys;
/// 통합 테스트·예제·측정 도구용 내부 재노출(⑨a). 제품 빌드(피처 없음)에는 없다.
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
