//! cpal 구현(WASAPI 공유·ASIO). cpal 콜백(RT) 포함. ASIO 싱글톤 우회는 모듈 머리(`device/mod.rs`) 참고.

use std::any::Any;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::engine::config::{DriverKind, EngineConfig, INTERNAL_RATE};
use crate::engine::rt::{InputRt, OutputRt};
use crate::engine::stats::AudioCounters;

use super::backend::AudioBackend;
use super::types::*;
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

/// `ASIOGetLatencies`(입력, 출력 프레임). asio-sys가 바인딩하지 않으므로 SDK 정적 라이브러리의
/// C++ 심볼을 직접 링크한다(asio-sys 바인딩과 같은 방식, `ASIOError ASIOGetLatencies(long*, long*)`).
/// 버퍼를 만든 뒤(스트림 생성 후)에만 의미가 있다.
#[cfg(all(feature = "asio", target_os = "windows", target_arch = "x86_64"))]
fn asio_latencies() -> Option<(u32, u32)> {
    use std::os::raw::c_long;
    unsafe extern "C" {
        #[link_name = "\u{1}?ASIOGetLatencies@@YAJPEAJ0@Z"]
        fn ASIOGetLatencies(input: *mut c_long, output: *mut c_long) -> c_long;
    }
    let (mut i, mut o): (c_long, c_long) = (0, 0);
    // SAFETY: SDK 함수는 드라이버가 없으면 ASE_NotPresent를 돌려주고 두 값을 0으로 쓴다. 포인터는 지역 변수다.
    let r = unsafe { ASIOGetLatencies(&mut i, &mut o) };
    // ASE_OK = 0
    (r == 0 && i >= 0 && o >= 0).then_some((i as u32, o as u32))
}

#[cfg(not(all(feature = "asio", target_os = "windows", target_arch = "x86_64")))]
fn asio_latencies() -> Option<(u32, u32)> {
    None
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
        // 드라이버 보고 지연: ASIO 듀플렉스만 알 수 있다(cpal WASAPI 공유는 알려 주지 않음).
        let reported = if duplex_asio { asio_latencies().unwrap_or((0, 0)) } else { (0, 0) };
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
            in_reported_latency_frames: reported.0,
            out_reported_latency_frames: reported.1,
            in_device_tag: device_tag(&in_dev.name().unwrap_or_default()),
            out_device_tag: device_tag(&out_dev.name().unwrap_or_default()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "asio"))]
    #[test]
    fn asio_unsupported_without_feature() {
        use super::CpalBackend;
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
