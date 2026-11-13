//! 드라이버 선택 규칙:
//! - `asio`(입·출력 중 하나라도) → cpal(ASIO 피처).
//! - `wasapiExclusive` → 자체 WASAPI 독점(이벤트 구동, 장치 최소 주기).
//! - `wasapiShared` → 자체 WASAPI `IAudioClient3` 저지연 공유.
//! - 자체 백엔드가 실패하면: 독점을 저지연 공유로 바꿔 한 번 더, 그래도 안 되면 cpal(공유, ≈10ms).
//!   실제로 열린 백엔드는 `EngineStats.backend`로 보인다.
//! - Windows가 아니면 cpal만 쓴다.

use std::any::Any;
use std::sync::Arc;

use crate::engine::config::{DriverKind, EngineConfig};
use crate::engine::rt::{InputRt, OutputRt};
use crate::engine::stats::AudioCounters;

use super::backend::AudioBackend;
use super::cpal::CpalBackend;
use super::types::{DeviceInfo, StreamInfo};

pub struct SystemBackend {
    cpal: CpalBackend,
}

impl Default for SystemBackend {
    fn default() -> Self {
        Self { cpal: CpalBackend }
    }
}

impl SystemBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(windows)]
fn mode_of(d: DriverKind) -> Option<crate::engine::wasapi::ShareMode> {
    use crate::engine::wasapi::ShareMode;
    match d {
        DriverKind::WasapiExclusive => Some(ShareMode::Exclusive),
        DriverKind::WasapiShared => Some(ShareMode::Shared3),
        DriverKind::Asio => None,
    }
}

impl AudioBackend for SystemBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        #[cfg(windows)]
        {
            match crate::engine::wasapi::enumerate_endpoints() {
                Ok(eps) => {
                    let mut out = crate::engine::wasapi::device_infos(&eps);
                    // ASIO 장치는 cpal에서(피처가 있을 때만 나온다)
                    if cfg!(feature = "asio") {
                        if let Ok(all) = self.cpal.list_devices() {
                            out.extend(all.into_iter().filter(|d| d.driver == DriverKind::Asio));
                        }
                    }
                    return Ok(out);
                }
                Err(e) => log::warn!("자체 WASAPI 열거 실패, cpal로 대신합니다: {e}"),
            }
        }
        self.cpal.list_devices()
    }

    fn open(
        &self,
        cfg: &EngineConfig,
        input: InputRt,
        output: OutputRt,
        counters: Arc<AudioCounters>,
    ) -> Result<(StreamInfo, Box<dyn Any>), String> {
        #[cfg(windows)]
        let (input, output) = {
            use crate::engine::wasapi::{fallback_modes, open_engine_streams};
            let mut rts = (input, output);
            if let (Some(mi), Some(mo)) = (mode_of(cfg.input_driver), mode_of(cfg.output_driver)) {
                for modes in fallback_modes(mi, mo) {
                    match open_engine_streams(cfg, modes, rts.0, rts.1, counters.clone()) {
                        Ok(ok) => return Ok(ok),
                        Err(f) => {
                            log::warn!("자체 WASAPI {modes:?} 열기 실패: {}", f.message);
                            match f.rts {
                                Some(r) => rts = *r,
                                None => return Err(f.message),
                            }
                        }
                    }
                }
                log::warn!("자체 WASAPI를 열 수 없어 cpal(공유 모드)로 엽니다");
            }
            rts
        };
        // cpal 콜백 스레드는 백엔드가 관리한다. 자체 WASAPI 시도(시간 초과 포함)가 남긴 값을 지운다(리뷰 M1).
        counters.audio_thread.store(crate::sys::prio::ThreadPriority::Unmanaged);
        self.cpal.open(cfg, input, output, counters)
    }
}
