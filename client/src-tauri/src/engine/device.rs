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

use std::any::Any;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::Serialize;

use super::config::{DriverKind, EngineConfig, INTERNAL_RATE};
use super::rt::{InputRt, OutputRt};
use super::stats::AudioCounters;

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

// ============================================================================
// cpal 구현
// ============================================================================

pub mod cpal_backend {
    use super::*;
    use crate::engine::rt::RtSample;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, SizedSample, StreamConfig};
    use parking_lot::Mutex;
    use std::sync::atomic::AtomicBool;

    /// ASIO 동시 접근 방지: 열거와 스트림 생성을 직렬화.
    static DEVICE_ENUM_LOCK: Mutex<()> = Mutex::new(());
    /// ASIO 스트림이 열려 있는 동안 true. 이때는 재열거하지 않는다.
    static ASIO_IN_USE: AtomicBool = AtomicBool::new(false);
    /// 마지막 ASIO 열거 결과.
    static ASIO_CACHE: Mutex<Vec<DeviceInfo>> = Mutex::new(Vec::new());

    const PROBE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

    pub struct CpalBackend;

    struct Streams {
        _input: cpal::Stream,
        _output: cpal::Stream,
        asio: bool,
    }

    impl Drop for Streams {
        fn drop(&mut self) {
            if self.asio {
                ASIO_IN_USE.store(false, Ordering::SeqCst);
            }
        }
    }

    fn host_for(driver: DriverKind) -> Result<cpal::Host, String> {
        match driver {
            DriverKind::Asio => asio_host(),
            DriverKind::WasapiShared | DriverKind::WasapiExclusive => {
                #[cfg(target_os = "windows")]
                {
                    cpal::host_from_id(cpal::HostId::Wasapi).map_err(|e| format!("WASAPI 호스트 실패: {e}"))
                }
                #[cfg(not(target_os = "windows"))]
                {
                    Ok(cpal::default_host())
                }
            }
        }
    }

    #[cfg(feature = "asio")]
    fn asio_host() -> Result<cpal::Host, String> {
        cpal::host_from_id(cpal::HostId::Asio).map_err(|e| format!("ASIO 호스트를 열 수 없습니다: {e}"))
    }

    #[cfg(not(feature = "asio"))]
    fn asio_host() -> Result<cpal::Host, String> {
        Err("이 빌드는 ASIO를 지원하지 않습니다(asio 피처 없이 빌드됨)".into())
    }

    fn rates_of<I: Iterator<Item = cpal::SupportedStreamConfigRange>>(it: I) -> (Vec<u32>, u16) {
        let mut rates = Vec::new();
        let mut max_ch = 0;
        for r in it {
            max_ch = max_ch.max(r.channels());
            for &p in &PROBE_RATES {
                if r.min_sample_rate().0 <= p && p <= r.max_sample_rate().0 && !rates.contains(&p) {
                    rates.push(p);
                }
            }
        }
        rates.sort_unstable();
        (rates, max_ch)
    }

    fn describe(device: &cpal::Device, driver: DriverKind, is_input: bool, default_name: Option<&str>) -> Option<DeviceInfo> {
        let name = device.name().ok()?;
        let (rates, ch) = if is_input {
            let def = device.default_input_config().ok()?;
            let (mut rates, ch) = device.supported_input_configs().map(rates_of).unwrap_or_default();
            if rates.is_empty() {
                rates.push(def.sample_rate().0);
            }
            (rates, ch.max(def.channels()))
        } else {
            let def = device.default_output_config().ok()?;
            let (mut rates, ch) = device.supported_output_configs().map(rates_of).unwrap_or_default();
            if rates.is_empty() {
                rates.push(def.sample_rate().0);
            }
            (rates, ch.max(def.channels()))
        };
        Some(DeviceInfo {
            id: device_id(driver, &name),
            is_default: default_name == Some(name.as_str()),
            name,
            driver,
            is_input,
            channels: ch,
            sample_rates: rates,
        })
    }

    fn list_host(host: &cpal::Host, driver: DriverKind, out: &mut Vec<DeviceInfo>) {
        let def_in = host.default_input_device().and_then(|d| d.name().ok());
        let def_out = host.default_output_device().and_then(|d| d.name().ok());
        if let Ok(devs) = host.input_devices() {
            for d in devs {
                if let Some(i) = describe(&d, driver, true, def_in.as_deref()) {
                    out.push(i);
                }
            }
        }
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                if let Some(i) = describe(&d, driver, false, def_out.as_deref()) {
                    out.push(i);
                }
            }
        }
    }

    impl AudioBackend for CpalBackend {
        fn list_devices(&self) -> Result<Vec<DeviceInfo>, String> {
            let _lock = DEVICE_ENUM_LOCK.lock();
            let mut out = Vec::new();
            // cpal 0.15 WASAPI는 공유 모드만 지원하므로 장치는 wasapiShared로만 나열한다.
            match host_for(DriverKind::WasapiShared) {
                Ok(h) => list_host(&h, DriverKind::WasapiShared, &mut out),
                Err(e) => log::warn!("{e}"),
            }
            if cfg!(feature = "asio") {
                if ASIO_IN_USE.load(Ordering::SeqCst) {
                    out.extend(ASIO_CACHE.lock().iter().cloned());
                } else if let Ok(h) = asio_host() {
                    // 열거용 Host는 이 블록 끝에서 해제된다
                    let mut asio = Vec::new();
                    list_host(&h, DriverKind::Asio, &mut asio);
                    *ASIO_CACHE.lock() = asio.clone();
                    out.extend(asio);
                }
            }
            Ok(out)
        }

        fn open(
            &self,
            cfg: &EngineConfig,
            input: InputRt,
            output: OutputRt,
            counters: Arc<AudioCounters>,
        ) -> Result<(StreamInfo, Box<dyn Any>), String> {
            let _lock = DEVICE_ENUM_LOCK.lock();
            if cfg.input_driver == DriverKind::WasapiExclusive || cfg.output_driver == DriverKind::WasapiExclusive {
                log::warn!("cpal 0.15는 WASAPI 배타 모드를 지원하지 않아 공유 모드로 엽니다");
            }
            let duplex_asio = cfg.input_driver == DriverKind::Asio && cfg.output_driver == DriverKind::Asio;
            let any_asio = cfg.input_driver == DriverKind::Asio || cfg.output_driver == DriverKind::Asio;
            let required_in = input.required_device_channels();

            let (in_dev, out_dev) = if duplex_asio {
                let host = host_for(DriverKind::Asio)?;
                let wanted = cfg.input_device.as_deref().or(cfg.output_device.as_deref()).map(device_name_from_id);
                let dev = find_device(&host, wanted, true)
                    .or_else(|| find_device(&host, wanted, false))
                    .ok_or_else(|| "ASIO 장치를 찾을 수 없습니다".to_string())?;
                (dev.clone(), dev)
            } else {
                let ih = host_for(cfg.input_driver)?;
                let oh = host_for(cfg.output_driver)?;
                let i = find_device(&ih, cfg.input_device.as_deref().map(device_name_from_id), true)
                    .ok_or_else(|| "입력 장치를 찾을 수 없습니다".to_string())?;
                let o = find_device(&oh, cfg.output_device.as_deref().map(device_name_from_id), false)
                    .ok_or_else(|| "출력 장치를 찾을 수 없습니다".to_string())?;
                (i, o)
            };

            let in_sel = select_config(&in_dev, true, cfg.input_driver, required_in, cfg.buffer_size)?;
            let mut out_sel = select_config(&out_dev, false, cfg.output_driver, 1, cfg.buffer_size)?;
            if duplex_asio && out_sel.config.sample_rate != in_sel.config.sample_rate {
                // 듀플렉스는 같은 레이트여야 한다. 출력이 그 레이트를 지원하는지 다시 확인한다(리뷰 L8).
                let r = in_sel.config.sample_rate.0;
                let ok = out_dev
                    .supported_output_configs()
                    .map(|mut it| it.any(|c| c.min_sample_rate().0 <= r && r <= c.max_sample_rate().0))
                    .unwrap_or(false);
                if !ok {
                    return Err(format!("ASIO 출력이 입력 레이트 {r}Hz를 지원하지 않습니다"));
                }
                out_sel.config.sample_rate = in_sel.config.sample_rate;
            }
            if (in_sel.config.channels as usize) < required_in {
                return Err(format!(
                    "입력 장치 채널 수({})가 선택한 입력 채널(Ch{})보다 적습니다",
                    in_sel.config.channels, required_in
                ));
            }

            let in_ch = in_sel.config.channels as usize;
            let out_ch = out_sel.config.channels as usize;
            let c_in = counters.clone();
            let input_stream = match in_sel.format {
                SampleFormat::F32 => build_in::<f32>(&in_dev, &in_sel.config, input, in_ch, c_in),
                SampleFormat::I16 => build_in::<i16>(&in_dev, &in_sel.config, input, in_ch, c_in),
                SampleFormat::I32 => build_in::<i32>(&in_dev, &in_sel.config, input, in_ch, c_in),
                SampleFormat::U16 => build_in::<u16>(&in_dev, &in_sel.config, input, in_ch, c_in),
                f => Err(format!("지원하지 않는 입력 샘플 포맷: {f:?}")),
            }?;
            let c_out = counters;
            let output_stream = match out_sel.format {
                SampleFormat::F32 => build_out::<f32>(&out_dev, &out_sel.config, output, out_ch, c_out),
                SampleFormat::I16 => build_out::<i16>(&out_dev, &out_sel.config, output, out_ch, c_out),
                SampleFormat::I32 => build_out::<i32>(&out_dev, &out_sel.config, output, out_ch, c_out),
                SampleFormat::U16 => build_out::<u16>(&out_dev, &out_sel.config, output, out_ch, c_out),
                f => Err(format!("지원하지 않는 출력 샘플 포맷: {f:?}")),
            }?;
            input_stream.play().map_err(|e| format!("입력 스트림 시작 실패: {e}"))?;
            output_stream.play().map_err(|e| format!("출력 스트림 시작 실패: {e}"))?;
            if any_asio {
                ASIO_IN_USE.store(true, Ordering::SeqCst);
            }
            log::info!(
                "오디오 열림: in {} {}Hz {}ch {:?}, out {} {}Hz {}ch {:?}",
                in_dev.name().unwrap_or_default(),
                in_sel.config.sample_rate.0,
                in_ch,
                in_sel.format,
                out_dev.name().unwrap_or_default(),
                out_sel.config.sample_rate.0,
                out_ch,
                out_sel.format
            );
            let kind = |d: DriverKind| if d == DriverKind::Asio { BackendKind::CpalAsio } else { BackendKind::CpalWasapi };
            let info = StreamInfo {
                in_rate: in_sel.config.sample_rate.0,
                out_rate: out_sel.config.sample_rate.0,
                in_channels: in_ch,
                out_channels: out_ch,
                buffer_frames: cfg.buffer_size,
                in_backend: kind(cfg.input_driver),
                out_backend: kind(cfg.output_driver),
                // cpal은 실제 주기를 알려 주지 않는다(WASAPI 공유는 요청과 무관하게 ≈10ms). 콜백 크기로 잰다.
                in_period_frames: 0,
                out_period_frames: 0,
            };
            Ok((info, Box::new(Streams { _input: input_stream, _output: output_stream, asio: any_asio })))
        }
    }

    fn find_device(host: &cpal::Host, name: Option<&str>, input: bool) -> Option<cpal::Device> {
        match name {
            Some(n) => {
                let mut devs = if input { host.input_devices().ok()? } else { host.output_devices().ok()? };
                devs.find(|d| d.name().ok().as_deref() == Some(n))
            }
            None => {
                if input {
                    host.default_input_device()
                } else {
                    host.default_output_device()
                }
            }
        }
    }

    struct Selected {
        config: StreamConfig,
        format: SampleFormat,
    }

    fn select_config(
        dev: &cpal::Device,
        input: bool,
        driver: DriverKind,
        required_ch: usize,
        buffer: u32,
    ) -> Result<Selected, String> {
        let def = if input { dev.default_input_config() } else { dev.default_output_config() }
            .map_err(|e| format!("장치 기본 설정을 읽을 수 없습니다: {e}"))?;
        let ranges: Vec<cpal::SupportedStreamConfigRange> = if input {
            dev.supported_input_configs().map(|i| i.collect()).unwrap_or_default()
        } else {
            dev.supported_output_configs().map(|i| i.collect()).unwrap_or_default()
        };
        let mut channels = def.channels();
        let mut format = def.sample_format();
        let mut rate = def.sample_rate().0;
        let mut buf_support = *def.buffer_size();
        match driver {
            DriverKind::Asio => {
                // 필요한 채널 이상 지원하는 설정 중 48k를 우선, 채널 수는 필요한 만큼(최소 2)
                let want_ch = required_ch.max(if input { 1 } else { 2 }) as u16;
                let pick = ranges
                    .iter()
                    .filter(|r| r.channels() >= want_ch)
                    .min_by_key(|r| {
                        let has48 = r.min_sample_rate().0 <= INTERNAL_RATE && INTERNAL_RATE <= r.max_sample_rate().0;
                        (!has48, r.channels())
                    });
                if let Some(r) = pick {
                    channels = r.channels();
                    format = r.sample_format();
                    buf_support = *r.buffer_size();
                    rate = if r.min_sample_rate().0 <= INTERNAL_RATE && INTERNAL_RATE <= r.max_sample_rate().0 {
                        INTERNAL_RATE
                    } else {
                        def.sample_rate().0
                    };
                }
            }
            DriverKind::WasapiShared | DriverKind::WasapiExclusive => {
                // 공유 모드는 믹스 포맷(채널/포맷) 그대로. 레이트는 48k가 같은 채널/포맷으로 지원되면 48k.
                let rr: Vec<(u32, u32)> = ranges
                    .iter()
                    .filter(|r| r.channels() == channels && r.sample_format() == format)
                    .map(|r| (r.min_sample_rate().0, r.max_sample_rate().0))
                    .collect();
                rate = choose_rate(&rr, rate);
            }
        }
        Ok(Selected {
            config: StreamConfig { channels, sample_rate: cpal::SampleRate(rate), buffer_size: buffer_request(&buf_support, buffer) },
            format,
        })
    }

    /// 드라이버가 보고한 범위 안이면 `Fixed`, 밖이면 `Default`(드라이버 선호 크기).
    /// cpal 0.15 ASIO는 `Fixed`를 그대로 `ASIOCreateBuffers`에 넘겨, 범위 밖 값이면 시작이 실패한다(리뷰 L8).
    fn buffer_request(support: &cpal::SupportedBufferSize, buffer: u32) -> cpal::BufferSize {
        match support {
            cpal::SupportedBufferSize::Range { min, max } if buffer < *min || buffer > *max => {
                log::warn!("버퍼 {buffer}는 장치 범위 {min}..{max} 밖이라 장치 기본 크기를 씁니다");
                cpal::BufferSize::Default
            }
            _ => cpal::BufferSize::Fixed(buffer),
        }
    }

    fn build_in<T: SizedSample + RtSample>(
        dev: &cpal::Device,
        cfg: &StreamConfig,
        mut rt: InputRt,
        ch: usize,
        counters: Arc<AudioCounters>,
    ) -> Result<cpal::Stream, String> {
        dev.build_input_stream(
            cfg,
            move |data: &[T], _: &cpal::InputCallbackInfo| rt.process(data, ch),
            move |_err| {
                counters.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("입력 스트림 생성 실패: {e}"))
    }

    fn build_out<T: SizedSample + RtSample>(
        dev: &cpal::Device,
        cfg: &StreamConfig,
        mut rt: OutputRt,
        ch: usize,
        counters: Arc<AudioCounters>,
    ) -> Result<cpal::Stream, String> {
        dev.build_output_stream(
            cfg,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| rt.process(data, ch),
            move |_err| {
                counters.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("출력 스트림 생성 실패: {e}"))
    }
}

// ============================================================================
// 시스템 백엔드: 자체 WASAPI 우선, cpal 폴백 (P1-0)
// ============================================================================

pub mod system {
    //! 드라이버 선택 규칙:
    //! - `asio`(입·출력 중 하나라도) → cpal(ASIO 피처).
    //! - `wasapiExclusive` → 자체 WASAPI 독점(이벤트 구동, 장치 최소 주기).
    //! - `wasapiShared` → 자체 WASAPI `IAudioClient3` 저지연 공유.
    //! - 자체 백엔드가 실패하면: 독점을 저지연 공유로 바꿔 한 번 더, 그래도 안 되면 cpal(공유, ≈10ms).
    //!   실제로 열린 백엔드는 `EngineStats.backend`로 보인다.
    //! - Windows가 아니면 cpal만 쓴다.

    use super::cpal_backend::CpalBackend;
    use super::*;

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
                use crate::engine::wasapi::{open_engine_streams, ShareMode};
                let mut rts = (input, output);
                if let (Some(mi), Some(mo)) = (mode_of(cfg.input_driver), mode_of(cfg.output_driver)) {
                    let shared = |m: ShareMode| if m == ShareMode::Exclusive { ShareMode::Shared3 } else { m };
                    let mut attempts = vec![(mi, mo)];
                    if (shared(mi), shared(mo)) != (mi, mo) {
                        attempts.push((shared(mi), shared(mo)));
                    }
                    for modes in attempts {
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
            self.cpal.open(cfg, input, output, counters)
        }
    }
}

// ============================================================================
// 가짜 장치 (테스트/통합 테스트용)
// ============================================================================

pub mod fake {
    //! 실제 장치 없이 엔진 전체를 돌리기 위한 가짜 오디오 장치.
    //!
    //! 실제 장치처럼 입력과 출력을 **별도 스레드**에서 각자의 클럭으로 호출한다(리뷰 "테스트 신뢰도" 1):
    //! - 입력/출력 버퍼 크기, 레이트, 클럭 오차(ppm)를 따로 줄 수 있다.
    //! - 출력 시작 위상(`out_phase_us`)과 콜백 기상 지연(`wake_jitter_us`, 늦으면 따라잡으며
    //!   몰아서 호출 = 버스트)을 흉내 낸다.
    //! - 열기 지연(`open_delay`)과 스트림 에러 주입(`error_after`)을 지원한다.
    //!
    //! 시간 모델: 입력 샘플 i는 `t0 + i/in_rate`에 캡처되고, 출력 샘플 j(=sink 인덱스/채널)는
    //! `t0 + out_phase + j/out_rate`에 재생된다. 따라서 임펄스 지연 = out_phase + j/out_rate − i/in_rate.

    use super::*;
    use parking_lot::Mutex;
    use rand::{Rng, SeedableRng};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::time::{Duration, Instant};

    /// 입력 신호 생성기: (절대 프레임 인덱스, 채널 수, 인터리브 버퍼).
    pub type SourceFn = dyn Fn(u64, usize, &mut [f32]) + Send + Sync;

    #[derive(Clone)]
    pub struct FakeBackend {
        pub in_rate: u32,
        pub out_rate: u32,
        pub channels: usize,
        /// 입력 콜백 크기. None이면 설정의 bufferSize.
        pub in_buffer: Option<usize>,
        /// 출력 콜백 크기. None이면 설정의 bufferSize.
        pub out_buffer: Option<usize>,
        /// 장치 클럭 오차(ppm). 양수면 실제보다 빠르게 돈다.
        pub in_drift_ppm: f64,
        pub out_drift_ppm: f64,
        /// 출력 스레드 시작 지연(µs).
        pub out_phase_us: u64,
        /// 콜백마다 0..이 값(µs) 사이로 늦게 깨어난다. 다음 콜백은 원래 일정으로 따라잡는다.
        pub wake_jitter_us: u64,
        pub open_delay: Duration,
        /// 이 시간 뒤 스트림 에러 1회 보고(장치 분리 흉내).
        pub error_after: Option<Duration>,
        pub source: Arc<SourceFn>,
        /// 출력 콜백이 기록한 샘플(인터리브, `channels`채널).
        pub sink: Arc<Mutex<Vec<f32>>>,
        pub record_output: bool,
        pub seed: u64,
        /// (주기, 멈춤): 출력 콜백 스레드가 주기마다 멈췄다가 몰아서 따라잡는다 → 출력 언더런 유발.
        pub out_stall: Option<(Duration, Duration)>,
        /// 이 시간 뒤 입력·출력 콜백이 멈춘다(장치 분리 흉내).
        pub halt_after: Option<Duration>,
        /// 공유하면 동시에 한 번만 열 수 있다(배타 장치 흉내).
        pub exclusive: Option<Arc<AtomicBool>>,
        /// 클럭 원점. 두 가짜 장치가 같은 원점을 쓰면 입력 인덱스와 출력 인덱스로 종단 간 지연을 잴 수 있다.
        /// None이면 열 때의 시각. 원점이 과거면 콜백은 현재 시각에 해당하는 인덱스부터 시작한다.
        pub t0: Option<Instant>,
        /// 출력 sink 첫 샘플의 절대 프레임 인덱스(원점 기준).
        /// sink 프레임 j의 재생 시각 = t0 + out_phase + (origin + j)/out_rate.
        pub sink_origin: Arc<AtomicU64>,
    }

    impl FakeBackend {
        pub fn new(rate: u32, source: Arc<SourceFn>) -> Self {
            Self {
                in_rate: rate,
                out_rate: rate,
                channels: 2,
                in_buffer: None,
                out_buffer: None,
                in_drift_ppm: 0.0,
                out_drift_ppm: 0.0,
                out_phase_us: 0,
                wake_jitter_us: 0,
                open_delay: Duration::ZERO,
                error_after: None,
                source,
                sink: Arc::new(Mutex::new(Vec::new())),
                record_output: true,
                seed: 1,
                out_stall: None,
                halt_after: None,
                exclusive: None,
                t0: None,
                sink_origin: Arc::new(AtomicU64::new(0)),
            }
        }

        pub fn silence(rate: u32) -> Self {
            Self::new(rate, Arc::new(|_, _, buf: &mut [f32]| buf.fill(0.0)))
        }

        /// 입출력 모두 같은 클럭 오차(같은 장치의 듀플렉스).
        pub fn with_drift(mut self, ppm: f64) -> Self {
            self.in_drift_ppm = ppm;
            self.out_drift_ppm = ppm;
            self
        }
    }

    struct FakeGuard {
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
        exclusive: Option<Arc<AtomicBool>>,
    }

    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            for t in self.threads.drain(..) {
                let _ = t.join();
            }
            if let Some(ex) = &self.exclusive {
                ex.store(false, Ordering::SeqCst);
            }
        }
    }

    /// `t0 + offset + period·(k + first)`에 `f(k)`를 부르는 실시간 루프.
    /// 기상 지연은 `jitter_us` 이내 난수이며, 늦어지면 다음 호출들이 몰려서(버스트) 따라잡는다.
    struct LoopCfg {
        t0: Instant,
        offset: Duration,
        period: f64,
        first: u64,
        jitter_us: u64,
        seed: u64,
        /// (주기, 멈춤): 주기마다 콜백 스레드가 이만큼 멈췄다가 몰아서 따라잡는다(CPU 스파이크 흉내).
        stall: Option<(Duration, Duration)>,
        /// 이 시간 뒤 콜백을 더 부르지 않는다(장치 분리 흉내).
        halt_after: Option<Duration>,
    }

    fn clocked_loop(stop: Arc<AtomicBool>, c: LoopCfg, mut f: impl FnMut(u64)) {
        // 실제 장치 콜백 스레드(cpal WASAPI/ASIO)는 시간 임계 우선순위로 돈다. 가짜도 같게 둔다.
        let _prio = crate::engine::prio::promote_current_thread();
        let mut rng = rand::rngs::StdRng::seed_from_u64(c.seed);
        let mut k: u64 = 0;
        let mut next_stall = c.stall.map(|(every, _)| every);
        while !stop.load(Ordering::Relaxed) {
            if c.halt_after.is_some_and(|h| c.t0.elapsed() >= h) {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            if let (Some(at), Some((every, len))) = (next_stall, c.stall) {
                if c.t0.elapsed() >= at {
                    std::thread::sleep(len);
                    next_stall = Some(at + every);
                }
            }
            let jitter = if c.jitter_us > 0 { Duration::from_micros(rng.gen_range(0..c.jitter_us)) } else { Duration::ZERO };
            let due = c.t0 + c.offset + Duration::from_secs_f64(c.period * (k + c.first) as f64) + jitter;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            f(k);
            k += 1;
        }
    }

    impl AudioBackend for FakeBackend {
        fn list_devices(&self) -> Result<Vec<DeviceInfo>, String> {
            let mk = |is_input| DeviceInfo {
                id: device_id(DriverKind::WasapiShared, "Fake"),
                name: "Fake".into(),
                driver: DriverKind::WasapiShared,
                is_input,
                is_default: true,
                channels: self.channels as u16,
                sample_rates: vec![if is_input { self.in_rate } else { self.out_rate }],
            };
            Ok(vec![mk(true), mk(false)])
        }

        fn open(
            &self,
            cfg: &EngineConfig,
            mut input: InputRt,
            mut output: OutputRt,
            counters: Arc<AudioCounters>,
        ) -> Result<(StreamInfo, Box<dyn Any>), String> {
            if input.required_device_channels() > self.channels {
                return Err("가짜 장치 채널 부족".into());
            }
            // 배타 장치 흉내: 이미 열려 있으면 실패(ASIO 드라이버 1개, WASAPI 배타, 고정 포트와 같은 성질)
            if let Some(ex) = &self.exclusive {
                if ex.swap(true, Ordering::SeqCst) {
                    return Err("장치가 이미 사용 중입니다".into());
                }
            }
            std::thread::sleep(self.open_delay);
            let stop = Arc::new(AtomicBool::new(false));
            let ch = self.channels;
            let in_buf = self.in_buffer.unwrap_or(cfg.buffer_size as usize).max(1);
            let out_buf = self.out_buffer.unwrap_or(cfg.buffer_size as usize).max(1);
            let t0 = self.t0.unwrap_or_else(Instant::now);
            // 원점이 과거면 지금 시각에 해당하는 콜백 인덱스부터 시작한다(밀린 콜백을 몰아서 부르지 않음).
            let start_k = move |period: f64, offset: f64| -> u64 {
                let el = t0.elapsed().as_secs_f64() - offset;
                if el <= 0.0 {
                    0
                } else {
                    (el / period).ceil() as u64
                }
            };

            let me = self.clone();
            let st = stop.clone();
            let in_period = in_buf as f64 / me.in_rate as f64 / (1.0 + me.in_drift_ppm * 1e-6);
            let in_thread = std::thread::Builder::new()
                .name("band-fake-in".into())
                .spawn(move || {
                    let mut buf = vec![0f32; in_buf * ch];
                    let seed = me.seed;
                    let k0 = start_k(in_period, 0.0);
                    // 입력 콜백 k는 블록 [k, k+1)×in_buf의 캡처가 끝난 t0 + (k+1)·period에 불린다.
                    let lc = LoopCfg {
                        t0,
                        offset: Duration::ZERO,
                        period: in_period,
                        first: 1 + k0,
                        jitter_us: me.wake_jitter_us,
                        seed,
                        stall: None,
                        halt_after: me.halt_after,
                    };
                    clocked_loop(st, lc, |k| {
                        (me.source)((k + k0) * in_buf as u64, ch, &mut buf);
                        input.process(&buf, ch);
                    });
                })
                .map_err(|e| format!("가짜 입력 스레드 생성 실패: {e}"))?;

            let me = self.clone();
            let st = stop.clone();
            let out_period = out_buf as f64 / me.out_rate as f64 / (1.0 + me.out_drift_ppm * 1e-6);
            let spawn_out = std::thread::Builder::new().name("band-fake-out".into()).spawn(move || {
                let mut buf = vec![0f32; out_buf * ch];
                let mut errored = false;
                let phase = Duration::from_micros(me.out_phase_us);
                let seed = me.seed.wrapping_add(17);
                let k0 = start_k(out_period, phase.as_secs_f64());
                me.sink_origin.store(k0 * out_buf as u64, Ordering::SeqCst);
                // 출력 콜백 k는 t0 + phase + k·period에 불려 재생 구간 [k, k+1)×out_buf를 채운다.
                let lc = LoopCfg {
                    t0,
                    offset: phase,
                    period: out_period,
                    first: k0,
                    jitter_us: me.wake_jitter_us,
                    seed,
                    stall: me.out_stall,
                    halt_after: me.halt_after,
                };
                clocked_loop(st, lc, |_k| {
                    output.process(&mut buf, ch);
                    if me.record_output {
                        me.sink.lock().extend_from_slice(&buf);
                    }
                    if !errored && me.error_after.is_some_and(|d| t0.elapsed() >= d) {
                        errored = true;
                        counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                    }
                });
            });
            let out_thread = match spawn_out {
                Ok(t) => t,
                Err(e) => {
                    stop.store(true, Ordering::SeqCst);
                    let _ = in_thread.join();
                    return Err(format!("가짜 출력 스레드 생성 실패: {e}"));
                }
            };
            let info = StreamInfo {
                in_rate: self.in_rate,
                out_rate: self.out_rate,
                in_channels: ch,
                out_channels: ch,
                buffer_frames: cfg.buffer_size,
                in_backend: BackendKind::Fake,
                out_backend: BackendKind::Fake,
                in_period_frames: in_buf as u32,
                out_period_frames: out_buf as u32,
            };
            Ok((info, Box::new(FakeGuard { stop, threads: vec![in_thread, out_thread], exclusive: self.exclusive.clone() })))
        }
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

    #[cfg(not(feature = "asio"))]
    #[test]
    fn asio_unsupported_without_feature() {
        use super::cpal_backend::CpalBackend;
        use crate::engine::mixer::MixControls;
        use crate::engine::rt::{input_ring, output_path};
        let counters = Arc::new(AudioCounters::default());
        let (p, _c) = input_ring();
        let (out_rt, _feed) = output_path(counters.clone(), Arc::new(MixControls::default()));
        let cfg = EngineConfig {
            input_driver: DriverKind::Asio,
            output_driver: DriverKind::Asio,
            ..Default::default()
        };
        let r = CpalBackend.open(
            &cfg,
            InputRt::new(p, counters.clone(), &[0], 1),
            out_rt,
            counters,
        );
        let err = r.expect_err("ASIO는 실패해야 함");
        assert!(err.contains("ASIO"), "{err}");
    }
}
