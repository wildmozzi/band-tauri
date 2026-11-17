//! WASAPI 형식: 열 모드(`ShareMode`)와 폴백 순서, 샘플 형식·채널 마스크·`StreamFormat`(WAVEFORMATEX 변환),
//! 24비트 패킹 변환(RT, 할당 없음), 루프 오류 분류(`classify`). COM 객체를 만들지 않는다(이동만, 엔진 ⑥-5).

use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::E_OUTOFMEMORY;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};

use crate::engine::device::types::BackendKind;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// 열 모드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    Exclusive,
    Shared3,
}

/// 열기 시도 순서(L3): 요청 그대로 → 한 방향만 저지연 공유로(다른 앱이 한쪽만 쥐고 있어도 다른 쪽 독점 유지)
/// → 둘 다 저지연 공유. 그다음은 호출자가 cpal로 폴백한다.
pub(crate) fn fallback_modes(mi: ShareMode, mo: ShareMode) -> Vec<(ShareMode, ShareMode)> {
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
    pub(crate) fn backend(self) -> BackendKind {
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
pub(super) fn mask_candidates(channels: u16, mix: Option<(u16, u32)>) -> Vec<u32> {
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

    pub(super) fn to_wfx_masked(self, mask: u32) -> WAVEFORMATEXTENSIBLE {
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
    pub(super) unsafe fn from_wfx_mask(p: *const WAVEFORMATEX) -> Option<(Self, Option<u32>)> {
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
    pub(super) unsafe fn from_wfx(p: *const WAVEFORMATEX) -> Option<Self> {
        // SAFETY: 호출자 보장.
        unsafe { Self::from_wfx_mask(p) }.map(|x| x.0)
    }
}

// ============================================================================
// 24비트 패킹 변환(RT, 할당 없음)
// ============================================================================

const I24_SCALE: f32 = 8_388_608.0; // 2^23

/// 24비트 패킹(리틀 엔디언 3바이트) → f32. `dst.len()`과 `src.len() / 3` 중 작은 만큼 변환하고 그 수를 돌려준다.
pub(crate) fn i24_to_f32(src: &[u8], dst: &mut [f32]) -> usize {
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
pub(crate) fn f32_to_i24(src: &[f32], dst: &mut [u8]) -> usize {
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
pub(crate) enum ErrClass {
    /// 장치가 사라짐/무효화: `deviceLost`.
    DeviceLost,
    /// 복구 불가(메모리 부족): 스트림 중단.
    Fatal,
    /// 일시 오류: 다음 이벤트에 다시 시도, 반복되면 재시작.
    Transient,
}

pub(crate) fn classify(code: HRESULT) -> ErrClass {
    if code == AUDCLNT_E_DEVICE_INVALIDATED || code == AUDCLNT_E_SERVICE_NOT_RUNNING || code == AUDCLNT_E_RESOURCES_INVALIDATED {
        ErrClass::DeviceLost
    } else if code == E_OUTOFMEMORY {
        ErrClass::Fatal
    } else {
        ErrClass::Transient
    }
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

    use windows::Win32::Media::Audio::{AUDCLNT_E_BUFFER_ERROR, AUDCLNT_E_BUFFER_TOO_LARGE};

    #[test]
    fn classify_hresults() {
        assert_eq!(classify(AUDCLNT_E_DEVICE_INVALIDATED), ErrClass::DeviceLost);
        assert_eq!(classify(AUDCLNT_E_SERVICE_NOT_RUNNING), ErrClass::DeviceLost);
        assert_eq!(classify(AUDCLNT_E_RESOURCES_INVALIDATED), ErrClass::DeviceLost);
        assert_eq!(classify(E_OUTOFMEMORY), ErrClass::Fatal);
        assert_eq!(classify(AUDCLNT_E_BUFFER_ERROR), ErrClass::Transient);
        assert_eq!(classify(AUDCLNT_E_BUFFER_TOO_LARGE), ErrClass::Transient);
    }
}
