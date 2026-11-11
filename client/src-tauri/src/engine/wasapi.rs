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

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::{CloseHandle, E_OUTOFMEMORY, HANDLE, PROPERTYKEY, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_ENDPOINT_CREATE_FAILED,
    AUDCLNT_E_INVALID_DEVICE_PERIOD, AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

use super::config::{DriverKind, EngineConfig};
use super::device::{device_id, device_name_from_id, device_tag, BackendKind, DeviceInfo, StreamInfo};
use super::rt::{InputRt, OutputRt};
use super::stats::AudioCounters;

const HNS_PER_SEC: f64 = 10_000_000.0;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// PKEY_Device_FriendlyName
const PKEY_FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// 독점 포맷 탐색 순서: 48k 우선, 44.1k(리샘플), 그다음 고레이트.
const EXCLUSIVE_RATES: [u32; 4] = [48_000, 44_100, 96_000, 88_200];
/// 장치 목록에 보고할 독점 지원 레이트(탐색 비용을 줄이려고 대표값만).
const LIST_RATES: [u32; 3] = [44_100, 48_000, 96_000];
/// 변환 버퍼(f32 샘플): 무음 캡처 패킷의 0, 24비트 패킹 변환. 독점 192kHz 8채널 50ms도 덮는다.
/// 더 큰 패킷은 이 크기로 나눠 처리한다.
const SCRATCH_SAMPLES: usize = 192_000 / 20 * 8;
/// 이벤트 대기 타임아웃. 이 시간 동안 이벤트가 없으면 스트림 상태를 확인한다(L1).
const WAIT_TIMEOUT_MS: u32 = 100;
/// 같은 방향 연속 일시 오류가 이 횟수면 그 스트림을 재시작한다.
pub const RESTART_AFTER: u32 = 4;
/// 성공한 서비스 없이 재시작이 이 횟수 이어지면 치명.
pub const MAX_RESTARTS: u32 = 3;
/// 연속 일시 오류(재시작 실패 포함)가 이 횟수면 치명(독점 2~3ms 주기 ≈ 0.1~0.15초).
pub const FAIL_AFTER: u32 = 50;

/// 열 모드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    Exclusive,
    Shared3,
}

/// 열기 시도 순서(L3): 요청 그대로 → 한 방향만 저지연 공유로(다른 앱이 한쪽만 쥐고 있어도 다른 쪽 독점 유지)
/// → 둘 다 저지연 공유. 그다음은 호출자가 cpal로 폴백한다.
pub fn fallback_modes(mi: ShareMode, mo: ShareMode) -> Vec<(ShareMode, ShareMode)> {
    use ShareMode::{Exclusive, Shared3};
    let mut v = vec![(mi, mo)];
    let mut add = |m: (ShareMode, ShareMode)| {
        if !v.contains(&m) {
            v.push(m);
        }
    };
    if mi == Exclusive && mo == Exclusive {
        add((Exclusive, Shared3));
        add((Shared3, Exclusive));
    }
    add((if mi == Exclusive { Shared3 } else { mi }, if mo == Exclusive { Shared3 } else { mo }));
    v
}

impl ShareMode {
    pub fn backend(self) -> BackendKind {
        match self {
            ShareMode::Exclusive => BackendKind::WasapiExclusive,
            ShareMode::Shared3 => BackendKind::WasapiShared3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    /// 32비트 컨테이너 정수(유효 24 또는 32비트, 상위 정렬 = 24-in-32).
    I32,
    /// 24비트 패킹(3바이트 서브슬롯, 리틀 엔디언). UAC2 USB 인터페이스가 독점으로 흔히 요구한다(M1).
    I24,
    I16,
}

/// WAVEFORMATEXTENSIBLE 채널 마스크.
const SPEAKER_FRONT_CENTER: u32 = 0x4;
const SPEAKER_STEREO: u32 = 0x3;
/// KSAUDIO_SPEAKER_DIRECTOUT: 스피커 배치 없음(다채널 프로 인터페이스 드라이버가 기대하는 경우가 많다).
const SPEAKER_DIRECTOUT: u32 = 0;

/// 채널 수의 기본 마스크: 모노 = FC, 스테레오 = FL|FR, 그 이상 = 하위 n비트.
fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => SPEAKER_FRONT_CENTER,
        2 => SPEAKER_STEREO,
        n => ((1u64 << n.min(32)) - 1) as u32,
    }
}

/// 독점 포맷 협상 때 시도할 채널 마스크 순서(M1):
/// 믹스 포맷과 채널 수가 같으면 그 마스크(드라이버가 자기 배치를 기대) → 기본 마스크(1·2채널) 또는
/// DIRECTOUT(3채널 이상) → 나머지 하나.
fn mask_candidates(channels: u16, mix: Option<(u16, u32)>) -> Vec<u32> {
    let mut v = Vec::with_capacity(3);
    if let Some((mc, mm)) = mix {
        if mc == channels && mm != SPEAKER_DIRECTOUT {
            v.push(mm);
        }
    }
    let order = if channels <= 2 {
        [default_mask(channels), SPEAKER_DIRECTOUT]
    } else {
        [SPEAKER_DIRECTOUT, default_mask(channels)]
    };
    for m in order {
        if !v.contains(&m) {
            v.push(m);
        }
    }
    v
}

/// 장치 스트림 포맷.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub kind: SampleKind,
    pub channels: u16,
    pub rate: u32,
    pub valid_bits: u16,
}

impl StreamFormat {
    fn container_bytes(&self) -> u16 {
        match self.kind {
            SampleKind::F32 | SampleKind::I32 => 4,
            SampleKind::I24 => 3,
            SampleKind::I16 => 2,
        }
    }

    /// 기본 채널 마스크로 만든 WAVEFORMATEXTENSIBLE.
    #[cfg(test)]
    fn to_wfx(self) -> WAVEFORMATEXTENSIBLE {
        self.to_wfx_masked(default_mask(self.channels))
    }

    fn to_wfx_masked(self, mask: u32) -> WAVEFORMATEXTENSIBLE {
        let bytes = self.container_bytes();
        let block = bytes * self.channels;
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE,
                nChannels: self.channels,
                nSamplesPerSec: self.rate,
                nAvgBytesPerSec: self.rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: bytes * 8,
                cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: self.valid_bits },
            dwChannelMask: mask,
            SubFormat: if self.kind == SampleKind::F32 { SUBTYPE_IEEE_FLOAT } else { SUBTYPE_PCM },
        }
    }

    /// 장치가 준 WAVEFORMATEX(믹스 포맷 등)를 해석한다. 엔진이 다루지 못하는 형식이면 None.
    /// 두 번째 값은 채널 마스크(확장 포맷이 아니면 None).
    ///
    /// # Safety
    /// `p`는 유효한 WAVEFORMATEX(확장이면 WAVEFORMATEXTENSIBLE)를 가리켜야 한다.
    unsafe fn from_wfx_mask(p: *const WAVEFORMATEX) -> Option<(Self, Option<u32>)> {
        if p.is_null() {
            return None;
        }
        // SAFETY: 호출자가 유효한 포인터를 보장한다. packed 구조라 read_unaligned로 읽는다.
        let f = unsafe { std::ptr::read_unaligned(p) };
        let (tag, valid, mask) = if f.wFormatTag == WAVE_FORMAT_EXTENSIBLE && f.cbSize >= 22 {
            // SAFETY: cbSize가 확장 필드를 덮는다.
            let x = unsafe { std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE) };
            let sub = x.SubFormat;
            // SAFETY: union의 u16 필드 읽기.
            let valid = unsafe { x.Samples.wValidBitsPerSample };
            let tag = if sub == SUBTYPE_IEEE_FLOAT {
                WAVE_FORMAT_IEEE_FLOAT
            } else if sub == SUBTYPE_PCM {
                WAVE_FORMAT_PCM
            } else {
                0
            };
            (tag, valid, Some(x.dwChannelMask))
        } else {
            (f.wFormatTag, f.wBitsPerSample, None)
        };
        let kind = match (tag, f.wBitsPerSample) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleKind::F32,
            (WAVE_FORMAT_PCM, 32) => SampleKind::I32,
            (WAVE_FORMAT_PCM, 24) => SampleKind::I24,
            (WAVE_FORMAT_PCM, 16) => SampleKind::I16,
            _ => return None,
        };
        // 블록 정렬이 컨테이너 × 채널과 다르면(드문 비표준 패딩) 다루지 않는다.
        let fmt = Self { kind, channels: f.nChannels, rate: f.nSamplesPerSec, valid_bits: valid.max(1) };
        if f.nBlockAlign != fmt.container_bytes() * fmt.channels {
            return None;
        }
        Some((fmt, mask))
    }

    /// # Safety
    /// `from_wfx_mask`와 같다.
    unsafe fn from_wfx(p: *const WAVEFORMATEX) -> Option<Self> {
        // SAFETY: 호출자 보장.
        unsafe { Self::from_wfx_mask(p) }.map(|x| x.0)
    }
}

// ============================================================================
// 24비트 패킹 변환(RT, 할당 없음)
// ============================================================================

const I24_SCALE: f32 = 8_388_608.0; // 2^23

/// 24비트 패킹(리틀 엔디언 3바이트) → f32. `dst.len()`과 `src.len() / 3` 중 작은 만큼 변환하고 그 수를 돌려준다.
pub fn i24_to_f32(src: &[u8], dst: &mut [f32]) -> usize {
    let mut n = 0;
    for (b, d) in src.as_chunks::<3>().0.iter().zip(dst.iter_mut()) {
        // 상위 3바이트에 놓고 산술 시프트로 부호 확장
        let v = i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8;
        *d = v as f32 / I24_SCALE;
        n += 1;
    }
    n
}

/// f32 → 24비트 패킹. [-1, 1]로 자르고 반올림한다. 변환한 샘플 수를 돌려준다.
pub fn f32_to_i24(src: &[f32], dst: &mut [u8]) -> usize {
    let mut n = 0;
    for (s, b) in src.iter().zip(dst.as_chunks_mut::<3>().0.iter_mut()) {
        let v = (s.clamp(-1.0, 1.0) * I24_SCALE).round().clamp(-I24_SCALE, I24_SCALE - 1.0) as i32;
        let le = v.to_le_bytes();
        b.copy_from_slice(&le[..3]);
        n += 1;
    }
    n
}

// ============================================================================
// HRESULT 분류(H1)
// ============================================================================

/// 루프 오류 분류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrClass {
    /// 장치가 사라짐/무효화: `deviceLost`.
    DeviceLost,
    /// 복구 불가(메모리 부족): 스트림 중단.
    Fatal,
    /// 일시 오류: 다음 이벤트에 다시 시도, 반복되면 재시작.
    Transient,
}

pub fn classify(code: HRESULT) -> ErrClass {
    if code == AUDCLNT_E_DEVICE_INVALIDATED || code == AUDCLNT_E_SERVICE_NOT_RUNNING || code == AUDCLNT_E_RESOURCES_INVALIDATED {
        ErrClass::DeviceLost
    } else if code == E_OUTOFMEMORY {
        ErrClass::Fatal
    } else {
        ErrClass::Transient
    }
}

/// `IAudioClient3` 공유 모드 엔진 주기(프레임).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shared3Periods {
    pub default: u32,
    pub fundamental: u32,
    pub min: u32,
    pub max: u32,
}

/// 장치(엔드포인트) 정보.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// WASAPI 엔드포인트 ID.
    pub endpoint_id: String,
    pub name: String,
    pub is_input: bool,
    pub is_default: bool,
    pub mix: Option<StreamFormat>,
    /// `GetDevicePeriod`(100ns 단위): (기본, 최소).
    pub device_period_hns: (i64, i64),
    pub shared3: Option<Shared3Periods>,
    /// 독점으로 지원하는 포맷(대표 레이트 × 형식 × 채널).
    pub exclusive_formats: Vec<StreamFormat>,
}

// ============================================================================
// COM 보조
// ============================================================================

struct ComGuard(bool);

impl ComGuard {
    fn init() -> Self {
        // SAFETY: 현재 스레드의 COM 초기화. 이미 다른 모드로 초기화된 스레드면 실패하고 그대로 쓴다.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: 성공한 CoInitializeEx와 짝을 맞춘다.
            unsafe { CoUninitialize() };
        }
    }
}

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {e} (0x{:08X})", e.code().0 as u32)
}

/// CoTaskMem 문자열을 String으로 옮기고 해제한다.
fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: p는 COM이 할당한 NUL 종료 UTF-16 문자열이다. 읽은 뒤 한 번만 해제한다.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    // SAFETY: COM이 초기화된 스레드에서 호출한다.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| err("장치 열거기 생성 실패", e))
}

fn endpoint_id(dev: &IMMDevice) -> String {
    // SAFETY: 유효한 IMMDevice.
    unsafe { dev.GetId() }.map(take_pwstr).unwrap_or_default()
}

fn friendly_name(dev: &IMMDevice) -> String {
    // SAFETY: 유효한 IMMDevice. PROPVARIANT는 읽은 뒤 PropVariantClear로 해제한다.
    unsafe {
        let Ok(store) = dev.OpenPropertyStore(STGM_READ) else {
            return String::new();
        };
        let Ok(mut pv) = store.GetValue(&PKEY_FRIENDLY_NAME) else {
            return String::new();
        };
        let s = PropVariantToStringAlloc(&pv).map(take_pwstr).unwrap_or_default();
        let _ = PropVariantClear(&mut pv);
        s
    }
}

fn flow(is_input: bool) -> EDataFlow {
    if is_input {
        eCapture
    } else {
        eRender
    }
}

fn default_endpoint_id(en: &IMMDeviceEnumerator, is_input: bool) -> Option<String> {
    // SAFETY: 유효한 열거기.
    unsafe { en.GetDefaultAudioEndpoint(flow(is_input), eConsole) }.ok().map(|d| endpoint_id(&d))
}

/// 이름으로 활성 엔드포인트를 찾는다. 이름이 없으면 기본 장치.
fn find_device(en: &IMMDeviceEnumerator, is_input: bool, name: Option<&str>) -> Result<IMMDevice, String> {
    let what = if is_input { "입력" } else { "출력" };
    // SAFETY: 유효한 열거기와 컬렉션.
    unsafe {
        match name {
            None => en.GetDefaultAudioEndpoint(flow(is_input), eConsole).map_err(|e| err(&format!("기본 {what} 장치 없음"), e)),
            Some(n) => {
                let col = en.EnumAudioEndpoints(flow(is_input), DEVICE_STATE_ACTIVE).map_err(|e| err("장치 열거 실패", e))?;
                for i in 0..col.GetCount().unwrap_or(0) {
                    if let Ok(d) = col.Item(i) {
                        if friendly_name(&d) == n {
                            return Ok(d);
                        }
                    }
                }
                Err(format!("{what} 장치를 찾을 수 없습니다: {n}"))
            }
        }
    }
}

fn activate<T: Interface>(dev: &IMMDevice) -> Result<T, String> {
    // SAFETY: 유효한 IMMDevice. 반환 인터페이스는 이 스레드에서만 쓴다.
    unsafe { dev.Activate::<T>(CLSCTX_ALL, None) }.map_err(|e| err("IAudioClient 활성화 실패", e))
}

/// 믹스 포맷과 그 채널 마스크.
fn mix_format(client: &IAudioClient) -> (Option<StreamFormat>, Option<(u16, u32)>) {
    // SAFETY: GetMixFormat이 할당한 포맷을 읽고 해제한다.
    unsafe {
        let Ok(p) = client.GetMixFormat() else {
            return (None, None);
        };
        let f = StreamFormat::from_wfx_mask(p);
        CoTaskMemFree(Some(p as *const _));
        match f {
            Some((f, m)) => (Some(f), m.map(|m| (f.channels, m))),
            None => (None, None),
        }
    }
}

fn exclusive_supported(client: &IAudioClient, f: StreamFormat, mask: u32) -> bool {
    let w = f.to_wfx_masked(mask);
    // SAFETY: w는 유효한 WAVEFORMATEXTENSIBLE이며 호출 동안 살아 있다. 독점 모드는 closest match를 받지 않는다.
    unsafe { client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &w.Format, None) }.0 == 0
}

/// 샘플 형식 탐색 순서: f32 → 32비트 정수 → 24-in-32 → 24비트 패킹 → 16비트.
fn sample_candidates() -> [(SampleKind, u16); 5] {
    [(SampleKind::F32, 32), (SampleKind::I32, 32), (SampleKind::I32, 24), (SampleKind::I24, 24), (SampleKind::I16, 16)]
}

/// 독점 지원 여부를 마스크 후보 순서로 확인하고, 받아들여진 마스크를 돌려준다.
fn exclusive_mask(client: &IAudioClient, f: StreamFormat, mix: Option<(u16, u32)>) -> Option<u32> {
    mask_candidates(f.channels, mix).into_iter().find(|&m| exclusive_supported(client, f, m))
}

fn channel_candidates(mix_channels: u16, min_channels: u16) -> Vec<u16> {
    let mut v = Vec::new();
    for c in [mix_channels, 2, min_channels] {
        let c = c.max(min_channels).max(1);
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

/// 독점 포맷 선택: 레이트(48k 우선) → 샘플 형식 → 채널 → 채널 마스크 순으로 처음 지원되는 것.
fn pick_exclusive_format(client: &IAudioClient, mix: Option<(u16, u32)>, mix_channels: u16, min_channels: u16) -> Option<(StreamFormat, u32)> {
    for rate in EXCLUSIVE_RATES {
        for (kind, valid_bits) in sample_candidates() {
            for channels in channel_candidates(mix_channels, min_channels) {
                let f = StreamFormat { kind, channels, rate, valid_bits };
                if let Some(m) = exclusive_mask(client, f, mix) {
                    return Some((f, m));
                }
            }
        }
    }
    None
}

fn describe_endpoint(dev: &IMMDevice, is_input: bool, default_id: Option<&str>) -> Endpoint {
    let id = endpoint_id(dev);
    let mut ep = Endpoint {
        is_default: default_id == Some(id.as_str()),
        endpoint_id: id,
        name: friendly_name(dev),
        is_input,
        mix: None,
        device_period_hns: (0, 0),
        shared3: None,
        exclusive_formats: Vec::new(),
    };
    if let Ok(client) = activate::<IAudioClient>(dev) {
        let (mix, mix_mask) = mix_format(&client);
        ep.mix = mix;
        let (mut d, mut m) = (0i64, 0i64);
        // SAFETY: 출력 포인터는 지역 변수.
        if unsafe { client.GetDevicePeriod(Some(&mut d), Some(&mut m)) }.is_ok() {
            ep.device_period_hns = (d, m);
        }
        let mix_ch = ep.mix.map(|f| f.channels).unwrap_or(2);
        for rate in LIST_RATES {
            for (kind, valid_bits) in sample_candidates() {
                for channels in channel_candidates(mix_ch, 1) {
                    let f = StreamFormat { kind, channels, rate, valid_bits };
                    if exclusive_mask(&client, f, mix_mask).is_some() {
                        ep.exclusive_formats.push(f);
                    }
                }
            }
        }
    }
    if let Ok(c3) = activate::<IAudioClient3>(dev) {
        // SAFETY: 믹스 포맷을 얻어 엔진 주기를 묻고 해제한다.
        unsafe {
            if let Ok(p) = c3.GetMixFormat() {
                let (mut a, mut b, mut c, mut e) = (0u32, 0u32, 0u32, 0u32);
                if c3.GetSharedModeEnginePeriod(p, &mut a, &mut b, &mut c, &mut e).is_ok() {
                    ep.shared3 = Some(Shared3Periods { default: a, fundamental: b, min: c, max: e });
                }
                CoTaskMemFree(Some(p as *const _));
            }
        }
    }
    ep
}

/// 모든 활성 엔드포인트를 조사한다(전용 COM 스레드에서 실행).
pub fn enumerate_endpoints() -> Result<Vec<Endpoint>, String> {
    std::thread::Builder::new()
        .name("band-wasapi-enum".into())
        .spawn(|| {
            let _com = ComGuard::init();
            let en = enumerator()?;
            let mut out = Vec::new();
            for is_input in [true, false] {
                let def = default_endpoint_id(&en, is_input);
                // SAFETY: 유효한 열거기와 컬렉션.
                let col = unsafe { en.EnumAudioEndpoints(flow(is_input), DEVICE_STATE_ACTIVE) }
                    .map_err(|e| err("장치 열거 실패", e))?;
                // SAFETY: 위와 같음.
                let n = unsafe { col.GetCount() }.unwrap_or(0);
                for i in 0..n {
                    // SAFETY: i < n.
                    if let Ok(d) = unsafe { col.Item(i) } {
                        out.push(describe_endpoint(&d, is_input, def.as_deref()));
                    }
                }
            }
            Ok(out)
        })
        .map_err(|e| format!("열거 스레드 생성 실패: {e}"))?
        .join()
        .map_err(|_| "열거 스레드 패닉".to_string())?
}

/// 엔드포인트 → 프론트 `DeviceInfo`(공유 1개 + 독점 지원 시 독점 1개). id는 cpal과 같은 `wasapi::<이름>`.
pub fn device_infos(eps: &[Endpoint]) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for ep in eps {
        let mix_rate = ep.mix.map(|f| f.rate).unwrap_or(48_000);
        let mix_ch = ep.mix.map(|f| f.channels).unwrap_or(2);
        out.push(DeviceInfo {
            id: device_id(DriverKind::WasapiShared, &ep.name),
            name: ep.name.clone(),
            driver: DriverKind::WasapiShared,
            is_input: ep.is_input,
            is_default: ep.is_default,
            channels: mix_ch,
            sample_rates: vec![mix_rate],
        });
        if !ep.exclusive_formats.is_empty() {
            let mut rates: Vec<u32> = ep.exclusive_formats.iter().map(|f| f.rate).collect();
            rates.sort_unstable();
            rates.dedup();
            out.push(DeviceInfo {
                id: device_id(DriverKind::WasapiExclusive, &ep.name),
                name: ep.name.clone(),
                driver: DriverKind::WasapiExclusive,
                is_input: ep.is_input,
                is_default: ep.is_default,
                channels: ep.exclusive_formats.iter().map(|f| f.channels).max().unwrap_or(mix_ch),
                sample_rates: rates,
            });
        }
    }
    out
}

// ============================================================================
// 한 방향 스트림
// ============================================================================

/// 열린 한 방향(캡처 또는 렌더) 스트림. 만든 스레드에서만 쓴다.
pub struct DirStream {
    client: IAudioClient,
    render: Option<IAudioRenderClient>,
    capture: Option<IAudioCaptureClient>,
    event: HANDLE,
    pub format: StreamFormat,
    pub mode: ShareMode,
    /// 장치 버퍼 크기(프레임).
    pub buffer_frames: u32,
    /// 이벤트 주기(프레임). 독점은 버퍼 크기와 같다.
    pub period_frames: u32,
    pub endpoint_id: String,
    pub name: String,
    started: bool,
}

impl Drop for DirStream {
    fn drop(&mut self) {
        // SAFETY: 이 스레드에서 만든 객체와 핸들이다.
        unsafe {
            if self.started {
                let _ = self.client.Stop();
            }
            let _ = CloseHandle(self.event);
        }
    }
}

impl DirStream {
    /// 엔드포인트를 지정한 모드로 연다. `min_channels`: 필요한 최소 장치 채널 수(입력 라우팅).
    pub fn open(en: &IMMDeviceEnumerator, is_input: bool, name: Option<&str>, mode: ShareMode, min_channels: u16) -> Result<Self, String> {
        let dev = find_device(en, is_input, name)?;
        let endpoint_id = endpoint_id(&dev);
        let dev_name = friendly_name(&dev);
        let (client, format, buffer_frames, period_frames) = match mode {
            ShareMode::Exclusive => open_exclusive(&dev, min_channels)?,
            ShareMode::Shared3 => open_shared3(&dev, min_channels)?,
        };
        // SAFETY: 자동 리셋 이벤트 생성과 등록. 핸들은 Drop에서 닫는다.
        let event = unsafe { CreateEventW(None, false, false, None) }.map_err(|e| err("이벤트 생성 실패", e))?;
        let mut s = DirStream {
            client,
            render: None,
            capture: None,
            event,
            format,
            mode,
            buffer_frames,
            period_frames,
            endpoint_id,
            name: dev_name,
            started: false,
        };
        // SAFETY: 초기화된 클라이언트에 이벤트를 등록하고 서비스 인터페이스를 얻는다.
        unsafe {
            s.client.SetEventHandle(s.event).map_err(|e| err("이벤트 등록 실패", e))?;
            if is_input {
                s.capture = Some(s.client.GetService::<IAudioCaptureClient>().map_err(|e| err("캡처 클라이언트 실패", e))?);
            } else {
                s.render = Some(s.client.GetService::<IAudioRenderClient>().map_err(|e| err("렌더 클라이언트 실패", e))?);
            }
        }
        Ok(s)
    }

    pub fn period_ms(&self) -> f64 {
        self.period_frames as f64 * 1000.0 / self.format.rate.max(1) as f64
    }

    /// 렌더: 시작 전 무음 한 주기(독점은 버퍼 전체)를 채워 첫 이벤트까지 끊기지 않게 한다.
    fn prefill(&self) -> windows::core::Result<()> {
        let Some(r) = &self.render else {
            return Ok(());
        };
        let n = match self.mode {
            ShareMode::Exclusive => self.buffer_frames,
            ShareMode::Shared3 => self.period_frames.min(self.buffer_frames),
        };
        // SAFETY: 시작 전 버퍼에 n ≤ 버퍼 크기 프레임을 무음으로 기록한다.
        unsafe {
            r.GetBuffer(n)?;
            r.ReleaseBuffer(n, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
        }
    }

    fn start(&mut self) -> Result<(), String> {
        self.prefill().map_err(|e| err("렌더 무음 prefill 실패", e))?;
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.Start() }.map_err(|e| err("스트림 시작 실패", e))?;
        self.started = true;
        Ok(())
    }

    /// 일시 오류가 반복될 때의 재시작: Stop → Reset → (렌더) 무음 prefill → Start.
    fn restart(&self) -> windows::core::Result<()> {
        // SAFETY: 이 스레드에서 연 초기화된 클라이언트.
        unsafe {
            let _ = self.client.Stop();
            self.client.Reset()?;
            self.prefill()?;
            self.client.Start()
        }
    }

    /// `GetStreamLatency`(ms). 실패하면 0.
    pub fn stream_latency_ms(&self) -> f64 {
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.GetStreamLatency() }.map(|h| h as f64 / 10_000.0).unwrap_or(0.0)
    }

    /// 상태 확인(이벤트가 멈췄을 때): 장치가 무효화됐으면 오류를 돌려준다.
    fn check(&self) -> windows::core::Result<()> {
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.GetCurrentPadding() }.map(|_| ())
    }
}

fn open_exclusive(dev: &IMMDevice, min_channels: u16) -> Result<(IAudioClient, StreamFormat, u32, u32), String> {
    let client: IAudioClient = activate(dev)?;
    let (mix, mix_mask) = mix_format(&client);
    let mix_ch = mix.map(|f| f.channels).unwrap_or(2);
    let (format, mask) = pick_exclusive_format(&client, mix_mask, mix_ch, min_channels)
        .ok_or_else(|| format!("독점 모드로 지원하는 포맷이 없습니다(필요 채널 {min_channels})"))?;
    let (mut def, mut min) = (0i64, 0i64);
    // SAFETY: 출력 포인터는 지역 변수.
    unsafe { client.GetDevicePeriod(Some(&mut def), Some(&mut min)) }.map_err(|e| err("장치 주기 조회 실패", e))?;
    let wfx = format.to_wfx_masked(mask);
    let init = |c: &IAudioClient, period: i64| {
        // SAFETY: wfx는 호출 동안 살아 있다. 이벤트 구동 독점 모드는 버퍼 길이 = 주기.
        unsafe { c.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, period, &wfx.Format, None) }
    };
    // 초기화에 실패한 클라이언트는 독점 핸들을 쥘 수 있어 먼저 해제하고 새로 활성화한다(L2, MS 예제 순서).
    let reinit = |old: IAudioClient, period: i64, what: &str| -> Result<IAudioClient, String> {
        drop(old);
        let c: IAudioClient = activate(dev)?;
        init(&c, period).map_err(|e| err(what, e))?;
        Ok(c)
    };
    let client = match init(&client, min) {
        Ok(()) => client,
        Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // 장치가 정렬된 크기를 알려 준다: 그 크기로 주기를 다시 계산하고 새 클라이언트로 재초기화.
            // SAFETY: 초기화 실패 후에도 GetBufferSize는 정렬된 크기를 돌려준다(문서화된 절차).
            let frames = unsafe { client.GetBufferSize() }.map_err(|e| err("정렬 버퍼 크기 조회 실패", e))?;
            let period = (HNS_PER_SEC * frames as f64 / format.rate as f64 + 0.5) as i64;
            reinit(client, period, "독점 모드 재초기화 실패")?
        }
        // 최소 주기를 거절하는 드라이버: 공유로 내려가기 전에 기본 주기로 한 번 더 독점을 시도한다(L2).
        Err(e) if (e.code() == AUDCLNT_E_INVALID_DEVICE_PERIOD || e.code() == AUDCLNT_E_ENDPOINT_CREATE_FAILED) && def > min => {
            log::warn!("독점 최소 주기 거절({}), 기본 주기로 재시도", err("Initialize", e));
            reinit(client, def, "독점 모드(기본 주기) 초기화 실패")?
        }
        Err(e) => return Err(err("독점 모드 초기화 실패", e)),
    };
    // SAFETY: 초기화된 클라이언트.
    let frames = unsafe { client.GetBufferSize() }.map_err(|e| err("버퍼 크기 조회 실패", e))?;
    Ok((client, format, frames, frames))
}

fn open_shared3(dev: &IMMDevice, min_channels: u16) -> Result<(IAudioClient, StreamFormat, u32, u32), String> {
    let c3: IAudioClient3 = activate(dev)?;
    // SAFETY: 믹스 포맷 포인터는 초기화가 끝날 때까지 쓰고 해제한다.
    unsafe {
        let p = c3.GetMixFormat().map_err(|e| err("믹스 포맷 조회 실패", e))?;
        let result = (|| {
            let format = StreamFormat::from_wfx(p).ok_or("믹스 포맷이 지원하지 않는 샘플 형식입니다")?;
            if format.channels < min_channels {
                return Err(format!("장치 채널 수({})가 선택한 입력 채널보다 적습니다", format.channels));
            }
            let (mut d, mut f, mut mn, mut mx) = (0u32, 0u32, 0u32, 0u32);
            c3.GetSharedModeEnginePeriod(p, &mut d, &mut f, &mut mn, &mut mx)
                .map_err(|e| err("공유 모드 엔진 주기 조회 실패", e))?;
            c3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, mn, p, None)
                .map_err(|e| err("저지연 공유 스트림 초기화 실패", e))?;
            let frames = c3.GetBufferSize().map_err(|e| err("버퍼 크기 조회 실패", e))?;
            Ok((format, frames, mn))
        })();
        CoTaskMemFree(Some(p as *const _));
        let (format, frames, period) = result?;
        let client: IAudioClient = c3.cast().map_err(|e| err("IAudioClient 변환 실패", e))?;
        Ok((client, format, frames, period))
    }
}

// ============================================================================
// 이벤트 루프
// ============================================================================

/// 장치 버퍼 뷰. 24비트 패킹은 루프가 f32로 변환해 넘기므로 여기에 없다.
pub enum DevBuf<'a> {
    F32(&'a mut [f32]),
    I32(&'a mut [i32]),
    I16(&'a mut [i16]),
}

impl DevBuf<'_> {
    /// # Safety
    /// `p`는 `frames × channels` 샘플을 담은 유효한 장치 버퍼여야 하고 반환값보다 오래 살아야 한다.
    /// `f.kind`는 `I24`가 아니어야 한다(호출자가 변환 경로로 보낸다).
    unsafe fn from_raw<'a>(p: *mut u8, frames: u32, f: &StreamFormat) -> DevBuf<'a> {
        let n = frames as usize * f.channels as usize;
        // SAFETY: 호출자 보장. WASAPI 버퍼는 샘플 정렬되어 있다.
        unsafe {
            match f.kind {
                SampleKind::F32 => DevBuf::F32(std::slice::from_raw_parts_mut(p as *mut f32, n)),
                SampleKind::I32 => DevBuf::I32(std::slice::from_raw_parts_mut(p as *mut i32, n)),
                SampleKind::I16 | SampleKind::I24 => DevBuf::I16(std::slice::from_raw_parts_mut(p as *mut i16, n)),
            }
        }
    }
}

/// 루프 종료 사유.
#[derive(Debug)]
pub enum LoopEnd {
    Stopped,
    /// 장치 분리·무효화(`deviceLost`).
    DeviceLost(windows::core::Error),
    /// 치명 오류 또는 재시도로 복구하지 못한 반복 오류(`streamFailed`).
    Failed(windows::core::Error),
}

/// 이벤트 대기 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    Timeout,
    /// 신호가 온 스트림 인덱스.
    Ready(usize),
    /// 대기 자체가 실패(핸들 무효).
    Failed,
}

/// 이벤트 루프가 쓰는 장치 I/O. 실제 구현은 `WasapiIo`, 테스트는 가짜 HRESULT 시퀀스.
pub trait LoopIo {
    fn streams(&self) -> usize;
    fn wait(&mut self, timeout_ms: u32) -> WaitResult;
    /// 신호가 온 스트림 처리(캡처: 쌓인 패킷 전부, 렌더: 한 주기).
    fn service(&mut self, idx: usize) -> windows::core::Result<()>;
    /// 이벤트가 멈췄을 때 스트림 상태 확인.
    fn check(&mut self, idx: usize) -> windows::core::Result<()>;
    /// Stop → Reset → prefill → Start.
    fn restart(&mut self, idx: usize) -> windows::core::Result<()>;
}

/// 방향별 연속 실패 상태.
#[derive(Default, Clone, Copy)]
struct FailState {
    /// 연속 실패(성공하면 0).
    fails: u32,
    /// 마지막 성공 이후 재시작 횟수.
    restarts: u32,
}

/// 실패 하나를 처리한다. 루프를 끝내야 하면 `Some`.
fn on_error(io: &mut impl LoopIo, idx: usize, e: windows::core::Error, st: &mut FailState, counters: &AudioCounters) -> Option<LoopEnd> {
    match classify(e.code()) {
        ErrClass::DeviceLost => return Some(LoopEnd::DeviceLost(e)),
        ErrClass::Fatal => return Some(LoopEnd::Failed(e)),
        ErrClass::Transient => {}
    }
    counters.stream_errors.fetch_add(1, Ordering::Relaxed);
    st.fails += 1;
    if st.fails >= FAIL_AFTER {
        return Some(LoopEnd::Failed(e));
    }
    if !st.fails.is_multiple_of(RESTART_AFTER) {
        // 다음 이벤트(다음 주기)에 다시 시도
        return None;
    }
    if st.restarts >= MAX_RESTARTS {
        return Some(LoopEnd::Failed(e));
    }
    st.restarts += 1;
    counters.stream_restarts.fetch_add(1, Ordering::Relaxed);
    if let Err(e2) = io.restart(idx) {
        match classify(e2.code()) {
            ErrClass::DeviceLost => return Some(LoopEnd::DeviceLost(e2)),
            ErrClass::Fatal => return Some(LoopEnd::Failed(e2)),
            ErrClass::Transient => {
                counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                st.fails += 1;
                if st.fails >= FAIL_AFTER {
                    return Some(LoopEnd::Failed(e2));
                }
            }
        }
    }
    None
}

/// 오류 정책 루프(H1). RT 경로: 할당·락·로그 없음(오류 생성 시 `windows::core::Error`만 예외).
pub fn drive_loop(io: &mut impl LoopIo, stop: &AtomicBool, counters: &AudioCounters) -> LoopEnd {
    let n = io.streams().min(2);
    if n == 0 {
        return LoopEnd::Stopped;
    }
    let mut st = [FailState::default(); 2];
    while !stop.load(Ordering::Relaxed) {
        match io.wait(WAIT_TIMEOUT_MS) {
            WaitResult::Timeout => {
                // 이벤트가 멈췄다: 장치 무효화를 드러내려고 상태를 확인한다(L1).
                for (i, s) in st.iter_mut().enumerate().take(n) {
                    if let Err(e) = io.check(i) {
                        if let Some(end) = on_error(io, i, e, s, counters) {
                            return end;
                        }
                    }
                }
            }
            WaitResult::Failed => return LoopEnd::Failed(windows::core::Error::from_thread()),
            WaitResult::Ready(i) => {
                let Some(s) = st.get_mut(i).filter(|_| i < n) else {
                    return LoopEnd::Failed(windows::core::Error::from_thread());
                };
                match io.service(i) {
                    Ok(()) => *s = FailState::default(),
                    Err(e) => {
                        if let Some(end) = on_error(io, i, e, s, counters) {
                            return end;
                        }
                    }
                }
            }
        }
    }
    LoopEnd::Stopped
}

/// 실제 WASAPI 이벤트 I/O. 인덱스 0이 캡처(있으면)라 둘 다 신호 상태면 캡처가 먼저 처리된다.
struct WasapiIo<'a, C, R> {
    streams: [Option<&'a DirStream>; 2],
    handles: [HANDLE; 2],
    n: usize,
    /// f32 변환 버퍼(무음 패킷의 0, 24비트 패킹 변환).
    scratch: &'a mut [f32],
    counters: &'a AudioCounters,
    /// 시작/재시작 후 첫 캡처 패킷 전(그 패킷의 DISCONTINUITY는 세지 않는다).
    fresh: [bool; 2],
    on_capture: C,
    on_render: R,
}

impl<C: FnMut(DevBuf<'_>), R: FnMut(DevBuf<'_>) -> bool> LoopIo for WasapiIo<'_, C, R> {
    fn streams(&self) -> usize {
        self.n
    }

    fn wait(&mut self, timeout_ms: u32) -> WaitResult {
        // SAFETY: 유효한 이벤트 핸들.
        let w = unsafe { WaitForMultipleObjects(&self.handles[..self.n], false, timeout_ms) };
        if w == WAIT_TIMEOUT {
            return WaitResult::Timeout;
        }
        let idx = w.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if idx < self.n {
            WaitResult::Ready(idx)
        } else {
            WaitResult::Failed
        }
    }

    fn service(&mut self, idx: usize) -> windows::core::Result<()> {
        let Some(Some(s)) = self.streams.get(idx).copied() else {
            return Ok(());
        };
        if s.capture.is_some() {
            let fresh = &mut self.fresh[idx];
            service_capture(s, self.scratch, self.counters, fresh, &mut self.on_capture)
        } else {
            service_render(s, self.scratch, &mut self.on_render)
        }
    }

    fn check(&mut self, idx: usize) -> windows::core::Result<()> {
        match self.streams.get(idx).copied().flatten() {
            Some(s) => s.check(),
            None => Ok(()),
        }
    }

    fn restart(&mut self, idx: usize) -> windows::core::Result<()> {
        let Some(Some(s)) = self.streams.get(idx).copied() else {
            return Ok(());
        };
        if let Some(f) = self.fresh.get_mut(idx) {
            *f = true;
        }
        s.restart()
    }
}

/// 입·출력 이벤트 루프. `on_capture(buf)`, `on_render(buf) -> 무음 플래그`.
/// RT 경로: 할당·락·로그 없음(오류는 반환값으로). `scratch`는 미리 할당한 f32 버퍼.
pub fn run_loop(
    input: Option<&DirStream>,
    output: Option<&DirStream>,
    stop: &AtomicBool,
    counters: &AudioCounters,
    scratch: &mut [f32],
    on_capture: impl FnMut(DevBuf<'_>),
    on_render: impl FnMut(DevBuf<'_>) -> bool,
) -> LoopEnd {
    let mut io = WasapiIo {
        streams: [None, None],
        handles: [HANDLE::default(); 2],
        n: 0,
        scratch,
        counters,
        fresh: [true; 2],
        on_capture,
        on_render,
    };
    for s in [input, output].into_iter().flatten() {
        io.streams[io.n] = Some(s);
        io.handles[io.n] = s.event;
        io.n += 1;
    }
    drive_loop(&mut io, stop, counters)
}

/// `scratch`를 채널 배수 크기로 자른 길이(한 번에 넘길 수 있는 샘플 수).
fn chunk_samples(scratch_len: usize, ch: usize) -> usize {
    (scratch_len / ch.max(1) * ch.max(1)).max(ch.max(1))
}

fn service_capture(
    s: &DirStream,
    scratch: &mut [f32],
    counters: &AudioCounters,
    fresh: &mut bool,
    on_capture: &mut impl FnMut(DevBuf<'_>),
) -> windows::core::Result<()> {
    let Some(cap) = &s.capture else {
        return Ok(());
    };
    let ch = s.format.channels as usize;
    let per = chunk_samples(scratch.len(), ch);
    // SAFETY: 이 스레드에서 연 캡처 클라이언트. GetBuffer로 받은 포인터는 ReleaseBuffer 전까지만 쓴다.
    unsafe {
        loop {
            if cap.GetNextPacketSize()? == 0 {
                return Ok(());
            }
            let mut p: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            cap.GetBuffer(&mut p, &mut frames, &mut flags, None, None)?;
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && !*fresh {
                // 장치가 데이터를 버렸다(이 스레드가 한 주기 넘게 늦음) = 입력 오버런(M2)
                counters.input_discontinuities.fetch_add(1, Ordering::Relaxed);
                counters.xruns.fetch_add(1, Ordering::Relaxed);
            }
            *fresh = false;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent {
                counters.input_silent_packets.fetch_add(1, Ordering::Relaxed);
            }
            let samples = frames as usize * ch;
            if silent || p.is_null() {
                // 장치가 무음이라고 표시한 패킷: 버퍼 내용은 의미 없으므로 0을 넘긴다. 변환 버퍼보다 크면
                // 나눠 넘겨 입력 프레임 수를 보존한다(L6).
                let mut left = samples;
                while left > 0 {
                    let k = left.min(per);
                    let Some(z) = scratch.get_mut(..k) else {
                        break;
                    };
                    z.fill(0.0);
                    on_capture(DevBuf::F32(z));
                    left -= k;
                }
            } else if s.format.kind == SampleKind::I24 {
                let bytes = std::slice::from_raw_parts(p as *const u8, samples * 3);
                for part in bytes.chunks(per * 3) {
                    let m = i24_to_f32(part, scratch);
                    if let Some(z) = scratch.get_mut(..m) {
                        on_capture(DevBuf::F32(z));
                    }
                }
            } else if samples > 0 {
                on_capture(DevBuf::from_raw(p, frames, &s.format));
            }
            cap.ReleaseBuffer(frames)?;
        }
    }
}

fn service_render(s: &DirStream, scratch: &mut [f32], on_render: &mut impl FnMut(DevBuf<'_>) -> bool) -> windows::core::Result<()> {
    let Some(r) = &s.render else {
        return Ok(());
    };
    // SAFETY: 이 스레드에서 연 렌더 클라이언트. GetBuffer로 받은 포인터는 ReleaseBuffer 전까지만 쓴다.
    unsafe {
        let frames = match s.mode {
            // 독점 이벤트 모드: 이벤트마다 버퍼 전체(= 한 주기)를 채운다.
            ShareMode::Exclusive => s.buffer_frames,
            // 공유: 빈 만큼, 단 한 주기까지만(장치 큐를 한 주기 근처로 유지해 지연을 늘리지 않음).
            ShareMode::Shared3 => {
                let pad = s.client.GetCurrentPadding()?;
                s.buffer_frames.saturating_sub(pad).min(s.period_frames)
            }
        };
        if frames == 0 {
            return Ok(());
        }
        let p = r.GetBuffer(frames)?;
        let silent = if s.format.kind == SampleKind::I24 {
            // f32로 섞은 뒤 3바이트로 패킹. 보통 한 번에 들어간다(변환 버퍼 ≥ 50ms × 8ch).
            let ch = s.format.channels as usize;
            let samples = frames as usize * ch;
            let bytes = std::slice::from_raw_parts_mut(p, samples * 3);
            let per = chunk_samples(scratch.len(), ch);
            let mut all_silent = true;
            for part in bytes.chunks_mut(per * 3) {
                let m = (part.len() / 3).min(scratch.len());
                let Some(z) = scratch.get_mut(..m) else {
                    break;
                };
                all_silent &= on_render(DevBuf::F32(z));
                f32_to_i24(z, part);
            }
            all_silent
        } else {
            on_render(DevBuf::from_raw(p, frames, &s.format))
        };
        let flags = if silent { AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 } else { 0 };
        r.ReleaseBuffer(frames, flags)?;
    }
    Ok(())
}

fn feed_input(rt: &mut InputRt, buf: DevBuf<'_>, ch: usize) {
    match buf {
        DevBuf::F32(b) => rt.process::<f32>(b, ch),
        DevBuf::I32(b) => rt.process::<i32>(b, ch),
        DevBuf::I16(b) => rt.process::<i16>(b, ch),
    }
}

fn fill_output(rt: &mut OutputRt, buf: DevBuf<'_>, ch: usize) {
    match buf {
        DevBuf::F32(b) => rt.process::<f32>(b, ch),
        DevBuf::I32(b) => rt.process::<i32>(b, ch),
        DevBuf::I16(b) => rt.process::<i16>(b, ch),
    }
}

// ============================================================================
// 엔진용 열기
// ============================================================================

/// 열기 실패. RT 상태를 돌려받으면 다른 백엔드로 다시 시도할 수 있다.
pub struct OpenFailure {
    pub message: String,
    pub rts: Option<Box<(InputRt, OutputRt)>>,
}

struct WasapiGuard {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for WasapiGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// 입·출력을 각 모드로 연다. 성공하면 재생이 시작된 상태로 돌아온다.
pub fn open_engine_streams(
    cfg: &EngineConfig,
    modes: (ShareMode, ShareMode),
    input: InputRt,
    output: OutputRt,
    counters: Arc<AudioCounters>,
) -> Result<(StreamInfo, Box<dyn Any>), OpenFailure> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<(StreamInfo, [Option<String>; 2], String), OpenFailure>>();
    let in_name = cfg.input_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let out_name = cfg.output_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let buffer_frames = cfg.buffer_size;
    let st = stop.clone();
    let c = counters.clone();
    let spawned = std::thread::Builder::new().name("band-wasapi".into()).spawn(move || {
        let _com = ComGuard::init();
        // MMCSS 등록과 변환 버퍼 할당은 스트림 시작 전에 끝낸다. 독점 2~3ms 주기에서는 시작 뒤에 하면
        // 첫 이벤트를 놓쳐 시작 글리치가 난다(L5).
        let prio = super::prio::promote_current_thread();
        c.audio_thread.store(prio.status());
        let mut scratch = vec![0f32; SCRATCH_SAMPLES];
        let (mut input, mut output) = (input, output);
        let opened = (|| {
            let en = enumerator()?;
            let min_in = input.required_device_channels() as u16;
            let mut i = DirStream::open(&en, true, in_name.as_deref(), modes.0, min_in)?;
            let mut o = DirStream::open(&en, false, out_name.as_deref(), modes.1, 1)?;
            // 열기가 시간 초과로 포기됐으면 시작하지 않는다(L4).
            if st.load(Ordering::SeqCst) {
                return Err("열기 시간 초과 후 취소".to_string());
            }
            o.start()?;
            i.start()?;
            Ok::<_, String>((i, o))
        })();
        let (i, o) = match opened {
            Ok(x) => x,
            Err(message) => {
                // 이 스레드는 곧 끝나고 MMCSS 등록도 풀린다. 다음 시도(다른 모드 또는 cpal 폴백)가 다시 적기 전까지
                // 오디오 스레드는 우리가 관리하지 않는 상태다(리뷰 M1). send보다 먼저 적어 받는 쪽이 이 값을 본다.
                c.audio_thread.store(super::prio::ThreadPriority::Unmanaged);
                let _ = tx.send(Err(OpenFailure { message, rts: Some(Box::new((input, output))) }));
                return;
            }
        };
        let info = StreamInfo {
            in_rate: i.format.rate,
            out_rate: o.format.rate,
            in_channels: i.format.channels as usize,
            out_channels: o.format.channels as usize,
            buffer_frames,
            in_backend: modes.0.backend(),
            out_backend: modes.1.backend(),
            in_period_frames: i.period_frames,
            out_period_frames: o.period_frames,
            in_reported_latency_frames: (i.stream_latency_ms() * i.format.rate as f64 / 1000.0).round() as u32,
            out_reported_latency_frames: (o.stream_latency_ms() * o.format.rate as f64 / 1000.0).round() as u32,
            in_device_tag: device_tag(&i.endpoint_id),
            out_device_tag: device_tag(&o.endpoint_id),
        };
        // 기본 장치로 연 방향은 감시한다(기본 장치가 바뀌면 deviceLost).
        let watch = [
            in_name.is_none().then(|| i.endpoint_id.clone()),
            out_name.is_none().then(|| o.endpoint_id.clone()),
        ];
        // 로그는 받는 쪽(비RT)이 남긴다(L5).
        let desc = format!(
            "WASAPI 열림: in {} {:?} {:?} 주기 {}프레임({:.2}ms), out {} {:?} {:?} 주기 {}프레임({:.2}ms) 버퍼 {}",
            i.name,
            modes.0,
            i.format,
            i.period_frames,
            i.period_ms(),
            o.name,
            modes.1,
            o.format,
            o.period_frames,
            o.period_ms(),
            o.buffer_frames
        );
        let _ = tx.send(Ok((info, watch, desc)));
        let (ich, och) = (i.format.channels as usize, o.format.channels as usize);
        let end = run_loop(
            Some(&i),
            Some(&o),
            &st,
            &c,
            &mut scratch,
            |b| feed_input(&mut input, b, ich),
            |b| {
                fill_output(&mut output, b, och);
                false
            },
        );
        match end {
            LoopEnd::Stopped => {}
            LoopEnd::DeviceLost(e) => {
                c.stream_errors.fetch_add(1, Ordering::Relaxed);
                c.device_lost.store(true, Ordering::SeqCst);
                log::error!("WASAPI 장치 분실(분리/무효화): {e}");
            }
            LoopEnd::Failed(e) => {
                c.stream_errors.fetch_add(1, Ordering::Relaxed);
                c.stream_failed.store(true, Ordering::SeqCst);
                log::error!("WASAPI 스트림 복구 실패(재시작 반복 실패 또는 치명 오류): {e}");
            }
        }
    });
    let audio = match spawned {
        Ok(h) => h,
        Err(e) => return Err(OpenFailure { message: format!("WASAPI 스레드 생성 실패: {e}"), rts: None }),
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok((info, watch, desc))) => {
            log::info!("{desc}");
            let mut threads = vec![audio];
            if watch.iter().any(|w| w.is_some()) {
                let st = stop.clone();
                let c = counters;
                if let Ok(h) = std::thread::Builder::new().name("band-wasapi-watch".into()).spawn(move || watch_default(watch, &st, &c)) {
                    threads.push(h);
                }
            }
            Ok((info, Box::new(WasapiGuard { stop, threads })))
        }
        Ok(Err(f)) => {
            let _ = audio.join();
            Err(f)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err(OpenFailure { message: "WASAPI 장치 열기 시간 초과".into(), rts: None })
        }
    }
}

/// 기본 장치 감시(비RT 스레드). 기본 장치로 연 방향의 기본 장치가 바뀌면 `device_lost`.
fn watch_default(watch: [Option<String>; 2], stop: &AtomicBool, counters: &AudioCounters) {
    let _com = ComGuard::init();
    let Ok(en) = enumerator() else {
        return;
    };
    let mut last_check = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        if last_check.elapsed() < Duration::from_millis(500) {
            continue;
        }
        last_check = Instant::now();
        for (is_input, opened) in [(true, &watch[0]), (false, &watch[1])] {
            let Some(opened) = opened else {
                continue;
            };
            let now = default_endpoint_id(&en, is_input);
            if now.as_deref() != Some(opened.as_str()) && !counters.device_lost.swap(true, Ordering::SeqCst) {
                log::warn!("기본 {} 장치가 바뀌었습니다: 엔진 재시작 필요", if is_input { "입력" } else { "출력" });
            }
        }
    }
}

// ============================================================================
// 스모크 테스트(examples/wasapi_probe.rs)
// ============================================================================

/// 한 방향 스트림을 열어 `seconds` 동안 돌리며 콜백 간격을 잰다. 출력은 무음(SILENT 플래그)이다.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub name: String,
    pub format: StreamFormat,
    pub mode: ShareMode,
    pub period_frames: u32,
    pub buffer_frames: u32,
    /// 콜백 간격(µs).
    pub intervals_us: Vec<u32>,
    pub frames: u64,
    pub error: Option<String>,
    /// `IAudioClient::GetStreamLatency`(ms): 드라이버/코덱 FIFO 등 장치 주기 밖의 지연(estLatencyMs에 없음).
    pub stream_latency_ms: f64,
    /// 캡처 DATA_DISCONTINUITY(시작 직후 첫 패킷 제외), SILENT 패킷, 일시 오류, 재시작 횟수.
    pub discontinuities: u64,
    pub silent_packets: u64,
    pub stream_errors: u32,
    pub restarts: u32,
}

pub fn probe_stream(is_input: bool, device: Option<&str>, mode: ShareMode, seconds: f64) -> Result<ProbeReport, String> {
    let device = device.map(|s| s.to_string());
    std::thread::Builder::new()
        .name("band-wasapi-probe".into())
        .spawn(move || {
            let _com = ComGuard::init();
            let en = enumerator()?;
            let mut s = DirStream::open(&en, is_input, device.as_deref(), mode, 1)?;
            s.start()?;
            let _prio = super::prio::promote_current_thread();
            let stop = AtomicBool::new(false);
            let ch = s.format.channels as usize;
            // (마지막 콜백 시각, 간격 목록, 프레임 수). 프로브는 RT 규칙 대상이 아니다(측정 도구).
            let state = std::cell::RefCell::new((None::<Instant>, Vec::with_capacity((seconds * 2000.0) as usize + 16), 0u64));
            let t0 = Instant::now();
            let tick = |n: usize| {
                let mut st = state.borrow_mut();
                let now = Instant::now();
                if let Some(l) = st.0 {
                    st.1.push((now - l).as_micros() as u32);
                }
                st.0 = Some(now);
                st.2 += (n / ch.max(1)) as u64;
                if t0.elapsed().as_secs_f64() >= seconds {
                    stop.store(true, Ordering::Relaxed);
                }
            };
            let len = |b: &DevBuf<'_>| match b {
                DevBuf::F32(x) => x.len(),
                DevBuf::I32(x) => x.len(),
                DevBuf::I16(x) => x.len(),
            };
            let (input, output) = if is_input { (Some(&s), None) } else { (None, Some(&s)) };
            let mut scratch = vec![0f32; SCRATCH_SAMPLES];
            let counters = AudioCounters::default();
            let end = run_loop(input, output, &stop, &counters, &mut scratch, |b| tick(len(&b)), |mut b| {
                // 무음: 내용을 0으로 채우고 SILENT 플래그도 켠다(소리 내지 않음).
                match &mut b {
                    DevBuf::F32(x) => x.fill(0.0),
                    DevBuf::I32(x) => x.fill(0),
                    DevBuf::I16(x) => x.fill(0),
                }
                tick(len(&b));
                true
            });
            let (_, intervals, frames) = state.into_inner();
            Ok(ProbeReport {
                name: s.name.clone(),
                format: s.format,
                mode,
                period_frames: s.period_frames,
                buffer_frames: s.buffer_frames,
                intervals_us: intervals,
                frames,
                error: match end {
                    LoopEnd::Stopped => None,
                    LoopEnd::DeviceLost(e) => Some(format!("장치 분실: {e}")),
                    LoopEnd::Failed(e) => Some(format!("복구 실패: {e}")),
                },
                stream_latency_ms: s.stream_latency_ms(),
                discontinuities: counters.input_discontinuities.load(Ordering::Relaxed),
                silent_packets: counters.input_silent_packets.load(Ordering::Relaxed),
                stream_errors: counters.stream_errors.load(Ordering::Relaxed),
                restarts: counters.stream_restarts.load(Ordering::Relaxed),
            })
        })
        .map_err(|e| format!("프로브 스레드 생성 실패: {e}"))?
        .join()
        .map_err(|_| "프로브 스레드 패닉".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_roundtrip() {
        for f in [
            StreamFormat { kind: SampleKind::F32, channels: 2, rate: 48_000, valid_bits: 32 },
            StreamFormat { kind: SampleKind::I32, channels: 8, rate: 44_100, valid_bits: 24 },
            StreamFormat { kind: SampleKind::I16, channels: 1, rate: 96_000, valid_bits: 16 },
            StreamFormat { kind: SampleKind::I24, channels: 2, rate: 48_000, valid_bits: 24 },
        ] {
            let w = f.to_wfx();
            // SAFETY: 지역 WAVEFORMATEXTENSIBLE을 가리킨다.
            let back = unsafe { StreamFormat::from_wfx(&w.Format) }.unwrap();
            assert_eq!(back, f);
            let block = w.Format.nBlockAlign;
            assert_eq!(block as u32 * f.rate, { w.Format.nAvgBytesPerSec });
        }
    }

    #[test]
    fn plain_waveformatex_parsed() {
        let w = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM,
            nChannels: 2,
            nSamplesPerSec: 44_100,
            nAvgBytesPerSec: 44_100 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: 지역 구조체.
        let f = unsafe { StreamFormat::from_wfx(&w) }.unwrap();
        assert_eq!(f.kind, SampleKind::I16);
        // 24비트 패킹(3바이트 서브슬롯)은 I24로 다룬다(M1)
        let w24 = WAVEFORMATEX { wBitsPerSample: 24, nBlockAlign: 6, nAvgBytesPerSec: 44_100 * 6, ..w };
        // SAFETY: 지역 구조체.
        let f24 = unsafe { StreamFormat::from_wfx(&w24) }.unwrap();
        assert_eq!((f24.kind, f24.valid_bits), (SampleKind::I24, 24));
        // 블록 정렬이 컨테이너 × 채널과 다르면(비표준 패딩) 거절
        let odd = WAVEFORMATEX { wBitsPerSample: 24, nBlockAlign: 8, ..w };
        // SAFETY: 지역 구조체.
        assert!(unsafe { StreamFormat::from_wfx(&odd) }.is_none());
    }

    #[test]
    fn extensible_24_in_32_and_packed_24_parse_with_mask() {
        // USB 인터페이스(Focusrite Scarlett, Behringer UMC 계열)가 독점으로 내는 두 형식:
        // 24-in-32(I32, 유효 24)와 24 패킹(I24). 다채널은 DIRECTOUT 마스크.
        for (f, mask) in [
            (StreamFormat { kind: SampleKind::I32, channels: 2, rate: 48_000, valid_bits: 24 }, SPEAKER_STEREO),
            (StreamFormat { kind: SampleKind::I24, channels: 2, rate: 48_000, valid_bits: 24 }, SPEAKER_STEREO),
            (StreamFormat { kind: SampleKind::I24, channels: 4, rate: 96_000, valid_bits: 24 }, SPEAKER_DIRECTOUT),
            (StreamFormat { kind: SampleKind::I24, channels: 1, rate: 44_100, valid_bits: 20 }, SPEAKER_FRONT_CENTER),
        ] {
            let w = f.to_wfx_masked(mask);
            assert_eq!({ w.Format.nBlockAlign }, f.container_bytes() * f.channels);
            assert_eq!({ w.Format.wBitsPerSample }, f.container_bytes() * 8);
            // SAFETY: 지역 WAVEFORMATEXTENSIBLE.
            let (back, m) = unsafe { StreamFormat::from_wfx_mask(&w.Format) }.unwrap();
            assert_eq!(back, f);
            assert_eq!(m, Some(mask));
        }
    }

    #[test]
    fn i24_conversion_known_values() {
        let samples = [0.0f32, 0.5, -0.5, 1.0, -1.0, 1.5, -1.5, 1.0 / 8_388_608.0];
        let mut bytes = [0u8; 8 * 3];
        assert_eq!(f32_to_i24(&samples, &mut bytes), 8);
        let get = |i: usize| -> [u8; 3] { [bytes[i * 3], bytes[i * 3 + 1], bytes[i * 3 + 2]] };
        assert_eq!(get(0), [0, 0, 0]);
        assert_eq!(get(1), [0x00, 0x00, 0x40]); // 0x400000
        assert_eq!(get(2), [0x00, 0x00, 0xC0]); // -0x400000
        assert_eq!(get(3), [0xFF, 0xFF, 0x7F]); // 최대
        assert_eq!(get(4), [0x00, 0x00, 0x80]); // 최소
        assert_eq!(get(5), [0xFF, 0xFF, 0x7F]); // 클립
        assert_eq!(get(6), [0x00, 0x00, 0x80]);
        assert_eq!(get(7), [0x01, 0x00, 0x00]); // 1 LSB
        let mut back = [9.0f32; 8];
        assert_eq!(i24_to_f32(&bytes, &mut back), 8);
        assert_eq!(back[0], 0.0);
        assert_eq!(back[1], 0.5);
        assert_eq!(back[2], -0.5);
        assert!((back[3] - 1.0).abs() < 2e-7);
        assert_eq!(back[4], -1.0);
        assert_eq!(back[7], 1.0 / 8_388_608.0);
        // 부호 확장: 0xFFFFFF = -1 LSB
        let mut one = [0f32; 1];
        i24_to_f32(&[0xFF, 0xFF, 0xFF], &mut one);
        assert_eq!(one[0], -1.0 / 8_388_608.0);
    }

    #[test]
    fn i24_roundtrip_and_partial_buffers() {
        // 왕복 오차 ≤ 0.5 LSB, 짧은 쪽 길이만큼만 변환
        let src: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.137).sin() * 0.9).collect();
        let mut bytes = vec![0u8; src.len() * 3];
        assert_eq!(f32_to_i24(&src, &mut bytes), src.len());
        let mut back = vec![0f32; src.len()];
        assert_eq!(i24_to_f32(&bytes, &mut back), src.len());
        for (a, b) in src.iter().zip(&back) {
            assert!((a - b).abs() <= 0.5 / 8_388_608.0 + 1e-9, "{a} vs {b}");
        }
        let mut small = [0f32; 10];
        assert_eq!(i24_to_f32(&bytes, &mut small), 10);
        assert_eq!(i24_to_f32(&bytes[..7], &mut back), 2); // 남는 1바이트는 무시
        assert_eq!(f32_to_i24(&src, &mut bytes[..5]), 1);
    }

    #[test]
    fn i24_matches_24_in_32_scale() {
        // 24-in-32(상위 정렬)와 24 패킹이 같은 레벨로 들어온다: 같은 24비트 값 → 같은 f32
        use crate::engine::rt::RtSample;
        for v in [1i32, -1, 0x123456, -0x123456, 0x7FFFFF, -0x800000] {
            let le = v.to_le_bytes();
            let mut f = [0f32; 1];
            i24_to_f32(&le[..3], &mut f);
            assert_eq!(f[0], (v << 8).to_f32(), "{v:#x}");
        }
    }

    #[test]
    fn chunk_samples_is_channel_multiple() {
        assert_eq!(chunk_samples(SCRATCH_SAMPLES, 2), SCRATCH_SAMPLES);
        assert_eq!(chunk_samples(10, 3), 9);
        assert_eq!(chunk_samples(2, 4), 4); // 최소 한 프레임(호출부가 get_mut으로 거른다)
    }

    #[test]
    fn mask_candidates_order() {
        // 스테레오: 믹스 마스크가 같으면 하나로, DIRECTOUT은 마지막
        assert_eq!(mask_candidates(2, Some((2, SPEAKER_STEREO))), vec![SPEAKER_STEREO, SPEAKER_DIRECTOUT]);
        assert_eq!(mask_candidates(1, None), vec![SPEAKER_FRONT_CENTER, SPEAKER_DIRECTOUT]);
        // 다채널 인터페이스: 믹스 마스크(드라이버 자기 배치) → DIRECTOUT → 하위 n비트
        assert_eq!(mask_candidates(4, Some((4, 0x33))), vec![0x33, SPEAKER_DIRECTOUT, 0xF]);
        assert_eq!(mask_candidates(4, None), vec![SPEAKER_DIRECTOUT, 0xF]);
        // 채널 수가 다른 믹스 마스크는 쓰지 않는다
        assert_eq!(mask_candidates(2, Some((8, 0x63F))), vec![SPEAKER_STEREO, SPEAKER_DIRECTOUT]);
        assert_eq!(default_mask(32), u32::MAX);
    }

    #[test]
    fn fallback_order_keeps_one_exclusive_direction() {
        use ShareMode::{Exclusive as E, Shared3 as S};
        assert_eq!(fallback_modes(E, E), vec![(E, E), (E, S), (S, E), (S, S)]);
        assert_eq!(fallback_modes(E, S), vec![(E, S), (S, S)]);
        assert_eq!(fallback_modes(S, E), vec![(S, E), (S, S)]);
        assert_eq!(fallback_modes(S, S), vec![(S, S)]);
    }

    // ------------------------------------------------------------------------
    // 오류 정책(H1): 가짜 HRESULT 시퀀스 주입
    // ------------------------------------------------------------------------

    use std::collections::VecDeque;
    use windows::Win32::Media::Audio::{AUDCLNT_E_BUFFER_ERROR, AUDCLNT_E_BUFFER_TOO_LARGE};

    const OK: HRESULT = HRESULT(0);

    #[derive(Clone, Copy)]
    enum Step {
        /// 스트림 i 이벤트, 서비스 결과
        Ev(usize, HRESULT),
        /// 대기 타임아웃, 상태 확인 결과
        Timeout(HRESULT),
    }

    struct FakeIo<'a> {
        n: usize,
        script: VecDeque<Step>,
        restart_results: VecDeque<HRESULT>,
        restarts: Vec<usize>,
        serviced: usize,
        stop: &'a AtomicBool,
        pending: HRESULT,
    }

    impl<'a> FakeIo<'a> {
        fn new(n: usize, script: impl IntoIterator<Item = Step>, stop: &'a AtomicBool) -> Self {
            Self { n, script: script.into_iter().collect(), restart_results: VecDeque::new(), restarts: Vec::new(), serviced: 0, stop, pending: OK }
        }
    }

    impl LoopIo for FakeIo<'_> {
        fn streams(&self) -> usize {
            self.n
        }
        fn wait(&mut self, _ms: u32) -> WaitResult {
            match self.script.pop_front() {
                Some(Step::Ev(i, hr)) => {
                    self.pending = hr;
                    WaitResult::Ready(i)
                }
                Some(Step::Timeout(hr)) => {
                    self.pending = hr;
                    WaitResult::Timeout
                }
                None => {
                    self.stop.store(true, Ordering::SeqCst);
                    self.pending = OK;
                    WaitResult::Timeout
                }
            }
        }
        fn service(&mut self, _idx: usize) -> windows::core::Result<()> {
            self.serviced += 1;
            self.pending.ok()
        }
        fn check(&mut self, _idx: usize) -> windows::core::Result<()> {
            self.pending.ok()
        }
        fn restart(&mut self, idx: usize) -> windows::core::Result<()> {
            self.restarts.push(idx);
            self.restart_results.pop_front().unwrap_or(OK).ok()
        }
    }

    fn run(io: &mut FakeIo<'_>) -> (LoopEnd, u32, u32) {
        let c = AudioCounters::default();
        let stop = io.stop;
        let end = drive_loop(io, stop, &c);
        (end, c.stream_errors.load(Ordering::Relaxed), c.stream_restarts.load(Ordering::Relaxed))
    }

    fn ev(i: usize, hr: HRESULT, n: usize) -> impl Iterator<Item = Step> {
        std::iter::repeat_n(Step::Ev(i, hr), n)
    }

    #[test]
    fn classify_hresults() {
        assert_eq!(classify(AUDCLNT_E_DEVICE_INVALIDATED), ErrClass::DeviceLost);
        assert_eq!(classify(AUDCLNT_E_SERVICE_NOT_RUNNING), ErrClass::DeviceLost);
        assert_eq!(classify(AUDCLNT_E_RESOURCES_INVALIDATED), ErrClass::DeviceLost);
        assert_eq!(classify(E_OUTOFMEMORY), ErrClass::Fatal);
        assert_eq!(classify(AUDCLNT_E_BUFFER_ERROR), ErrClass::Transient);
        assert_eq!(classify(AUDCLNT_E_BUFFER_TOO_LARGE), ErrClass::Transient);
    }

    #[test]
    fn single_transient_error_keeps_running() {
        // 독점 캡처의 AUDCLNT_E_BUFFER_ERROR 한 번(DPC 스파이크): 이전에는 여기서 루프가 끝나 deviceLost였다.
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 100)
            .chain([Step::Ev(0, AUDCLNT_E_BUFFER_ERROR)])
            .chain(ev(0, OK, 100))
            .chain([Step::Ev(1, AUDCLNT_E_BUFFER_TOO_LARGE)])
            .chain(ev(1, OK, 10));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!((errors, restarts), (2, 0));
        assert_eq!(io.serviced, 212, "오류 뒤에도 계속 서비스");
    }

    #[test]
    fn sporadic_errors_never_escalate() {
        // 연속 (RESTART_AFTER-1)회 실패 후 성공을 200번 반복: 재시작도 치명도 없다(성공하면 연속 횟수 초기화).
        let stop = AtomicBool::new(false);
        let mut script = Vec::new();
        for _ in 0..200 {
            script.extend(ev(0, AUDCLNT_E_BUFFER_ERROR, (RESTART_AFTER - 1) as usize));
            script.push(Step::Ev(0, OK));
        }
        let mut io = FakeIo::new(1, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!(errors, 200 * (RESTART_AFTER - 1));
        assert_eq!(restarts, 0);
    }

    #[test]
    fn repeated_errors_restart_stream_then_recover() {
        let stop = AtomicBool::new(false);
        let script = ev(1, OK, 5).chain(ev(1, AUDCLNT_E_BUFFER_ERROR, RESTART_AFTER as usize)).chain(ev(1, OK, 50));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!((errors, restarts), (RESTART_AFTER, 1));
        assert_eq!(io.restarts, vec![1], "실패한 방향만 재시작");
    }

    #[test]
    fn other_direction_success_does_not_reset_failures() {
        // 캡처(0)가 계속 실패하는 동안 렌더(1)는 성공: 캡처 쪽 연속 실패는 그대로 쌓여 캡처만 재시작된다.
        let stop = AtomicBool::new(false);
        let mut script = Vec::new();
        for _ in 0..RESTART_AFTER {
            script.push(Step::Ev(0, AUDCLNT_E_BUFFER_ERROR));
            script.push(Step::Ev(1, OK));
        }
        let mut io = FakeIo::new(2, script, &stop);
        let (end, _, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!(restarts, 1);
        assert_eq!(io.restarts, vec![0]);
    }

    #[test]
    fn persistent_errors_become_fatal_after_restarts() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 1000), &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(&end, LoopEnd::Failed(e) if e.code() == AUDCLNT_E_BUFFER_ERROR), "{end:?}");
        assert_eq!(restarts, MAX_RESTARTS);
        assert_eq!(errors, RESTART_AFTER * (MAX_RESTARTS + 1));
        assert!(io.serviced < 100, "반복 실패는 오래 끌지 않는다: {}", io.serviced);
    }

    #[test]
    fn failing_restarts_count_toward_limit() {
        // 재시작 자체가 일시 오류로 실패해도 연속 실패에 더해져 결국 치명으로 끝난다(무한 루프 없음).
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 1000), &stop);
        io.restart_results = std::iter::repeat_n(AUDCLNT_E_BUFFER_ERROR, 100).collect();
        let (end, _, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::Failed(_)), "{end:?}");
        assert!(io.serviced <= FAIL_AFTER as usize);
    }

    #[test]
    fn device_invalidated_is_device_lost_immediately() {
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 10).chain([Step::Ev(1, AUDCLNT_E_DEVICE_INVALIDATED)]).chain(ev(0, OK, 10));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(&end, LoopEnd::DeviceLost(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED), "{end:?}");
        assert_eq!((errors, restarts), (0, 0), "장치 분실은 재시도하지 않는다");
        assert_eq!(io.serviced, 11);
    }

    #[test]
    fn device_lost_during_restart() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 100), &stop);
        io.restart_results = [AUDCLNT_E_DEVICE_INVALIDATED].into();
        let (end, _, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::DeviceLost(_)), "{end:?}");
        assert_eq!(restarts, 1);
    }

    #[test]
    fn stalled_events_surface_invalidation() {
        // 이벤트가 멈춘 비기본 장치 분리(L1): 타임아웃 때 상태 확인이 무효화를 드러낸다.
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 5).chain([Step::Timeout(OK), Step::Timeout(AUDCLNT_E_DEVICE_INVALIDATED)]);
        let mut io = FakeIo::new(2, script, &stop);
        let (end, _, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::DeviceLost(_)), "{end:?}");
    }

    #[test]
    fn out_of_memory_is_fatal() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, [Step::Ev(0, E_OUTOFMEMORY)], &stop);
        let (end, errors, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::Failed(_)), "{end:?}");
        assert_eq!(errors, 0);
    }

    #[test]
    fn channel_candidates_respect_minimum() {
        assert_eq!(channel_candidates(2, 1), vec![2, 1]);
        assert_eq!(channel_candidates(8, 4), vec![8, 4]);
        assert_eq!(channel_candidates(2, 4), vec![4]);
    }

    #[test]
    fn failed_open_resets_audio_priority_to_unmanaged() {
        // 리뷰 M1: 열기 실패 후 cpal로 폴백하면 오디오 스레드 상태가 mmcss로 남으면 안 된다.
        use crate::engine::mixer::MixControls;
        use crate::engine::prio::ThreadPriority;
        use crate::engine::rt::{input_ring, output_path};
        let counters = Arc::new(AudioCounters::default());
        let (p, _c) = input_ring();
        let (out_rt, _feed) = output_path(counters.clone(), Arc::new(MixControls::default()));
        let cfg = EngineConfig {
            input_device: Some("없는-장치-band-test".into()),
            ..Default::default()
        };
        let r = open_engine_streams(
            &cfg,
            (ShareMode::Shared3, ShareMode::Shared3),
            InputRt::new(p, counters.clone(), &[0], 1),
            out_rt,
            counters.clone(),
        );
        let Err(f) = r else { panic!("없는 장치는 열기에 실패해야 함") };
        assert!(f.rts.is_some(), "폴백할 수 있게 RT를 돌려줘야 함: {}", f.message);
        assert_eq!(counters.audio_thread.load(), ThreadPriority::Unmanaged);
    }

    #[test]
    fn device_infos_list_shared_and_exclusive() {
        let ep = Endpoint {
            endpoint_id: "{x}".into(),
            name: "Speakers".into(),
            is_input: false,
            is_default: true,
            mix: Some(StreamFormat { kind: SampleKind::F32, channels: 2, rate: 48_000, valid_bits: 32 }),
            device_period_hns: (100_000, 30_000),
            shared3: None,
            exclusive_formats: vec![
                StreamFormat { kind: SampleKind::I32, channels: 2, rate: 96_000, valid_bits: 24 },
                StreamFormat { kind: SampleKind::I16, channels: 2, rate: 48_000, valid_bits: 16 },
            ],
        };
        let v = device_infos(&[ep]);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].driver, DriverKind::WasapiShared);
        assert_eq!(v[0].id, "wasapi::Speakers");
        assert_eq!(v[1].driver, DriverKind::WasapiExclusive);
        assert_eq!(v[1].id, "wasapi::Speakers");
        assert_eq!(v[1].sample_rates, vec![48_000, 96_000]);
    }
}
