//! cpal 구현(WASAPI 공유·ASIO). cpal 콜백(RT) 포함. ASIO 싱글톤 우회는 모듈 머리(`device/mod.rs`) 참고.

use std::any::Any;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use band_dsp::config::{DriverKind, EngineConfig, INTERNAL_RATE};
use crate::engine::rt::{InputRt, OutputRt};
use band_dsp::stats::AudioCounters;

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

pub(crate) struct CpalBackend;

struct Streams {
    _input: cpal::Stream,
    _output: cpal::Stream,
    asio: bool,
    /// 드라이버 리셋 요청 감시를 거두는 함수(ASIO만, `asio_reset_hook`). 스트림보다 먼저 거둔다.
    unhook: Option<Box<dyn FnOnce()>>,
}

impl Drop for Streams {
    fn drop(&mut self) {
        if let Some(unhook) = self.unhook.take() {
            unhook();
        }
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

/// ASIO SDK `asio.h`의 `kAsioResetRequest` 선택자 값(`kAsioSelectorSupported = 1`, `kAsioEngineVersion`, `kAsioResetRequest`).
const K_ASIO_RESET_REQUEST: i64 = 3;

/// 우리가 열면서 드라이버 샘플레이트를 바꿨을 때(cpal이 `set_sample_rate`를 부름), 훅을 건 뒤 이 시간 안에 오는 리셋 요청은
/// 그 변경에 대한 드라이버의 응답으로 보고 세지 않는다(#261 리뷰 M2). 드라이버에 따라 `ASIOSetSampleRate`가 리셋 요청을
/// 비동기로 보내 play 뒤에 도착하는데, 그 변경은 이미 새 버퍼로 반영했으므로 다시 열 필요가 없다 — 세면 재시작이 한 번 더 났다.
const SELF_RATE_RESET_GRACE: Duration = Duration::from_secs(2);

/// 레이트를 바꾸지 않은 열기에도 훅을 건 직후 이 시간 안의 리셋 요청은 세지 않는다(#261 재리뷰 L2).
/// cpal 0.15.3은 `build_*_stream_raw` 안에서 `ASIOStart`를 부르고(`play()`는 원자값 하나만 쓴다), 출력 스트림을 만들 때
/// 돌던 드라이버를 멈추고 버퍼를 다시 만든 뒤 한 번 더 시작한다. 훅은 그 두 번째 `ASIOStart`가 돌아온 뒤에 걸리므로 시작
/// **호출 중**의 요청은 세지 않지만, 일부 USB 드라이버가 시작 직후 수십 ms 뒤에 비동기로 보내는 거짓 요청은 그대로 셌다 —
/// 그러면 재시작이 되풀이돼 반복 상한에서 연주가 멈춘다. 사람이 그 사이에 제어판을 조작할 수는 없는 길이로 잡는다.
const START_RESET_GRACE: Duration = Duration::from_millis(300);

/// 메시지 콜백 한 번의 판정.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(feature = "asio", test)), allow(dead_code))]
enum ResetVerdict {
    /// 리셋 요청이 아니다(다른 선택자).
    NotReset,
    /// 리셋 요청이지만 유예 안이라 세지 않는다(시작 직후 거짓 요청, 우리 레이트 변경에 대한 응답). 버린 횟수는 따로 센다.
    Ignored,
    /// 다시 열어야 하는 리셋 요청.
    Counted,
}

/// 메시지 콜백 한 번을 리셋 요청으로 셀지(순수 함수 — SDK 없이 시험한다, `asio_reset_hook`).
/// `since_hook` = 훅을 건 뒤 지난 시간, `rate_set_by_us` = 이번 열기에서 드라이버 레이트를 우리가 바꿨는지.
#[cfg_attr(not(any(feature = "asio", test)), allow(dead_code))]
fn reset_verdict(selector: i64, since_hook: Duration, rate_set_by_us: bool) -> ResetVerdict {
    if selector != K_ASIO_RESET_REQUEST {
        return ResetVerdict::NotReset;
    }
    let grace = if rate_set_by_us { SELF_RATE_RESET_GRACE } else { START_RESET_GRACE };
    if since_hook < grace {
        ResetVerdict::Ignored
    } else {
        ResetVerdict::Counted
    }
}

/// 드라이버 메시지 콜백의 몸통(피처 밖 — SDK 없이 시험한다). 판정 결과에 따라 원자 덧셈 하나만 한다:
/// 센 요청은 `device_reset_requests`(워커 `ResetWatch`가 다시 열기로 바꾼다), 유예로 버린 요청은 `device_reset_ignored`
/// (워커가 로그로 남긴다 — 버린 뒤 드라이버 설정이 어긋났는지 가릴 단서, #261 재리뷰 L3).
#[cfg_attr(not(any(feature = "asio", test)), allow(dead_code))]
struct ResetFilter {
    counters: Arc<AudioCounters>,
    rate_set_by_us: bool,
}

#[cfg_attr(not(any(feature = "asio", test)), allow(dead_code))]
impl ResetFilter {
    fn on_message(&self, selector: i64, since_hook: Duration) {
        match reset_verdict(selector, since_hook, self.rate_set_by_us) {
            ResetVerdict::Counted => {
                self.counters.device_reset_requests.fetch_add(1, Ordering::Relaxed);
            }
            ResetVerdict::Ignored => {
                self.counters.device_reset_ignored.fetch_add(1, Ordering::Relaxed);
            }
            ResetVerdict::NotReset => {}
        }
    }
}

/// 열린 뒤 드라이버가 보고하는 레이트가 엔진이 고른 레이트와 다르면 그 값(#261 재리뷰 L3). 드라이버 값을 못 읽으면 판단하지 않는다.
fn rate_mismatch(selected: u32, driver: Option<u32>) -> Option<u32> {
    driver.filter(|&d| d != selected)
}

/// 드라이버 리셋 요청(`kAsioResetRequest`)을 `AudioCounters::device_reset_requests`로 센다(P1-11, 설계 문서 1.1절).
///
/// cpal 0.15.3은 이 요청을 버린다: asio-sys 0.2.6의 `asio_message`가 리셋 요청을 등록된 메시지 콜백
/// (`Driver::add_message_callback`)에 넘기지만 cpal ASIO 백엔드는 콜백을 등록하지 않고, 에러 콜백도 쓰지 않는다
/// (`host/asio/stream.rs`의 `_error_callback`). 그래서 cpal이 이미 연 드라이버 핸들(`cpal::platform::AsioDevice::driver`,
/// pub)에 우리 콜백을 직접 건다 — 패치·포크 없이. asio-sys의 메시지 콜백 목록은 프로세스 전역이고 드라이버는 하나다.
/// 콜백을 부르는 스레드는 ASIO 규약이 정하지 않는다: 대개 드라이버의 메시지 스레드지만 드라이버에 따라 오디오(bufferSwitch)
/// 스레드일 수 있다. 그래서 우리 콜백은 비교·`Instant` 읽기(QPC, 락·할당 없음)·원자 덧셈만 한다(판단·재시작은 워커의
/// `ResetWatch`와 프론트). asio-sys `asio_message`는 요청마다 콜백 목록 락과 `Vec` 할당을 한 번 한다 — 리셋 1회당 한 번이고
/// 곧 다시 열 것이라 실해는 없다(#261 리뷰 L1).
/// 스트림을 만들고 `play`한 뒤에 건다(실제 `ASIOStart`는 스트림을 만들 때 불린다 — `START_RESET_GRACE` 주석). 일부 USB ASIO
/// 드라이버가 시작 중에 보내는 가짜 리셋 요청을 세지 않는다(cpal 0.18도 같은 이유로 시작 중 요청을 무시한다). 시작 직후의 비동기
/// 거짓 요청과 우리가 레이트를 바꾼 열기의 응답은 유예로 거른다(`reset_verdict`). 돌려준 함수를 부르면 콜백을 거둔다.
#[cfg(feature = "asio")]
fn asio_reset_hook(dev: &cpal::Device, counters: &Arc<AudioCounters>, rate_set_by_us: bool) -> Option<Box<dyn FnOnce()>> {
    match dev.as_inner() {
        cpal::platform::DeviceInner::Asio(d) => {
            let filter = ResetFilter { counters: counters.clone(), rate_set_by_us };
            let hooked_at = std::time::Instant::now();
            // asio-sys 0.2.6은 리셋 요청에만 메시지 콜백을 부르지만, 다른 판이 다른 선택자를 넘겨도 리셋만 센다.
            let id = d.driver.add_message_callback(move |selector| filter.on_message(selector as i64, hooked_at.elapsed()));
            let driver = d.driver.clone();
            let unhook: Box<dyn FnOnce()> = Box::new(move || driver.remove_message_callback(id));
            Some(unhook)
        }
        _ => None,
    }
}

#[cfg(not(feature = "asio"))]
fn asio_reset_hook(_dev: &cpal::Device, _counters: &Arc<AudioCounters>, _rate_set_by_us: bool) -> Option<Box<dyn FnOnce()>> {
    None
}

/// 드라이버가 지금 보고하는 샘플레이트(`ASIOGetSampleRate`). 여는 스레드에서 스트림을 만든 뒤 한 번만 부른다(콜백 밖).
#[cfg(feature = "asio")]
fn asio_driver_rate(dev: &cpal::Device) -> Option<u32> {
    match dev.as_inner() {
        cpal::platform::DeviceInner::Asio(d) => d.driver.sample_rate().ok().map(|r| r.round() as u32),
        _ => None,
    }
}

#[cfg(not(feature = "asio"))]
fn asio_driver_rate(_dev: &cpal::Device) -> Option<u32> {
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
        let c_out = counters.clone();
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
        // 드라이버 리셋 요청 감시(ASIO만): play 뒤에 건다(시작 중 가짜 요청 제외, `asio_reset_hook`)
        // ASIO 드라이버 레이트를 우리가 바꿨는가 — 제어판에서 44.1k로 바꿔도 엔진은 48k로 다시 연다(설계 문서 1.1절, #261 리뷰 M2)
        let asio_sel = match (cfg.input_driver, cfg.output_driver) {
            (DriverKind::Asio, _) => Some(&in_sel),
            (_, DriverKind::Asio) => Some(&out_sel),
            _ => None,
        };
        let rate_set_by_us = asio_sel.is_some_and(|s| s.driver_rate != s.config.sample_rate.0);
        if let Some(s) = asio_sel.filter(|_| rate_set_by_us) {
            log::info!("ASIO 드라이버 샘플레이트 {}Hz → {}Hz로 맞춤(엔진 내부 레이트)", s.driver_rate, s.config.sample_rate.0);
        }
        let asio_dev = match (cfg.input_driver, cfg.output_driver) {
            (DriverKind::Asio, _) => Some(&in_dev),
            (_, DriverKind::Asio) => Some(&out_dev),
            _ => None,
        };
        let unhook = asio_dev.and_then(|d| asio_reset_hook(d, &counters, rate_set_by_us));
        // 열린 뒤 드라이버 레이트가 고른 레이트와 다르면(외부 클록에 묶여 `ASIOSetSampleRate`가 성공을 돌려주고도 그대로인 장치,
        // 유예 중에 버린 요청이 사실은 레이트 변경이던 경우) 엔진은 48k라고 믿은 채 다른 레이트로 돈다(음높이·클록 어긋남).
        // 장치 없이는 드라이버마다 이 조회가 얼마나 믿을 만한지 알 수 없어 열기를 실패시키지 않고 눈에 띄게 남긴다(#261 재리뷰 L3)
        if let (Some(s), Some(d)) = (asio_sel, asio_dev) {
            if let Some(actual) = rate_mismatch(s.config.sample_rate.0, asio_driver_rate(d)) {
                log::error!(
                    "ASIO 드라이버 레이트 {actual}Hz가 엔진이 연 {}Hz와 다릅니다 — 음높이·박자가 어긋날 수 있습니다(외부 클록 동기·제어판 샘플레이트 확인)",
                    s.config.sample_rate.0
                );
            }
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
            // ASIO는 실제로 열린 크기(드라이버가 한 크기만 받으면 그 크기, `opened_buffer_frames`) — 루프백 보정 키에 들어간다
            buffer_frames: asio_sel.map_or(cfg.buffer_size, |s| s.buffer_frames),
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
        Ok((info, Box::new(Streams { _input: input_stream, _output: output_stream, asio: any_asio, unhook })))
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
    /// 열기 전 드라이버(장치 기본 설정)의 레이트. ASIO에서 `config`와 다르면 cpal이 드라이버 레이트를 바꾼다.
    driver_rate: u32,
    /// 열릴 버퍼 크기(프레임, `opened_buffer_frames`).
    buffer_frames: u32,
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
        driver_rate: def.sample_rate().0,
        buffer_frames: opened_buffer_frames(&buf_support, buffer),
    })
}

/// 요청한 버퍼로 열면 실제로 열리는 크기(#261 재리뷰 L1). 범위 안이면 요청값(`Fixed`), 범위 밖이고 드라이버가 한 크기만
/// 받으면(min = max — Focusrite처럼 제어판에서 고른 크기만 보고) 그 크기다. 범위가 넓은데 요청이 밖이면 드라이버 선호 크기로
/// 열리지만 asio-sys가 그 값을 알려 주지 않아 요청값을 그대로 둔다(남은 빈틈 — 실측 콜백 크기는 `deviceBuffer`로 보인다).
fn opened_buffer_frames(support: &cpal::SupportedBufferSize, buffer: u32) -> u32 {
    match support {
        cpal::SupportedBufferSize::Range { min, max } if (buffer < *min || buffer > *max) && min == max => *min,
        _ => buffer,
    }
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

#[cfg(all(test, not(feature = "asio")))]
mod tests {
    use super::*;

    #[test]
    fn asio_unsupported_without_feature() {
        use super::CpalBackend;
        use band_dsp::mixer::MixControls;
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

    /// 메시지 콜백 판정(#261 리뷰 M2·L3 — asio 피처 없이도 시험): 리셋 선택자만 세고, 우리가 레이트를 바꾼 열기에서는
    /// 훅 뒤 2초 안의 요청(그 변경에 대한 드라이버 응답)을 거른다. 레이트를 바꾸지 않았으면 곧바로 센다.
    #[test]
    fn reset_selector_and_self_rate_change_grace() {
        use ResetVerdict::*;
        let ms = Duration::from_millis;
        assert_eq!(reset_verdict(1, ms(5_000), false), NotReset, "kAsioSelectorSupported 등 다른 선택자는 세지 않는다");
        assert_eq!(reset_verdict(2, ms(5_000), false), NotReset);
        assert_eq!(reset_verdict(3, ms(0), true), Ignored, "우리가 레이트를 바꾼 직후 요청은 그 응답");
        assert_eq!(reset_verdict(3, ms(1_999), true), Ignored);
        assert_eq!(reset_verdict(3, ms(2_000), true), Counted, "유예 뒤에는 사용자 변경으로 센다");
        // 레이트를 바꾸지 않은 열기: 시작 직후 짧은 유예(비동기 거짓 요청)만 거르고 그 뒤는 곧바로 센다(#261 재리뷰 L2)
        assert_eq!(reset_verdict(3, ms(0), false), Ignored);
        assert_eq!(reset_verdict(3, ms(299), false), Ignored);
        assert_eq!(reset_verdict(3, ms(300), false), Counted);
    }

    /// 콜백 몸통(#261 재리뷰 L3): 센 요청과 유예로 버린 요청을 따로 센다 — 버린 것은 워커가 로그로 남긴다.
    #[test]
    fn reset_filter_counts_and_ignored_separately() {
        let ms = Duration::from_millis;
        let c = Arc::new(AudioCounters::default());
        let get = |c: &AudioCounters| (c.device_reset_requests.load(Ordering::Relaxed), c.device_reset_ignored.load(Ordering::Relaxed));
        let f = ResetFilter { counters: c.clone(), rate_set_by_us: true };
        f.on_message(3, ms(100)); // 우리 레이트 변경의 응답
        f.on_message(3, ms(1_500)); // 유예 안의 요청(진짜일 수도 있다) — 세지 않되 버린 수에 남는다
        assert_eq!(get(&c), (0, 2));
        f.on_message(1, ms(3_000));
        assert_eq!(get(&c), (0, 2), "다른 선택자는 어느 쪽에도 세지 않는다");
        f.on_message(3, ms(3_000));
        assert_eq!(get(&c), (1, 2));
        let f = ResetFilter { counters: c.clone(), rate_set_by_us: false };
        f.on_message(3, ms(50));
        f.on_message(3, ms(400));
        assert_eq!(get(&c), (2, 3));
    }

    /// 루프백 보정 키에 들어가는 열린 버퍼 크기(#261 재리뷰 L1): 드라이버가 한 크기만 받으면 요청값이 아니라 그 크기.
    #[test]
    fn opened_buffer_is_driver_size_when_range_is_single() {
        use cpal::SupportedBufferSize::{Range, Unknown};
        assert_eq!(opened_buffer_frames(&Range { min: 64, max: 1024 }, 128), 128, "범위 안이면 요청값");
        assert_eq!(opened_buffer_frames(&Range { min: 256, max: 256 }, 128), 256, "제어판에서 256으로 바꾼 Focusrite");
        assert_eq!(opened_buffer_frames(&Range { min: 256, max: 256 }, 256), 256);
        assert_eq!(opened_buffer_frames(&Range { min: 256, max: 1024 }, 128), 128, "선호 크기를 알 수 없으면 요청값(남은 빈틈)");
        assert_eq!(opened_buffer_frames(&Unknown, 128), 128);
    }

    #[test]
    fn rate_mismatch_only_when_driver_reports_another_rate() {
        assert_eq!(rate_mismatch(48_000, Some(48_000)), None);
        assert_eq!(rate_mismatch(48_000, Some(44_100)), Some(44_100));
        assert_eq!(rate_mismatch(48_000, None), None, "드라이버 값을 못 읽으면 판단하지 않는다");
    }
}
