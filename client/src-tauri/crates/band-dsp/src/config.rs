//! 엔진 설정과 검증 (설계 문서 4절 `EngineConfig`).

use serde::{Deserialize, Serialize};

/// 내부 처리 레이트. 항상 48kHz.
pub const INTERNAL_RATE: u32 = 48_000;
/// UDP 패킷 최대 크기(헤더 + 암호문 + 태그).
pub const MAX_PACKET_BYTES: usize = 1200;
/// 패킷 헤더 크기.
pub const HEADER_BYTES: usize = 18;
/// AEAD 태그 크기.
pub const TAG_BYTES: usize = 16;
/// 페이로드(평문) 최대 크기.
pub const MAX_PAYLOAD_BYTES: usize = MAX_PACKET_BYTES - HEADER_BYTES - TAG_BYTES;
/// 출력 소스 슬롯 수: 0 = 로컬(모니터), 1.. = 피어(원격 최대 5명 + 교체 중인 슬롯 여유).
pub const MAX_SOURCES: usize = 8;
/// 로컬 입력(모니터) 소스 슬롯.
pub const LOCAL_SOURCE: usize = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriverKind {
    Asio,
    WasapiShared,
    WasapiExclusive,
}

impl DriverKind {
    /// 이 빌드가 열 수 있는 드라이버(P2-7, 설계 4절 `engine_supported_drivers`).
    /// ASIO는 `asio` 피처(+ CPAL_ASIO_DIR SDK)로 빌드했을 때만 들어간다.
    pub fn supported() -> Vec<DriverKind> {
        supported_drivers(cfg!(feature = "asio"))
    }
}

/// 지원 드라이버 목록(지연 낮은 순). `asio` = ASIO 포함 빌드인가.
pub fn supported_drivers(asio: bool) -> Vec<DriverKind> {
    let mut v = Vec::with_capacity(3);
    if asio {
        v.push(DriverKind::Asio);
    }
    v.push(DriverKind::WasapiExclusive);
    v.push(DriverKind::WasapiShared);
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodecKind {
    Opus,
    Pcm16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    pub input_driver: DriverKind,
    pub output_driver: DriverKind,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub buffer_size: u32,
    pub frame_ms: f32,
    pub codec: CodecKind,
    pub channels: u8,
    pub input_channels: Vec<u16>,
    #[serde(default)]
    pub opus_bitrate: Option<u32>,
    #[serde(default)]
    pub jitter_min_ms: Option<f32>,
    #[serde(default)]
    pub jitter_max_ms: Option<f32>,
    /// 지터 버퍼 '저지연(0) ↔ 안정(1)' 프로필(P1-5). 없으면 0.5(균형).
    #[serde(default)]
    pub jitter_profile: Option<f32>,
    #[serde(default)]
    pub udp_port: Option<u16>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            input_driver: DriverKind::WasapiShared,
            output_driver: DriverKind::WasapiShared,
            input_device: None,
            output_device: None,
            buffer_size: 128,
            frame_ms: 5.0,
            codec: CodecKind::Opus,
            channels: 1,
            input_channels: vec![0],
            opus_bitrate: None,
            jitter_min_ms: None,
            jitter_max_ms: None,
            jitter_profile: None,
            udp_port: None,
        }
    }
}

/// 프레임 크기 코드(헤더 17번 바이트 = frame_samples / 120).
pub fn frame_code_from_samples(samples: usize) -> Option<u8> {
    match samples {
        120 => Some(1),
        240 => Some(2),
        480 => Some(4),
        _ => None,
    }
}

pub fn frame_samples_from_code(code: u8) -> Option<usize> {
    match code {
        1 | 2 | 4 => Some(code as usize * 120),
        _ => None,
    }
}

impl EngineConfig {
    /// 48kHz 기준 프레임당 샘플 수(채널당).
    pub fn frame_samples(&self) -> usize {
        // validate()에서 2.5/5/10만 허용하므로 반올림 결과는 120/240/480.
        (self.frame_ms * INTERNAL_RATE as f32 / 1000.0).round() as usize
    }

    pub fn opus_bitrate(&self) -> u32 {
        self.opus_bitrate
            .unwrap_or(if self.channels == 2 { 160_000 } else { 96_000 })
    }

    pub fn jitter_min_ms(&self) -> f32 {
        self.jitter_min_ms.unwrap_or(0.0)
    }

    pub fn jitter_max_ms(&self) -> f32 {
        self.jitter_max_ms.unwrap_or(80.0)
    }

    pub fn jitter_profile(&self) -> f32 {
        self.jitter_profile.unwrap_or(0.5)
    }

    /// 이 설정에서 나올 수 있는 최대 페이로드 크기(바이트).
    pub fn max_payload_bytes(&self) -> usize {
        match self.codec {
            CodecKind::Pcm16 => self.frame_samples() * self.channels as usize * 2,
            // CBR 기준 비트레이트 × 프레임 길이. VBR 변동은 인코더 출력 버퍼 크기로 상한을 건다.
            CodecKind::Opus => {
                (self.opus_bitrate() as f32 * self.frame_ms / 1000.0 / 8.0).ceil() as usize
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(32..=1024).contains(&self.buffer_size) {
            return Err(format!("bufferSize는 32..1024 범위여야 합니다: {}", self.buffer_size));
        }
        let fm = self.frame_ms;
        if !((fm - 2.5).abs() < 1e-3 || (fm - 5.0).abs() < 1e-3 || (fm - 10.0).abs() < 1e-3) {
            return Err(format!("frameMs는 2.5, 5, 10 중 하나여야 합니다: {fm}"));
        }
        if self.channels != 1 && self.channels != 2 {
            return Err(format!("channels는 1 또는 2여야 합니다: {}", self.channels));
        }
        if self.input_channels.is_empty() || self.input_channels.len() > 2 {
            return Err("inputChannels는 채널 번호 1개 또는 2개여야 합니다".into());
        }
        if self.input_channels.iter().any(|&c| c >= 64) {
            return Err("inputChannels 값이 너무 큽니다(0..63)".into());
        }
        if let Some(b) = self.opus_bitrate {
            if !(6_000..=510_000).contains(&b) {
                return Err(format!("opusBitrate는 6000..510000 범위여야 합니다: {b}"));
            }
        }
        let jmin = self.jitter_min_ms();
        let jmax = self.jitter_max_ms();
        if !jmin.is_finite() || !jmax.is_finite() || jmin < 0.0 || jmax <= 0.0 {
            return Err("jitterMinMs/jitterMaxMs가 올바르지 않습니다".into());
        }
        if jmax > 1000.0 {
            return Err("jitterMaxMs는 1000ms 이하여야 합니다".into());
        }
        if jmin > jmax {
            return Err("jitterMinMs가 jitterMaxMs보다 큽니다".into());
        }
        if jmax < fm {
            return Err("jitterMaxMs는 frameMs 이상이어야 합니다".into());
        }
        let jp = self.jitter_profile();
        if !jp.is_finite() || !(0.0..=1.0).contains(&jp) {
            return Err(format!("jitterProfile은 0..1 범위여야 합니다: {jp}"));
        }
        let payload = self.max_payload_bytes();
        if payload > MAX_PAYLOAD_BYTES {
            return Err(format!(
                "패킷 크기 초과: 페이로드 {payload}B > {MAX_PAYLOAD_BYTES}B \
                 (PCM16 스테레오는 frameMs ≤ 5, 또는 Opus 비트레이트를 낮추세요)"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_drivers_follow_asio_flag() {
        assert_eq!(supported_drivers(false), vec![DriverKind::WasapiExclusive, DriverKind::WasapiShared]);
        assert_eq!(
            supported_drivers(true),
            vec![DriverKind::Asio, DriverKind::WasapiExclusive, DriverKind::WasapiShared]
        );
    }

    #[test]
    fn supported_matches_build_feature() {
        let s = DriverKind::supported();
        // 기본 빌드(asio 피처 꺼짐)에는 ASIO가 없어야 한다
        assert_eq!(s.contains(&DriverKind::Asio), cfg!(feature = "asio"));
        assert!(s.contains(&DriverKind::WasapiShared) && s.contains(&DriverKind::WasapiExclusive));
        // 직렬화 형식(설계 4절 DriverKind 문자열)
        let json = serde_json::to_string(&supported_drivers(true)).unwrap();
        assert_eq!(json, r#"["asio","wasapiExclusive","wasapiShared"]"#);
    }

    #[test]
    fn default_is_valid() {
        EngineConfig::default().validate().expect("기본 설정은 유효해야 함");
    }

    #[test]
    fn jitter_profile_default_and_range() {
        let c: EngineConfig = serde_json::from_str(&serde_json::to_string(&EngineConfig::default()).unwrap().replace(",\"jitterProfile\":null", "")).unwrap();
        assert_eq!(c.jitter_profile(), 0.5, "없으면 균형");
        for p in [0.0, 0.5, 1.0] {
            EngineConfig { jitter_profile: Some(p), ..Default::default() }.validate().unwrap();
        }
        for p in [-0.1, 1.5, f32::NAN] {
            assert!(EngineConfig { jitter_profile: Some(p), ..Default::default() }.validate().is_err(), "{p}");
        }
    }

    #[test]
    fn frame_sizes() {
        for (ms, n) in [(2.5, 120), (5.0, 240), (10.0, 480)] {
            let c = EngineConfig { frame_ms: ms, ..Default::default() };
            c.validate().unwrap();
            assert_eq!(c.frame_samples(), n);
            assert!(frame_code_from_samples(n).is_some());
            assert_eq!(frame_samples_from_code(frame_code_from_samples(n).unwrap()), Some(n));
        }
        let c = EngineConfig { frame_ms: 20.0, ..Default::default() };
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_oversized_pcm() {
        let c = EngineConfig {
            codec: CodecKind::Pcm16,
            channels: 2,
            input_channels: vec![0, 1],
            frame_ms: 10.0,
            ..Default::default()
        };
        assert!(c.validate().is_err(), "PCM16 stereo 10ms = 1920B는 거부");
        let ok = EngineConfig { frame_ms: 5.0, ..c.clone() };
        ok.validate().unwrap();
        let mono = EngineConfig { channels: 1, input_channels: vec![0], ..c };
        mono.validate().unwrap(); // 960B
    }

    #[test]
    fn rejects_bad_ranges() {
        let base = EngineConfig::default();
        assert!(EngineConfig { buffer_size: 16, ..base.clone() }.validate().is_err());
        assert!(EngineConfig { buffer_size: 2048, ..base.clone() }.validate().is_err());
        assert!(EngineConfig { channels: 3, ..base.clone() }.validate().is_err());
        assert!(EngineConfig { input_channels: vec![], ..base.clone() }.validate().is_err());
        assert!(EngineConfig { input_channels: vec![0, 1, 2], ..base.clone() }.validate().is_err());
        assert!(EngineConfig { opus_bitrate: Some(1000), ..base.clone() }.validate().is_err());
        assert!(EngineConfig { jitter_min_ms: Some(50.0), jitter_max_ms: Some(20.0), ..base.clone() }
            .validate()
            .is_err());
        // Opus 최대 비트레이트 10ms = 638B → 허용
        EngineConfig { opus_bitrate: Some(510_000), frame_ms: 10.0, ..base }.validate().unwrap();
    }

    #[test]
    fn deserializes_camel_case() {
        let json = r#"{
            "inputDriver":"asio","outputDriver":"wasapiShared",
            "inputDevice":null,"outputDevice":"wasapi::Speakers",
            "bufferSize":64,"frameMs":2.5,"codec":"pcm16","channels":1,
            "inputChannels":[1],"jitterMaxMs":40
        }"#;
        let c: EngineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.input_driver, DriverKind::Asio);
        assert_eq!(c.codec, CodecKind::Pcm16);
        assert_eq!(c.frame_samples(), 120);
        assert_eq!(c.jitter_max_ms(), 40.0);
        c.validate().unwrap();
    }
}
