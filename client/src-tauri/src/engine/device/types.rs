//! 장치 값 타입과 순수 함수(계층 1). `wasapi`·워커·측정·보고가 쓴다.

use serde::Serialize;

use crate::engine::config::{DriverKind, INTERNAL_RATE};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub driver: DriverKind,
    pub is_input: bool,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rates: Vec<u32>,
}

/// 실제로 열린 오디오 백엔드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// 자체 WASAPI 이벤트 구동 독점 모드.
    WasapiExclusive,
    /// 자체 WASAPI `IAudioClient3` 저지연 공유 모드.
    WasapiShared3,
    /// cpal WASAPI(공유 모드, 보통 10ms 주기).
    CpalWasapi,
    /// cpal ASIO.
    CpalAsio,
    /// 테스트용 가짜 장치.
    Fake,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::WasapiExclusive => "wasapiExclusive",
            BackendKind::WasapiShared3 => "wasapiShared3",
            BackendKind::CpalWasapi => "cpalWasapi",
            BackendKind::CpalAsio => "cpalAsio",
            BackendKind::Fake => "fake",
        }
    }
}

/// 열린 스트림 정보.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub in_rate: u32,
    pub out_rate: u32,
    pub in_channels: usize,
    pub out_channels: usize,
    /// 요청한 장치 버퍼 크기(프레임). 실제 크기는 콜백 길이로 측정한다.
    pub buffer_frames: u32,
    pub in_backend: BackendKind,
    pub out_backend: BackendKind,
    /// 장치에 설정된 주기(프레임). 0이면 알 수 없음(콜백 크기로 측정).
    pub in_period_frames: u32,
    pub out_period_frames: u32,
    /// 드라이버가 보고한 입력/출력 지연(프레임, 장치 레이트). 0이면 알 수 없음.
    /// ASIO는 `ASIOGetLatencies`, 자체 WASAPI는 `IAudioClient::GetStreamLatency`. 지연 측정 보고서에 쓴다.
    pub in_reported_latency_frames: u32,
    pub out_reported_latency_frames: u32,
    /// 실제로 연 입력/출력 장치의 식별 태그(`device_tag`). 기본 장치(`None`)로 열었어도 그때 고른 장치를 가리키므로
    /// OS 기본 장치가 바뀌면 달라진다. 루프백 보정값의 장치 구성 키(`Engine::device_key`)에 쓴다.
    pub in_device_tag: u64,
    pub out_device_tag: u64,
}

/// 장치 이름(또는 엔드포인트 ID)의 안정적인 64비트 태그(FNV-1a). `StreamInfo`를 `Copy`로 두려고 문자열 대신 쓴다.
pub fn device_tag(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

impl StreamInfo {
    /// `EngineStats.backend` 문자열: 입·출력이 같으면 하나, 다르면 `입력/출력`.
    pub fn backend_label(&self) -> String {
        if self.in_backend == self.out_backend {
            self.out_backend.as_str().to_string()
        } else {
            format!("{}/{}", self.in_backend.as_str(), self.out_backend.as_str())
        }
    }
}

pub const ID_SEP: &str = "::";

pub fn device_id(driver: DriverKind, name: &str) -> String {
    let prefix = match driver {
        DriverKind::Asio => "asio",
        DriverKind::WasapiShared | DriverKind::WasapiExclusive => "wasapi",
    };
    format!("{prefix}{ID_SEP}{name}")
}

/// id에서 장치 이름 부분. 접두사가 없으면 전체를 이름으로 본다.
pub fn device_name_from_id(id: &str) -> &str {
    id.split_once(ID_SEP).map(|(_, n)| n).unwrap_or(id)
}

/// 선호 레이트 선택: 48k 지원 시 48k, 아니면 기본 레이트.
pub fn choose_rate(supported: &[(u32, u32)], default_rate: u32) -> u32 {
    if supported.iter().any(|&(lo, hi)| lo <= INTERNAL_RATE && INTERNAL_RATE <= hi) {
        INTERNAL_RATE
    } else {
        default_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = device_id(DriverKind::Asio, "Focusrite USB ASIO");
        assert_eq!(id, "asio::Focusrite USB ASIO");
        assert_eq!(device_name_from_id(&id), "Focusrite USB ASIO");
        assert_eq!(device_name_from_id("Speakers"), "Speakers");
        assert_eq!(device_id(DriverKind::WasapiExclusive, "X"), "wasapi::X");
    }

    #[test]
    fn rate_choice() {
        assert_eq!(choose_rate(&[(44_100, 96_000)], 44_100), 48_000);
        assert_eq!(choose_rate(&[(44_100, 44_100)], 44_100), 44_100);
        assert_eq!(choose_rate(&[], 96_000), 96_000);
    }
}
