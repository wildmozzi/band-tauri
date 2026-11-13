//! 오디오 장치 백엔드 trait. 구현: `cpal`의 `CpalBackend`, `select`의 `SystemBackend`, `fake`의 `FakeBackend`.

use std::any::Any;
use std::sync::Arc;

use crate::engine::config::EngineConfig;
use crate::engine::rt::{InputRt, OutputRt};
use crate::engine::stats::AudioCounters;

use super::types::{DeviceInfo, StreamInfo};

/// 오디오 장치 백엔드. cpal 구현과 테스트용 가짜 구현이 있다.
pub trait AudioBackend: Send + Sync + 'static {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, String>;

    /// 스트림을 열고 재생을 시작한다. 반환된 가드가 drop되면 스트림이 멈춘다.
    /// 엔진의 전용 오디오 스레드에서 호출되며 가드도 그 스레드에서 drop된다.
    fn open(
        &self,
        cfg: &EngineConfig,
        input: InputRt,
        output: OutputRt,
        counters: Arc<AudioCounters>,
    ) -> Result<(StreamInfo, Box<dyn Any>), String>;
}
