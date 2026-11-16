//! WASAPI 장치 열거: COM 보조(스레드 COM 초기화, 열거기·엔드포인트 조회, 믹스 포맷, 독점 포맷 탐색)와
//! 엔드포인트 조사(`enumerate_endpoints`)·프론트 장치 목록(`device_infos`)(이동만, 엔진 ⑥-5).

use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioClient, IAudioClient3, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_EXCLUSIVE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use super::format::{mask_candidates, SampleKind, StreamFormat};
use band_dsp::config::DriverKind;
use crate::engine::device::types::{device_id, DeviceInfo};

/// PKEY_Device_FriendlyName
const PKEY_FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// 독점 포맷 탐색 순서: 48k 우선, 44.1k(리샘플), 그다음 고레이트.
const EXCLUSIVE_RATES: [u32; 4] = [48_000, 44_100, 96_000, 88_200];
/// 장치 목록에 보고할 독점 지원 레이트(탐색 비용을 줄이려고 대표값만).
const LIST_RATES: [u32; 3] = [44_100, 48_000, 96_000];

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

pub(super) struct ComGuard(bool);

impl ComGuard {
    pub(super) fn init() -> Self {
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

pub(super) fn err(what: &str, e: windows::core::Error) -> String {
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

pub(super) fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    // SAFETY: COM이 초기화된 스레드에서 호출한다.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| err("장치 열거기 생성 실패", e))
}

pub(super) fn endpoint_id(dev: &IMMDevice) -> String {
    // SAFETY: 유효한 IMMDevice.
    unsafe { dev.GetId() }.map(take_pwstr).unwrap_or_default()
}

pub(super) fn friendly_name(dev: &IMMDevice) -> String {
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

pub(super) fn default_endpoint_id(en: &IMMDeviceEnumerator, is_input: bool) -> Option<String> {
    // SAFETY: 유효한 열거기.
    unsafe { en.GetDefaultAudioEndpoint(flow(is_input), eConsole) }.ok().map(|d| endpoint_id(&d))
}

/// 이름으로 활성 엔드포인트를 찾는다. 이름이 없으면 기본 장치.
pub(super) fn find_device(en: &IMMDeviceEnumerator, is_input: bool, name: Option<&str>) -> Result<IMMDevice, String> {
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

pub(super) fn activate<T: Interface>(dev: &IMMDevice) -> Result<T, String> {
    // SAFETY: 유효한 IMMDevice. 반환 인터페이스는 이 스레드에서만 쓴다.
    unsafe { dev.Activate::<T>(CLSCTX_ALL, None) }.map_err(|e| err("IAudioClient 활성화 실패", e))
}

/// 믹스 포맷과 그 채널 마스크.
pub(super) fn mix_format(client: &IAudioClient) -> (Option<StreamFormat>, Option<(u16, u32)>) {
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
pub(super) fn pick_exclusive_format(client: &IAudioClient, mix: Option<(u16, u32)>, mix_channels: u16, min_channels: u16) -> Option<(StreamFormat, u32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
