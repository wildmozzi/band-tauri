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
//! ## 장치 분실
//! - 루프의 WASAPI 호출이 실패하면(`AUDCLNT_E_DEVICE_INVALIDATED` 등) 스트림을 멈추고 `deviceLost`를 켠다.
//! - 기본 장치로 연 방향은 감시 스레드가 0.5초마다 기본 장치 ID를 확인해, 바뀌면 `deviceLost`를 켠다
//!   (프론트의 "running=false → 엔진 정지" 감지가 동작하고, 다시 시작하면 새 기본 장치로 열린다).

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
    AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

use super::config::{DriverKind, EngineConfig};
use super::device::{device_id, device_name_from_id, BackendKind, DeviceInfo, StreamInfo};
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
/// 무음 캡처 패킷용 0 버퍼(샘플). 독점 192kHz 8채널 50ms도 덮는다.
const SILENCE_SAMPLES: usize = 192_000 / 20 * 8;

/// 열 모드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    Exclusive,
    Shared3,
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
    /// 32비트 컨테이너 정수(유효 24 또는 32비트, 상위 정렬).
    I32,
    I16,
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
            SampleKind::I16 => 2,
        }
    }

    fn to_wfx(self) -> WAVEFORMATEXTENSIBLE {
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
            dwChannelMask: match self.channels {
                1 => 0x4, // FRONT_CENTER
                2 => 0x3, // FRONT_LEFT | FRONT_RIGHT
                n => (1u32 << n.min(31)) - 1,
            },
            SubFormat: if self.kind == SampleKind::F32 { SUBTYPE_IEEE_FLOAT } else { SUBTYPE_PCM },
        }
    }

    /// 장치가 준 WAVEFORMATEX(믹스 포맷 등)를 해석한다. 엔진이 다루지 못하는 형식이면 None.
    ///
    /// # Safety
    /// `p`는 유효한 WAVEFORMATEX(확장이면 WAVEFORMATEXTENSIBLE)를 가리켜야 한다.
    unsafe fn from_wfx(p: *const WAVEFORMATEX) -> Option<Self> {
        if p.is_null() {
            return None;
        }
        // SAFETY: 호출자가 유효한 포인터를 보장한다. packed 구조라 read_unaligned로 읽는다.
        let f = unsafe { std::ptr::read_unaligned(p) };
        let (tag, valid) = if f.wFormatTag == WAVE_FORMAT_EXTENSIBLE && f.cbSize >= 22 {
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
            (tag, valid)
        } else {
            (f.wFormatTag, f.wBitsPerSample)
        };
        let kind = match (tag, f.wBitsPerSample) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleKind::F32,
            (WAVE_FORMAT_PCM, 32) => SampleKind::I32,
            (WAVE_FORMAT_PCM, 16) => SampleKind::I16,
            _ => return None,
        };
        Some(Self { kind, channels: f.nChannels, rate: f.nSamplesPerSec, valid_bits: valid.max(1) })
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

fn mix_format(client: &IAudioClient) -> Option<StreamFormat> {
    // SAFETY: GetMixFormat이 할당한 포맷을 읽고 해제한다.
    unsafe {
        let p = client.GetMixFormat().ok()?;
        let f = StreamFormat::from_wfx(p);
        CoTaskMemFree(Some(p as *const _));
        f
    }
}

fn exclusive_supported(client: &IAudioClient, f: StreamFormat) -> bool {
    let w = f.to_wfx();
    // SAFETY: w는 유효한 WAVEFORMATEXTENSIBLE이며 호출 동안 살아 있다. 독점 모드는 closest match를 받지 않는다.
    unsafe { client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &w.Format, None) }.0 == 0
}

fn sample_candidates() -> [(SampleKind, u16); 4] {
    [(SampleKind::F32, 32), (SampleKind::I32, 32), (SampleKind::I32, 24), (SampleKind::I16, 16)]
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

/// 독점 포맷 선택: 레이트(48k 우선) → 샘플 형식 → 채널 순으로 처음 지원되는 것.
fn pick_exclusive_format(client: &IAudioClient, mix_channels: u16, min_channels: u16) -> Option<StreamFormat> {
    for rate in EXCLUSIVE_RATES {
        for (kind, valid_bits) in sample_candidates() {
            for channels in channel_candidates(mix_channels, min_channels) {
                let f = StreamFormat { kind, channels, rate, valid_bits };
                if exclusive_supported(client, f) {
                    return Some(f);
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
        ep.mix = mix_format(&client);
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
                    if exclusive_supported(&client, f) {
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
    fn prefill(&self) -> Result<(), String> {
        let Some(r) = &self.render else {
            return Ok(());
        };
        let n = match self.mode {
            ShareMode::Exclusive => self.buffer_frames,
            ShareMode::Shared3 => self.period_frames.min(self.buffer_frames),
        };
        // SAFETY: 시작 전 버퍼에 n ≤ 버퍼 크기 프레임을 무음으로 기록한다.
        unsafe {
            r.GetBuffer(n).map_err(|e| err("렌더 버퍼 실패", e))?;
            r.ReleaseBuffer(n, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32).map_err(|e| err("렌더 버퍼 해제 실패", e))?;
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        self.prefill()?;
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.Start() }.map_err(|e| err("스트림 시작 실패", e))?;
        self.started = true;
        Ok(())
    }
}

fn open_exclusive(dev: &IMMDevice, min_channels: u16) -> Result<(IAudioClient, StreamFormat, u32, u32), String> {
    let client: IAudioClient = activate(dev)?;
    let mix_ch = mix_format(&client).map(|f| f.channels).unwrap_or(2);
    let format = pick_exclusive_format(&client, mix_ch, min_channels)
        .ok_or_else(|| format!("독점 모드로 지원하는 포맷이 없습니다(필요 채널 {min_channels})"))?;
    let (mut def, mut min) = (0i64, 0i64);
    // SAFETY: 출력 포인터는 지역 변수.
    unsafe { client.GetDevicePeriod(Some(&mut def), Some(&mut min)) }.map_err(|e| err("장치 주기 조회 실패", e))?;
    let wfx = format.to_wfx();
    let init = |c: &IAudioClient, period: i64| {
        // SAFETY: wfx는 호출 동안 살아 있다. 이벤트 구동 독점 모드는 버퍼 길이 = 주기.
        unsafe { c.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, period, &wfx.Format, None) }
    };
    let client = match init(&client, min) {
        Ok(()) => client,
        Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // 장치가 정렬된 크기를 알려 준다: 그 크기로 주기를 다시 계산하고 새 클라이언트로 재초기화.
            // SAFETY: 초기화 실패 후에도 GetBufferSize는 정렬된 크기를 돌려준다(문서화된 절차).
            let frames = unsafe { client.GetBufferSize() }.map_err(|e| err("정렬 버퍼 크기 조회 실패", e))?;
            let period = (HNS_PER_SEC * frames as f64 / format.rate as f64 + 0.5) as i64;
            let c2: IAudioClient = activate(dev)?;
            init(&c2, period).map_err(|e| err("독점 모드 재초기화 실패", e))?;
            c2
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

/// 장치 버퍼 뷰.
pub enum DevBuf<'a> {
    F32(&'a mut [f32]),
    I32(&'a mut [i32]),
    I16(&'a mut [i16]),
}

impl DevBuf<'_> {
    /// # Safety
    /// `p`는 `frames × channels` 샘플을 담은 유효한 장치 버퍼여야 하고 반환값보다 오래 살아야 한다.
    unsafe fn from_raw<'a>(p: *mut u8, frames: u32, f: &StreamFormat) -> DevBuf<'a> {
        let n = frames as usize * f.channels as usize;
        // SAFETY: 호출자 보장. WASAPI 버퍼는 샘플 정렬되어 있다.
        unsafe {
            match f.kind {
                SampleKind::F32 => DevBuf::F32(std::slice::from_raw_parts_mut(p as *mut f32, n)),
                SampleKind::I32 => DevBuf::I32(std::slice::from_raw_parts_mut(p as *mut i32, n)),
                SampleKind::I16 => DevBuf::I16(std::slice::from_raw_parts_mut(p as *mut i16, n)),
            }
        }
    }
}

/// 루프 종료 사유.
#[derive(Debug)]
pub enum LoopEnd {
    Stopped,
    Failed(windows::core::Error),
}

/// 입·출력 이벤트 루프. `on_capture(buf, silent)`, `on_render(buf) -> 무음 플래그`.
/// RT 경로: 할당·락·로그 없음(오류는 반환값으로).
pub fn run_loop(
    input: Option<&DirStream>,
    output: Option<&DirStream>,
    stop: &AtomicBool,
    silence: &mut [f32],
    mut on_capture: impl FnMut(DevBuf<'_>),
    mut on_render: impl FnMut(DevBuf<'_>) -> bool,
) -> LoopEnd {
    let mut handles = [HANDLE::default(); 2];
    let mut kinds = [false; 2]; // true = 캡처
    let mut n = 0;
    if let Some(i) = input {
        handles[n] = i.event;
        kinds[n] = true;
        n += 1;
    }
    if let Some(o) = output {
        handles[n] = o.event;
        n += 1;
    }
    if n == 0 {
        return LoopEnd::Stopped;
    }
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: 유효한 이벤트 핸들. 100ms마다 정지 플래그를 확인한다.
        let w = unsafe { WaitForMultipleObjects(&handles[..n], false, 100) };
        if w == WAIT_TIMEOUT {
            continue;
        }
        let idx = w.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if idx >= n {
            return LoopEnd::Failed(windows::core::Error::from_thread());
        }
        let r = if kinds[idx] {
            input.map(|i| service_capture(i, silence, &mut on_capture)).unwrap_or(Ok(()))
        } else {
            output.map(|o| service_render(o, &mut on_render)).unwrap_or(Ok(()))
        };
        if let Err(e) = r {
            return LoopEnd::Failed(e);
        }
    }
    LoopEnd::Stopped
}

fn service_capture(s: &DirStream, silence: &mut [f32], on_capture: &mut impl FnMut(DevBuf<'_>)) -> windows::core::Result<()> {
    let Some(cap) = &s.capture else {
        return Ok(());
    };
    // SAFETY: 이 스레드에서 연 캡처 클라이언트. GetBuffer로 받은 포인터는 ReleaseBuffer 전까지만 쓴다.
    unsafe {
        loop {
            if cap.GetNextPacketSize()? == 0 {
                return Ok(());
            }
            let mut p: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            cap.GetBuffer(&mut p, &mut frames, &mut flags, None, None)?;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let samples = frames as usize * s.format.channels as usize;
            if silent || p.is_null() {
                // 장치가 무음이라고 표시한 패킷: 버퍼 내용은 의미 없으므로 0을 넘긴다(미리 할당한 f32 버퍼,
                // 장치 형식과 무관하게 f32 경로로 처리된다).
                let k = samples.min(silence.len());
                if let Some(z) = silence.get_mut(..k) {
                    z.fill(0.0);
                    on_capture(DevBuf::F32(z));
                }
            } else if samples > 0 {
                on_capture(DevBuf::from_raw(p, frames, &s.format));
            }
            cap.ReleaseBuffer(frames)?;
        }
    }
}

fn service_render(s: &DirStream, on_render: &mut impl FnMut(DevBuf<'_>) -> bool) -> windows::core::Result<()> {
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
        let silent = on_render(DevBuf::from_raw(p, frames, &s.format));
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
    let (tx, rx) = mpsc::channel::<Result<(StreamInfo, [Option<String>; 2]), OpenFailure>>();
    let in_name = cfg.input_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let out_name = cfg.output_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let buffer_frames = cfg.buffer_size;
    let st = stop.clone();
    let c = counters.clone();
    let spawned = std::thread::Builder::new().name("band-wasapi".into()).spawn(move || {
        let _com = ComGuard::init();
        let (mut input, mut output) = (input, output);
        let opened = (|| {
            let en = enumerator()?;
            let min_in = input.required_device_channels() as u16;
            let mut i = DirStream::open(&en, true, in_name.as_deref(), modes.0, min_in)?;
            let mut o = DirStream::open(&en, false, out_name.as_deref(), modes.1, 1)?;
            o.start()?;
            i.start()?;
            Ok::<_, String>((i, o))
        })();
        let (i, o) = match opened {
            Ok(x) => x,
            Err(message) => {
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
        };
        // 기본 장치로 연 방향은 감시한다(기본 장치가 바뀌면 deviceLost).
        let watch = [
            in_name.is_none().then(|| i.endpoint_id.clone()),
            out_name.is_none().then(|| o.endpoint_id.clone()),
        ];
        log::info!(
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
        let _ = tx.send(Ok((info, watch)));
        let _prio = super::prio::promote_current_thread();
        let (ich, och) = (i.format.channels as usize, o.format.channels as usize);
        let mut silence = vec![0f32; SILENCE_SAMPLES];
        let end = run_loop(
            Some(&i),
            Some(&o),
            &st,
            &mut silence,
            |b| feed_input(&mut input, b, ich),
            |b| {
                fill_output(&mut output, b, och);
                false
            },
        );
        if let LoopEnd::Failed(e) = end {
            c.stream_errors.fetch_add(1, Ordering::Relaxed);
            c.device_lost.store(true, Ordering::SeqCst);
            log::error!("WASAPI 스트림 중단(장치 분리/무효화 가능): {e}");
        }
    });
    let audio = match spawned {
        Ok(h) => h,
        Err(e) => return Err(OpenFailure { message: format!("WASAPI 스레드 생성 실패: {e}"), rts: None }),
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok((info, watch))) => {
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
            let mut silence = vec![0f32; SILENCE_SAMPLES];
            let end = run_loop(input, output, &stop, &mut silence, |b| tick(len(&b)), |mut b| {
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
                    LoopEnd::Failed(e) => Some(e.to_string()),
                },
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
        // 24비트 패킹(3바이트)은 다루지 않는다
        let w24 = WAVEFORMATEX { wBitsPerSample: 24, nBlockAlign: 6, ..w };
        // SAFETY: 지역 구조체.
        assert!(unsafe { StreamFormat::from_wfx(&w24) }.is_none());
    }

    #[test]
    fn channel_candidates_respect_minimum() {
        assert_eq!(channel_candidates(2, 1), vec![2, 1]);
        assert_eq!(channel_candidates(8, 4), vec![8, 4]);
        assert_eq!(channel_candidates(2, 4), vec![4]);
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
