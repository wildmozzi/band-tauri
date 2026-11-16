//! 자체 WASAPI 백엔드 (Windows 전용, P1-0 / 엔진 리뷰 H2).
//!
//! cpal 0.15의 WASAPI는 공유 모드 10ms 주기 고정이고 독점 모드가 없어 GOAL 1절의 지연 목표를 낼 수 없다.
//! 이 모듈은 `windows` 크레이트로 IAudioClient를 직접 연다.
//!
//! - **독점(`wasapiExclusive`)**: 이벤트 구동 독점 모드, 장치 최소 주기(`GetDevicePeriod`의 최소값).
//!   포맷은 장치가 독점으로 지원하는 것 중 48kHz 우선, 없으면 44.1kHz(엔진이 리샘플), 그다음 96/88.2kHz.
//!   샘플 형식은 f32 → 32비트 정수(32/24 유효) → 16비트 순. 버퍼 정렬 오류(`AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED`)는
//!   장치가 돌려준 크기로 주기를 다시 계산해 재초기화한다.
//! - **저지연 공유(`wasapiShared3`)**: `IAudioClient3::InitializeSharedAudioStream`에 엔진이 허용하는 최소 주기
//!   (`GetSharedModeEnginePeriod`, 보통 128~144프레임 = 2.67~3ms). 포맷은 믹스 포맷 그대로.
//!
//! ## 스레드
//! 입력·출력을 **한 스레드**(`band-wasapi`, MMCSS "Pro Audio")가 이벤트 두 개로 처리한다. 둘 다 신호 상태면
//! 캡처를 먼저 처리한다(`WaitForMultipleObjects`는 낮은 인덱스를 돌려주고 자동 리셋 이벤트는 그것만 리셋한다).
//! 같은 장치 듀플렉스에서는 캡처 직후 렌더가 돌아 입력→출력 순서가 고정된다. COM 객체는 모두 이 스레드에서
//! 만들고 이 스레드에서 해제한다(MTA).
//!
//! ## RT 규칙
//! 루프 안에서 하는 일은 WASAPI 호출(`GetBuffer`/`ReleaseBuffer`/`GetCurrentPadding`/`GetNextPacketSize`)과
//! 대기, 그리고 `InputRt::process`/`OutputRt::process`(할당·락·로그 없음)뿐이다. 오류가 나면 루프를 빠져나온 뒤에
//! 로그를 남긴다. 루프는 할당하지 않는다(무음 캡처 패킷용 0 버퍼는 시작 전에 할당).
//!
//! ## 오류 분류(PR #11 H1)
//! 루프의 WASAPI 호출 실패는 HRESULT로 나눈다(`classify`).
//! - **장치 분실**(`AUDCLNT_E_DEVICE_INVALIDATED`, `AUDCLNT_E_SERVICE_NOT_RUNNING`,
//!   `AUDCLNT_E_RESOURCES_INVALIDATED`): 스트림을 멈추고 `deviceLost`를 켠다.
//! - **치명**(`E_OUTOFMEMORY`): 스트림을 멈추고 `streamFailed`를 켠다.
//! - **일시 오류**(그 밖: `AUDCLNT_E_BUFFER_ERROR`, `AUDCLNT_E_BUFFER_TOO_LARGE`, `AUDCLNT_E_OUT_OF_ORDER` 등):
//!   `streamErrors`만 올리고 다음 이벤트를 기다린다. 같은 방향이 연속 `RESTART_AFTER`회 실패하면 그 스트림을
//!   Stop → Reset → (렌더는 무음 prefill) → Start로 재시작한다(`streamRestarts`). 성공 없이 재시작이
//!   `MAX_RESTARTS`회 이어지거나 연속 실패가 `FAIL_AFTER`회면 치명으로 끝낸다(MS 문서: "다음 주기를 기다리고,
//!   반복될 때만 Stop/Reset/재생성").
//! - 이벤트가 `WAIT_TIMEOUT_MS` 동안 안 오면 `GetCurrentPadding`으로 스트림 상태를 확인한다(이벤트가 멈춘
//!   비기본 장치 분리도 무효화 오류로 드러난다).
//!
//! 정책 루프(`drive_loop`)는 장치 I/O(`LoopIo`)와 분리되어 있어 가짜 HRESULT 시퀀스로 단위 테스트한다.
//!
//! ## 장치 분실
//! - 기본 장치로 연 방향은 감시 스레드가 0.5초마다 기본 장치 ID를 확인해, 바뀌면 `deviceLost`를 켠다
//!   (프론트의 "running=false → 엔진 정지" 감지가 동작하고, 다시 시작하면 새 기본 장치로 열린다).
//!
//! ## 캡처 플래그(PR #11 M2)
//! `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`(장치가 데이터를 버림 = 입력 오버런)는 `inputDiscontinuities`와
//! `xruns`에, `AUDCLNT_BUFFERFLAGS_SILENT`는 `inputSilentPackets`에 센다. 스트림 시작(재시작) 직후 첫 패킷의
//! DISCONTINUITY는 장치가 늘 켜는 값이라 세지 않는다.

// 형식 값 타입·24비트 변환(RT)·HRESULT 분류. COM 객체를 만들지 않는다.
pub mod format;
// COM 보조와 장치 열거.
pub mod enumerate;
// 한 방향 스트림, 이벤트 루프(RT), 엔진용 열기·기본 장치 감시.
pub mod stream;
// 스모크 테스트(`examples/wasapi_probe.rs`만 쓴다).
pub mod probe;

pub use enumerate::{device_infos, enumerate_endpoints, Endpoint, Shared3Periods};
pub use format::{classify, f32_to_i24, fallback_modes, i24_to_f32, ErrClass, SampleKind, ShareMode, StreamFormat};
pub use probe::{probe_stream, ProbeReport};
pub use stream::{
    drive_loop, open_engine_streams, run_loop, DevBuf, DirStream, LoopEnd, LoopIo, OpenFailure, WaitResult, FAIL_AFTER, MAX_RESTARTS,
    RESTART_AFTER,
};
