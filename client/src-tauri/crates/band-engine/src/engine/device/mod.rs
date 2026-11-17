//! 오디오 I/O 백엔드 추상화와 cpal 구현 (설계 문서 1절, 1.1절).
//!
//! ## ASIO 싱글톤 우회 (기존 audio.rs에서 가져온 지식)
//! - ASIO SDK는 프로세스 전역에서 드라이버를 하나만 로드할 수 있다. 장치 열거와 스트림
//!   생성이 동시에 일어나면 드라이버 로드가 꼬이므로 `DEVICE_ENUM_LOCK`으로 직렬화한다.
//! - ASIO Host를 앱 시작 시 백그라운드 스레드에서 미리 만들지 않는다. 그렇게 하면 이후
//!   오디오 시작 시 Host가 장치를 찾지 못했다. Host는 필요할 때(열거/스트림 생성 시점)에
//!   만들고, 열거용 Host는 함수 끝에서 drop한다.
//! - ASIO 스트림이 살아 있는 동안에는 다시 열거하지 않고 캐시를 돌려준다.
//! - ASIO 장치는 입출력 겸용이므로 입력·출력을 같은 장치에서 연다(듀플렉스).
//!
//! `cpal::Stream`은 `Send`가 아니므로 `AudioBackend::open`은 엔진의 전용 오디오 스레드에서
//! 호출되고, 반환된 가드는 같은 스레드에서 drop된다.
//!
//! 나눔(엔진 ⑥-2, `engine-stage5-10-scope.md` 3.2절): `types`(값 타입·순수 함수, 계층 1), `backend`(`AudioBackend` trait),
//! `cpal`(cpal 구현), `select`(자체 WASAPI 우선·cpal 폴백), `fake`(가짜 장치). 여기에는 `mod`와 `pub use`만 둔다 —
//! 자식이 이 파일의 항목을 쓰면 부모↔자식 순환이다. 엔진 밖(어댑터·테스트)은 옛 경로 `engine::device::…`로 쓴다.

pub(crate) mod backend;
pub mod types;

// ============================================================================
// cpal 구현 — `backend-cpal` 피처 뒤(⑩-2, engine-stage5-10-scope.md 7.4절)
// ============================================================================

#[cfg(feature = "backend-cpal")]
pub(crate) mod cpal;

// ============================================================================
// 시스템 백엔드: 자체 WASAPI 우선, cpal 폴백 (P1-0). cpal이 폴백이자 Windows 밖의 유일한 길이라 `backend-cpal`
// 뒤에 둔다. 자체 WASAPI 부분은 그 안에서 `backend-wasapi`(+ Windows)일 때만 들어간다.
// ============================================================================

#[cfg(feature = "backend-cpal")]
pub mod select;

// ============================================================================
// 가짜 장치 (테스트/통합 테스트용)
// ============================================================================

// 제품 빌드(디버그·릴리스 모두)에는 들어가지 않는다: 단위 테스트, 또는 `test-support` 피처에서만 컴파일된다(⑨a-2).
// 테스트·예제 빌드는 Cargo.toml의 자기 자신 개발 의존이 늘 켠다(engine-architecture.md 7절).
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod fake;

pub use backend::AudioBackend;
pub(crate) use types::{DeviceInfo, StreamInfo};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use types::BackendKind;
pub use types::device_id;
