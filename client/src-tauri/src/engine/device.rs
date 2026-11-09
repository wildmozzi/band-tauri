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

/// 열린 스트림 정보.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub in_rate: u32,
    pub out_rate: u32,
    pub in_channels: usize,
    pub out_channels: usize,
    /// 요청한 장치 버퍼 크기(프레임). 실제 크기는 콜백 길이로 측정한다.
    pub buffer_frames: u32,
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
                // 듀플렉스는 같은 레이트여야 한다
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
            let info = StreamInfo {
                in_rate: in_sel.config.sample_rate.0,
                out_rate: out_sel.config.sample_rate.0,
                in_channels: in_ch,
                out_channels: out_ch,
                buffer_frames: cfg.buffer_size,
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
            config: StreamConfig {
                channels,
                sample_rate: cpal::SampleRate(rate),
                buffer_size: cpal::BufferSize::Fixed(buffer),
            },
            format,
        })
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
// 가짜 장치 (테스트/통합 테스트용)
// ============================================================================

pub mod fake {
    //! 실제 장치 없이 엔진 전체를 돌리기 위한 가짜 오디오 장치.
    //! 전용 스레드가 실시간 속도(드리프트 ppm 반영)로 입력/출력 콜백을 호출한다.

    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    /// 입력 신호 생성기: (절대 프레임 인덱스, 채널 수, 인터리브 버퍼).
    pub type SourceFn = dyn Fn(u64, usize, &mut [f32]) + Send + Sync;

    #[derive(Clone)]
    pub struct FakeBackend {
        pub rate: u32,
        pub channels: usize,
        /// 장치 클럭 오차(ppm). 양수면 실제보다 빠르게 돈다.
        pub drift_ppm: f64,
        pub source: Arc<SourceFn>,
        /// 출력 콜백이 기록한 샘플(인터리브, `channels`채널).
        pub sink: Arc<Mutex<Vec<f32>>>,
        pub record_output: bool,
    }

    impl FakeBackend {
        pub fn new(rate: u32, source: Arc<SourceFn>) -> Self {
            Self { rate, channels: 2, drift_ppm: 0.0, source, sink: Arc::new(Mutex::new(Vec::new())), record_output: true }
        }

        pub fn silence(rate: u32) -> Self {
            Self::new(rate, Arc::new(|_, _, buf: &mut [f32]| buf.fill(0.0)))
        }
    }

    struct FakeGuard {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
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
                sample_rates: vec![self.rate],
            };
            Ok(vec![mk(true), mk(false)])
        }

        fn open(
            &self,
            cfg: &EngineConfig,
            mut input: InputRt,
            mut output: OutputRt,
            _counters: Arc<AudioCounters>,
        ) -> Result<(StreamInfo, Box<dyn Any>), String> {
            if input.required_device_channels() > self.channels {
                return Err("가짜 장치 채널 부족".into());
            }
            let stop = Arc::new(AtomicBool::new(false));
            let me = self.clone();
            let buffer = cfg.buffer_size as usize;
            let st = stop.clone();
            let thread = std::thread::Builder::new()
                .name("band-fake-audio".into())
                .spawn(move || {
                    let ch = me.channels;
                    let mut inbuf = vec![0f32; buffer * ch];
                    let mut outbuf = vec![0f32; buffer * ch];
                    let period = buffer as f64 / me.rate as f64 / (1.0 + me.drift_ppm * 1e-6);
                    let t0 = Instant::now();
                    let mut k: u64 = 0;
                    while !st.load(Ordering::Relaxed) {
                        (me.source)(k * buffer as u64, ch, &mut inbuf);
                        input.process(&inbuf, ch);
                        output.process(&mut outbuf, ch);
                        if me.record_output {
                            me.sink.lock().extend_from_slice(&outbuf);
                        }
                        k += 1;
                        let due = t0 + Duration::from_secs_f64(period * k as f64);
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        }
                    }
                })
                .map_err(|e| format!("가짜 오디오 스레드 생성 실패: {e}"))?;
            let info = StreamInfo {
                in_rate: self.rate,
                out_rate: self.rate,
                in_channels: self.channels,
                out_channels: self.channels,
                buffer_frames: cfg.buffer_size,
            };
            Ok((info, Box::new(FakeGuard { stop, thread: Some(thread) })))
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
        use crate::engine::rt::{input_ring, output_ring};
        let counters = Arc::new(AudioCounters::default());
        let (p, _c) = input_ring();
        let (_p2, c2) = output_ring();
        let cfg = EngineConfig {
            input_driver: DriverKind::Asio,
            output_driver: DriverKind::Asio,
            ..Default::default()
        };
        let r = CpalBackend.open(
            &cfg,
            InputRt::new(p, counters.clone(), &[0], 1),
            OutputRt::new(c2, counters.clone()),
            counters,
        );
        let err = r.expect_err("ASIO는 실패해야 함");
        assert!(err.contains("ASIO"), "{err}");
    }
}
