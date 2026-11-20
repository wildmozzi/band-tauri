//! WASAPI 스트림: 한 방향 스트림(`DirStream`, 독점·저지연 공유 열기), 이벤트 루프(`drive_loop`·`run_loop`,
//! RT 경로: WASAPI 호출·대기만, 할당·락·로그 없음), 엔진용 열기(`open_engine_streams`)와 기본 장치 감시
//! (이동만, 엔진 ⑥-5).

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
    AUDCLNT_E_ENDPOINT_CREATE_FAILED, AUDCLNT_E_INVALID_DEVICE_PERIOD, AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

use super::enumerate::{
    activate, default_endpoint_id, endpoint_id, enumerator, err, find_device, friendly_name, mix_format, pick_exclusive_format, ComGuard,
};
use super::format::{classify, f32_to_i24, i24_to_f32, ErrClass, SampleKind, ShareMode, StreamFormat};
use band_dsp::config::EngineConfig;
use crate::engine::device::types::{device_name_from_id, device_tag, StreamInfo};
use crate::engine::rt::{InputRt, OutputRt};
use band_dsp::stats::AudioCounters;

const HNS_PER_SEC: f64 = 10_000_000.0;
/// 변환 버퍼(f32 샘플): 무음 캡처 패킷의 0, 24비트 패킹 변환. 독점 192kHz 8채널 50ms도 덮는다.
/// 더 큰 패킷은 이 크기로 나눠 처리한다.
pub(super) const SCRATCH_SAMPLES: usize = 192_000 / 20 * 8;
/// 이벤트 대기 타임아웃. 이 시간 동안 이벤트가 없으면 스트림 상태를 확인한다(L1).
const WAIT_TIMEOUT_MS: u32 = 100;
/// 같은 방향 연속 일시 오류가 이 횟수면 그 스트림을 재시작한다.
pub(crate) const RESTART_AFTER: u32 = 4;
/// 성공한 서비스 없이 재시작이 이 횟수 이어지면 치명.
pub(crate) const MAX_RESTARTS: u32 = 3;
/// 연속 일시 오류(재시작 실패 포함)가 이 횟수면 치명(독점 2~3ms 주기 ≈ 0.1~0.15초).
pub(crate) const FAIL_AFTER: u32 = 50;

// ============================================================================
// 한 방향 스트림
// ============================================================================

/// 열린 한 방향(캡처 또는 렌더) 스트림. 만든 스레드에서만 쓴다.
pub(crate) struct DirStream {
    client: IAudioClient,
    render: Option<IAudioRenderClient>,
    capture: Option<IAudioCaptureClient>,
    event: HANDLE,
    pub(crate) format: StreamFormat,
    pub(crate) mode: ShareMode,
    /// 장치 버퍼 크기(프레임).
    pub(crate) buffer_frames: u32,
    /// 이벤트 주기(프레임). 독점은 버퍼 크기와 같다.
    pub(crate) period_frames: u32,
    pub(crate) endpoint_id: String,
    pub(crate) name: String,
    started: bool,
}

impl Drop for DirStream {
    fn drop(&mut self) {
        // SAFETY: 이 스레드에서 만든 객체와 핸들이다.
        unsafe {
            if self.started {
                let _ = self.client.Stop();
            }
            let _ = CloseHandle(self.event);
        }
    }
}

impl DirStream {
    /// 엔드포인트를 지정한 모드로 연다. `min_channels`: 필요한 최소 장치 채널 수(입력 라우팅).
    pub(crate) fn open(en: &IMMDeviceEnumerator, is_input: bool, name: Option<&str>, mode: ShareMode, min_channels: u16) -> Result<Self, String> {
        let dev = find_device(en, is_input, name)?;
        let endpoint_id = endpoint_id(&dev);
        let dev_name = friendly_name(&dev);
        let (client, format, buffer_frames, period_frames) = match mode {
            ShareMode::Exclusive => open_exclusive(&dev, min_channels)?,
            ShareMode::Shared3 => open_shared3(&dev, min_channels)?,
        };
        // SAFETY: 자동 리셋 이벤트 생성과 등록. 핸들은 Drop에서 닫는다.
        let event = unsafe { CreateEventW(None, false, false, None) }.map_err(|e| err("이벤트 생성 실패", e))?;
        let mut s = DirStream {
            client,
            render: None,
            capture: None,
            event,
            format,
            mode,
            buffer_frames,
            period_frames,
            endpoint_id,
            name: dev_name,
            started: false,
        };
        // SAFETY: 초기화된 클라이언트에 이벤트를 등록하고 서비스 인터페이스를 얻는다.
        unsafe {
            s.client.SetEventHandle(s.event).map_err(|e| err("이벤트 등록 실패", e))?;
            if is_input {
                s.capture = Some(s.client.GetService::<IAudioCaptureClient>().map_err(|e| err("캡처 클라이언트 실패", e))?);
            } else {
                s.render = Some(s.client.GetService::<IAudioRenderClient>().map_err(|e| err("렌더 클라이언트 실패", e))?);
            }
        }
        Ok(s)
    }

    pub(crate) fn period_ms(&self) -> f64 {
        self.period_frames as f64 * 1000.0 / self.format.rate.max(1) as f64
    }

    /// 렌더: 시작 전 무음 한 주기(독점은 버퍼 전체)를 채워 첫 이벤트까지 끊기지 않게 한다.
    fn prefill(&self) -> windows::core::Result<()> {
        let Some(r) = &self.render else {
            return Ok(());
        };
        let n = match self.mode {
            ShareMode::Exclusive => self.buffer_frames,
            ShareMode::Shared3 => self.period_frames.min(self.buffer_frames),
        };
        // SAFETY: 시작 전 버퍼에 n ≤ 버퍼 크기 프레임을 무음으로 기록한다.
        unsafe {
            r.GetBuffer(n)?;
            r.ReleaseBuffer(n, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
        }
    }

    pub(super) fn start(&mut self) -> Result<(), String> {
        self.prefill().map_err(|e| err("렌더 무음 prefill 실패", e))?;
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.Start() }.map_err(|e| err("스트림 시작 실패", e))?;
        self.started = true;
        Ok(())
    }

    /// 일시 오류가 반복될 때의 재시작: Stop → Reset → (렌더) 무음 prefill → Start.
    fn restart(&self) -> windows::core::Result<()> {
        // SAFETY: 이 스레드에서 연 초기화된 클라이언트.
        unsafe {
            let _ = self.client.Stop();
            self.client.Reset()?;
            self.prefill()?;
            self.client.Start()
        }
    }

    /// `GetStreamLatency`(ms). 실패하면 0.
    pub(crate) fn stream_latency_ms(&self) -> f64 {
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.GetStreamLatency() }.map(|h| h as f64 / 10_000.0).unwrap_or(0.0)
    }

    /// 상태 확인(이벤트가 멈췄을 때): 장치가 무효화됐으면 오류를 돌려준다.
    fn check(&self) -> windows::core::Result<()> {
        // SAFETY: 초기화된 클라이언트.
        unsafe { self.client.GetCurrentPadding() }.map(|_| ())
    }
}

fn open_exclusive(dev: &IMMDevice, min_channels: u16) -> Result<(IAudioClient, StreamFormat, u32, u32), String> {
    let client: IAudioClient = activate(dev)?;
    let (mix, mix_mask) = mix_format(&client);
    let mix_ch = mix.map(|f| f.channels).unwrap_or(2);
    let (format, mask) = pick_exclusive_format(&client, mix_mask, mix_ch, min_channels)
        .ok_or_else(|| format!("독점 모드로 지원하는 포맷이 없습니다(필요 채널 {min_channels})"))?;
    let (mut def, mut min) = (0i64, 0i64);
    // SAFETY: 출력 포인터는 지역 변수.
    unsafe { client.GetDevicePeriod(Some(&mut def), Some(&mut min)) }.map_err(|e| err("장치 주기 조회 실패", e))?;
    let wfx = format.to_wfx_masked(mask);
    let init = |c: &IAudioClient, period: i64| {
        // SAFETY: wfx는 호출 동안 살아 있다. 이벤트 구동 독점 모드는 버퍼 길이 = 주기.
        unsafe { c.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, period, &wfx.Format, None) }
    };
    // 초기화에 실패한 클라이언트는 독점 핸들을 쥘 수 있어 먼저 해제하고 새로 활성화한다(L2, MS 예제 순서).
    let reinit = |old: IAudioClient, period: i64, what: &str| -> Result<IAudioClient, String> {
        drop(old);
        let c: IAudioClient = activate(dev)?;
        init(&c, period).map_err(|e| err(what, e))?;
        Ok(c)
    };
    let client = match init(&client, min) {
        Ok(()) => client,
        Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // 장치가 정렬된 크기를 알려 준다: 그 크기로 주기를 다시 계산하고 새 클라이언트로 재초기화.
            // SAFETY: 초기화 실패 후에도 GetBufferSize는 정렬된 크기를 돌려준다(문서화된 절차).
            let frames = unsafe { client.GetBufferSize() }.map_err(|e| err("정렬 버퍼 크기 조회 실패", e))?;
            let period = (HNS_PER_SEC * frames as f64 / format.rate as f64 + 0.5) as i64;
            reinit(client, period, "독점 모드 재초기화 실패")?
        }
        // 최소 주기를 거절하는 드라이버: 공유로 내려가기 전에 기본 주기로 한 번 더 독점을 시도한다(L2).
        Err(e) if (e.code() == AUDCLNT_E_INVALID_DEVICE_PERIOD || e.code() == AUDCLNT_E_ENDPOINT_CREATE_FAILED) && def > min => {
            log::warn!("독점 최소 주기 거절({}), 기본 주기로 재시도", err("Initialize", e));
            reinit(client, def, "독점 모드(기본 주기) 초기화 실패")?
        }
        Err(e) => return Err(err("독점 모드 초기화 실패", e)),
    };
    // SAFETY: 초기화된 클라이언트.
    let frames = unsafe { client.GetBufferSize() }.map_err(|e| err("버퍼 크기 조회 실패", e))?;
    Ok((client, format, frames, frames))
}

fn open_shared3(dev: &IMMDevice, min_channels: u16) -> Result<(IAudioClient, StreamFormat, u32, u32), String> {
    let c3: IAudioClient3 = activate(dev)?;
    // SAFETY: 믹스 포맷 포인터는 초기화가 끝날 때까지 쓰고 해제한다.
    unsafe {
        let p = c3.GetMixFormat().map_err(|e| err("믹스 포맷 조회 실패", e))?;
        let result = (|| {
            let format = StreamFormat::from_wfx(p).ok_or("믹스 포맷이 지원하지 않는 샘플 형식입니다")?;
            if format.channels < min_channels {
                return Err(format!("장치 채널 수({})가 선택한 입력 채널보다 적습니다", format.channels));
            }
            let (mut d, mut f, mut mn, mut mx) = (0u32, 0u32, 0u32, 0u32);
            c3.GetSharedModeEnginePeriod(p, &mut d, &mut f, &mut mn, &mut mx)
                .map_err(|e| err("공유 모드 엔진 주기 조회 실패", e))?;
            c3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, mn, p, None)
                .map_err(|e| err("저지연 공유 스트림 초기화 실패", e))?;
            let frames = c3.GetBufferSize().map_err(|e| err("버퍼 크기 조회 실패", e))?;
            Ok((format, frames, mn))
        })();
        CoTaskMemFree(Some(p as *const _));
        let (format, frames, period) = result?;
        let client: IAudioClient = c3.cast().map_err(|e| err("IAudioClient 변환 실패", e))?;
        Ok((client, format, frames, period))
    }
}

// ============================================================================
// 이벤트 루프
// ============================================================================

/// 장치 버퍼 뷰. 24비트 패킹은 루프가 f32로 변환해 넘기므로 여기에 없다.
pub(crate) enum DevBuf<'a> {
    F32(&'a mut [f32]),
    I32(&'a mut [i32]),
    I16(&'a mut [i16]),
}

impl DevBuf<'_> {
    /// # Safety
    /// `p`는 `frames × channels` 샘플을 담은 유효한 장치 버퍼여야 하고 반환값보다 오래 살아야 한다.
    /// `f.kind`는 `I24`가 아니어야 한다(호출자가 변환 경로로 보낸다).
    unsafe fn from_raw<'a>(p: *mut u8, frames: u32, f: &StreamFormat) -> DevBuf<'a> {
        let n = frames as usize * f.channels as usize;
        // SAFETY: 호출자 보장. WASAPI 버퍼는 샘플 정렬되어 있다.
        unsafe {
            match f.kind {
                SampleKind::F32 => DevBuf::F32(std::slice::from_raw_parts_mut(p as *mut f32, n)),
                SampleKind::I32 => DevBuf::I32(std::slice::from_raw_parts_mut(p as *mut i32, n)),
                SampleKind::I16 | SampleKind::I24 => DevBuf::I16(std::slice::from_raw_parts_mut(p as *mut i16, n)),
            }
        }
    }
}

/// 루프 종료 사유.
#[derive(Debug)]
pub(crate) enum LoopEnd {
    Stopped,
    /// 장치 분리·무효화(`deviceLost`).
    DeviceLost(windows::core::Error),
    /// 치명 오류 또는 재시도로 복구하지 못한 반복 오류(`streamFailed`).
    Failed(windows::core::Error),
}

/// 이벤트 대기 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitResult {
    Timeout,
    /// 신호가 온 스트림 인덱스.
    Ready(usize),
    /// 대기 자체가 실패(핸들 무효).
    Failed,
}

/// 이벤트 루프가 쓰는 장치 I/O. 실제 구현은 `WasapiIo`, 테스트는 가짜 HRESULT 시퀀스.
pub(crate) trait LoopIo {
    fn streams(&self) -> usize;
    fn wait(&mut self, timeout_ms: u32) -> WaitResult;
    /// 신호가 온 스트림 처리(캡처: 쌓인 패킷 전부, 렌더: 한 주기).
    fn service(&mut self, idx: usize) -> windows::core::Result<()>;
    /// 이벤트가 멈췄을 때 스트림 상태 확인.
    fn check(&mut self, idx: usize) -> windows::core::Result<()>;
    /// Stop → Reset → prefill → Start.
    fn restart(&mut self, idx: usize) -> windows::core::Result<()>;
}

/// 방향별 연속 실패 상태.
#[derive(Default, Clone, Copy)]
struct FailState {
    /// 연속 실패(성공하면 0).
    fails: u32,
    /// 마지막 성공 이후 재시작 횟수.
    restarts: u32,
}

/// 실패 하나를 처리한다. 루프를 끝내야 하면 `Some`.
fn on_error(io: &mut impl LoopIo, idx: usize, e: windows::core::Error, st: &mut FailState, counters: &AudioCounters) -> Option<LoopEnd> {
    match classify(e.code()) {
        ErrClass::DeviceLost => return Some(LoopEnd::DeviceLost(e)),
        ErrClass::Fatal => return Some(LoopEnd::Failed(e)),
        ErrClass::Transient => {}
    }
    counters.stream_errors.fetch_add(1, Ordering::Relaxed);
    st.fails += 1;
    if st.fails >= FAIL_AFTER {
        return Some(LoopEnd::Failed(e));
    }
    if !st.fails.is_multiple_of(RESTART_AFTER) {
        // 다음 이벤트(다음 주기)에 다시 시도
        return None;
    }
    if st.restarts >= MAX_RESTARTS {
        return Some(LoopEnd::Failed(e));
    }
    st.restarts += 1;
    counters.stream_restarts.fetch_add(1, Ordering::Relaxed);
    if let Err(e2) = io.restart(idx) {
        match classify(e2.code()) {
            ErrClass::DeviceLost => return Some(LoopEnd::DeviceLost(e2)),
            ErrClass::Fatal => return Some(LoopEnd::Failed(e2)),
            ErrClass::Transient => {
                counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                st.fails += 1;
                if st.fails >= FAIL_AFTER {
                    return Some(LoopEnd::Failed(e2));
                }
            }
        }
    }
    None
}

/// 오류 정책 루프(H1). RT 경로: 할당·락·로그 없음(오류 생성 시 `windows::core::Error`만 예외).
pub(crate) fn drive_loop(io: &mut impl LoopIo, stop: &AtomicBool, counters: &AudioCounters) -> LoopEnd {
    let n = io.streams().min(2);
    if n == 0 {
        return LoopEnd::Stopped;
    }
    let mut st = [FailState::default(); 2];
    while !stop.load(Ordering::Relaxed) {
        match io.wait(WAIT_TIMEOUT_MS) {
            WaitResult::Timeout => {
                // 이벤트가 멈췄다: 장치 무효화를 드러내려고 상태를 확인한다(L1).
                for (i, s) in st.iter_mut().enumerate().take(n) {
                    if let Err(e) = io.check(i) {
                        if let Some(end) = on_error(io, i, e, s, counters) {
                            return end;
                        }
                    }
                }
            }
            WaitResult::Failed => return LoopEnd::Failed(windows::core::Error::from_thread()),
            WaitResult::Ready(i) => {
                let Some(s) = st.get_mut(i).filter(|_| i < n) else {
                    return LoopEnd::Failed(windows::core::Error::from_thread());
                };
                match io.service(i) {
                    Ok(()) => *s = FailState::default(),
                    Err(e) => {
                        if let Some(end) = on_error(io, i, e, s, counters) {
                            return end;
                        }
                    }
                }
            }
        }
    }
    LoopEnd::Stopped
}

/// 실제 WASAPI 이벤트 I/O. 인덱스 0이 캡처(있으면)라 둘 다 신호 상태면 캡처가 먼저 처리된다.
struct WasapiIo<'a, C, R> {
    streams: [Option<&'a DirStream>; 2],
    handles: [HANDLE; 2],
    n: usize,
    /// f32 변환 버퍼(무음 패킷의 0, 24비트 패킹 변환).
    scratch: &'a mut [f32],
    counters: &'a AudioCounters,
    /// 시작/재시작 후 첫 캡처 패킷 전(그 패킷의 DISCONTINUITY는 세지 않는다).
    fresh: [bool; 2],
    on_capture: C,
    on_render: R,
}

impl<C: FnMut(DevBuf<'_>), R: FnMut(DevBuf<'_>) -> bool> LoopIo for WasapiIo<'_, C, R> {
    fn streams(&self) -> usize {
        self.n
    }

    fn wait(&mut self, timeout_ms: u32) -> WaitResult {
        // SAFETY: 유효한 이벤트 핸들.
        let w = unsafe { WaitForMultipleObjects(&self.handles[..self.n], false, timeout_ms) };
        if w == WAIT_TIMEOUT {
            return WaitResult::Timeout;
        }
        let idx = w.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if idx < self.n {
            WaitResult::Ready(idx)
        } else {
            WaitResult::Failed
        }
    }

    fn service(&mut self, idx: usize) -> windows::core::Result<()> {
        let Some(Some(s)) = self.streams.get(idx).copied() else {
            return Ok(());
        };
        if s.capture.is_some() {
            let fresh = &mut self.fresh[idx];
            service_capture(s, self.scratch, self.counters, fresh, &mut self.on_capture)
        } else {
            service_render(s, self.scratch, &mut self.on_render)
        }
    }

    fn check(&mut self, idx: usize) -> windows::core::Result<()> {
        match self.streams.get(idx).copied().flatten() {
            Some(s) => s.check(),
            None => Ok(()),
        }
    }

    fn restart(&mut self, idx: usize) -> windows::core::Result<()> {
        let Some(Some(s)) = self.streams.get(idx).copied() else {
            return Ok(());
        };
        if let Some(f) = self.fresh.get_mut(idx) {
            *f = true;
        }
        s.restart()
    }
}

/// 입·출력 이벤트 루프. `on_capture(buf)`, `on_render(buf) -> 무음 플래그`.
/// RT 경로: 할당·락·로그 없음(오류는 반환값으로). `scratch`는 미리 할당한 f32 버퍼.
pub(crate) fn run_loop(
    input: Option<&DirStream>,
    output: Option<&DirStream>,
    stop: &AtomicBool,
    counters: &AudioCounters,
    scratch: &mut [f32],
    on_capture: impl FnMut(DevBuf<'_>),
    on_render: impl FnMut(DevBuf<'_>) -> bool,
) -> LoopEnd {
    let mut io = WasapiIo {
        streams: [None, None],
        handles: [HANDLE::default(); 2],
        n: 0,
        scratch,
        counters,
        fresh: [true; 2],
        on_capture,
        on_render,
    };
    for s in [input, output].into_iter().flatten() {
        io.streams[io.n] = Some(s);
        io.handles[io.n] = s.event;
        io.n += 1;
    }
    drive_loop(&mut io, stop, counters)
}

/// `scratch`를 채널 배수 크기로 자른 길이(한 번에 넘길 수 있는 샘플 수).
fn chunk_samples(scratch_len: usize, ch: usize) -> usize {
    (scratch_len / ch.max(1) * ch.max(1)).max(ch.max(1))
}

fn service_capture(
    s: &DirStream,
    scratch: &mut [f32],
    counters: &AudioCounters,
    fresh: &mut bool,
    on_capture: &mut impl FnMut(DevBuf<'_>),
) -> windows::core::Result<()> {
    let Some(cap) = &s.capture else {
        return Ok(());
    };
    let ch = s.format.channels as usize;
    let per = chunk_samples(scratch.len(), ch);
    // SAFETY: 이 스레드에서 연 캡처 클라이언트. GetBuffer로 받은 포인터는 ReleaseBuffer 전까지만 쓴다.
    unsafe {
        loop {
            if cap.GetNextPacketSize()? == 0 {
                return Ok(());
            }
            let mut p: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            cap.GetBuffer(&mut p, &mut frames, &mut flags, None, None)?;
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && !*fresh {
                // 장치가 데이터를 버렸다(이 스레드가 한 주기 넘게 늦음) = 입력 오버런(M2)
                counters.input_discontinuities.fetch_add(1, Ordering::Relaxed);
                counters.xruns.fetch_add(1, Ordering::Relaxed);
            }
            *fresh = false;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent {
                counters.input_silent_packets.fetch_add(1, Ordering::Relaxed);
            }
            let samples = frames as usize * ch;
            if silent || p.is_null() {
                // 장치가 무음이라고 표시한 패킷: 버퍼 내용은 의미 없으므로 0을 넘긴다. 변환 버퍼보다 크면
                // 나눠 넘겨 입력 프레임 수를 보존한다(L6).
                let mut left = samples;
                while left > 0 {
                    let k = left.min(per);
                    let Some(z) = scratch.get_mut(..k) else {
                        break;
                    };
                    z.fill(0.0);
                    on_capture(DevBuf::F32(z));
                    left -= k;
                }
            } else if s.format.kind == SampleKind::I24 {
                let bytes = std::slice::from_raw_parts(p as *const u8, samples * 3);
                for part in bytes.chunks(per * 3) {
                    let m = i24_to_f32(part, scratch);
                    if let Some(z) = scratch.get_mut(..m) {
                        on_capture(DevBuf::F32(z));
                    }
                }
            } else if samples > 0 {
                on_capture(DevBuf::from_raw(p, frames, &s.format));
            }
            cap.ReleaseBuffer(frames)?;
        }
    }
}

fn service_render(s: &DirStream, scratch: &mut [f32], on_render: &mut impl FnMut(DevBuf<'_>) -> bool) -> windows::core::Result<()> {
    let Some(r) = &s.render else {
        return Ok(());
    };
    // SAFETY: 이 스레드에서 연 렌더 클라이언트. GetBuffer로 받은 포인터는 ReleaseBuffer 전까지만 쓴다.
    unsafe {
        let frames = match s.mode {
            // 독점 이벤트 모드: 이벤트마다 버퍼 전체(= 한 주기)를 채운다.
            ShareMode::Exclusive => s.buffer_frames,
            // 공유: 빈 만큼, 단 한 주기까지만(장치 큐를 한 주기 근처로 유지해 지연을 늘리지 않음).
            ShareMode::Shared3 => {
                let pad = s.client.GetCurrentPadding()?;
                s.buffer_frames.saturating_sub(pad).min(s.period_frames)
            }
        };
        if frames == 0 {
            return Ok(());
        }
        let p = r.GetBuffer(frames)?;
        let silent = if s.format.kind == SampleKind::I24 {
            // f32로 섞은 뒤 3바이트로 패킹. 보통 한 번에 들어간다(변환 버퍼 ≥ 50ms × 8ch).
            let ch = s.format.channels as usize;
            let samples = frames as usize * ch;
            let bytes = std::slice::from_raw_parts_mut(p, samples * 3);
            let per = chunk_samples(scratch.len(), ch);
            let mut all_silent = true;
            for part in bytes.chunks_mut(per * 3) {
                let m = (part.len() / 3).min(scratch.len());
                let Some(z) = scratch.get_mut(..m) else {
                    break;
                };
                all_silent &= on_render(DevBuf::F32(z));
                f32_to_i24(z, part);
            }
            all_silent
        } else {
            on_render(DevBuf::from_raw(p, frames, &s.format))
        };
        let flags = if silent { AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 } else { 0 };
        r.ReleaseBuffer(frames, flags)?;
    }
    Ok(())
}

fn feed_input(rt: &mut InputRt, buf: DevBuf<'_>, ch: usize) {
    match buf {
        DevBuf::F32(b) => rt.process::<f32>(b, ch),
        DevBuf::I32(b) => rt.process::<i32>(b, ch),
        DevBuf::I16(b) => rt.process::<i16>(b, ch),
    }
}

fn fill_output(rt: &mut OutputRt, buf: DevBuf<'_>, ch: usize) {
    match buf {
        DevBuf::F32(b) => rt.process::<f32>(b, ch),
        DevBuf::I32(b) => rt.process::<i32>(b, ch),
        DevBuf::I16(b) => rt.process::<i16>(b, ch),
    }
}

// ============================================================================
// 엔진용 열기
// ============================================================================

/// 열기 실패. RT 상태를 돌려받으면 다른 백엔드로 다시 시도할 수 있다.
pub(crate) struct OpenFailure {
    pub(crate) message: String,
    pub(crate) rts: Option<Box<(InputRt, OutputRt)>>,
}

struct WasapiGuard {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for WasapiGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// 입·출력을 각 모드로 연다. 성공하면 재생이 시작된 상태로 돌아온다.
pub(crate) fn open_engine_streams(
    cfg: &EngineConfig,
    modes: (ShareMode, ShareMode),
    input: InputRt,
    output: OutputRt,
    counters: Arc<AudioCounters>,
) -> Result<(StreamInfo, Box<dyn Any>), OpenFailure> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<(StreamInfo, [Option<String>; 2], String), OpenFailure>>();
    let in_name = cfg.input_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let out_name = cfg.output_device.as_deref().map(|s| device_name_from_id(s).to_string());
    let buffer_frames = cfg.buffer_size;
    let st = stop.clone();
    let c = counters.clone();
    let spawned = std::thread::Builder::new().name("band-wasapi".into()).spawn(move || {
        let _com = ComGuard::init();
        // MMCSS 등록과 변환 버퍼 할당은 스트림 시작 전에 끝낸다. 독점 2~3ms 주기에서는 시작 뒤에 하면
        // 첫 이벤트를 놓쳐 시작 글리치가 난다(L5).
        let prio = crate::sys::prio::promote_current_thread();
        c.audio_thread.store(prio.status());
        let mut scratch = vec![0f32; SCRATCH_SAMPLES];
        let (mut input, mut output) = (input, output);
        let opened = (|| {
            let en = enumerator()?;
            let min_in = input.required_device_channels() as u16;
            let mut i = DirStream::open(&en, true, in_name.as_deref(), modes.0, min_in)?;
            let mut o = DirStream::open(&en, false, out_name.as_deref(), modes.1, 1)?;
            // 열기가 시간 초과로 포기됐으면 시작하지 않는다(L4).
            if st.load(Ordering::SeqCst) {
                return Err("열기 시간 초과 후 취소".to_string());
            }
            o.start()?;
            i.start()?;
            Ok::<_, String>((i, o))
        })();
        let (i, o) = match opened {
            Ok(x) => x,
            Err(message) => {
                // 이 스레드는 곧 끝나고 MMCSS 등록도 풀린다. 다음 시도(다른 모드 또는 cpal 폴백)가 다시 적기 전까지
                // 오디오 스레드는 우리가 관리하지 않는 상태다(리뷰 M1). send보다 먼저 적어 받는 쪽이 이 값을 본다.
                c.audio_thread.store(band_dsp::prio_slot::ThreadPriority::Unmanaged);
                let _ = tx.send(Err(OpenFailure { message, rts: Some(Box::new((input, output))) }));
                return;
            }
        };
        let info = StreamInfo {
            in_rate: i.format.rate,
            out_rate: o.format.rate,
            in_channels: i.format.channels as usize,
            out_channels: o.format.channels as usize,
            buffer_frames,
            in_backend: modes.0.backend(),
            out_backend: modes.1.backend(),
            in_period_frames: i.period_frames,
            out_period_frames: o.period_frames,
            in_reported_latency_frames: (i.stream_latency_ms() * i.format.rate as f64 / 1000.0).round() as u32,
            out_reported_latency_frames: (o.stream_latency_ms() * o.format.rate as f64 / 1000.0).round() as u32,
            in_device_tag: device_tag(&i.endpoint_id),
            out_device_tag: device_tag(&o.endpoint_id),
        };
        // 기본 장치로 연 방향은 감시한다(기본 장치가 바뀌면 deviceLost).
        let watch = [
            in_name.is_none().then(|| i.endpoint_id.clone()),
            out_name.is_none().then(|| o.endpoint_id.clone()),
        ];
        // 로그는 받는 쪽(비RT)이 남긴다(L5).
        let desc = format!(
            "WASAPI 열림: in {} {:?} {:?} 주기 {}프레임({:.2}ms), out {} {:?} {:?} 주기 {}프레임({:.2}ms) 버퍼 {}",
            i.name,
            modes.0,
            i.format,
            i.period_frames,
            i.period_ms(),
            o.name,
            modes.1,
            o.format,
            o.period_frames,
            o.period_ms(),
            o.buffer_frames
        );
        let _ = tx.send(Ok((info, watch, desc)));
        let (ich, och) = (i.format.channels as usize, o.format.channels as usize);
        let end = run_loop(
            Some(&i),
            Some(&o),
            &st,
            &c,
            &mut scratch,
            |b| feed_input(&mut input, b, ich),
            |b| {
                fill_output(&mut output, b, och);
                false
            },
        );
        match end {
            LoopEnd::Stopped => {}
            LoopEnd::DeviceLost(e) => {
                c.stream_errors.fetch_add(1, Ordering::Relaxed);
                c.device_lost.store(true, Ordering::SeqCst);
                log::error!("WASAPI 장치 분실(분리/무효화): {e}");
            }
            LoopEnd::Failed(e) => {
                c.stream_errors.fetch_add(1, Ordering::Relaxed);
                c.stream_failed.store(true, Ordering::SeqCst);
                log::error!("WASAPI 스트림 복구 실패(재시작 반복 실패 또는 치명 오류): {e}");
            }
        }
    });
    let audio = match spawned {
        Ok(h) => h,
        Err(e) => return Err(OpenFailure { message: format!("WASAPI 스레드 생성 실패: {e}"), rts: None }),
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok((info, watch, desc))) => {
            log::info!("{desc}");
            let mut threads = vec![audio];
            if watch.iter().any(|w| w.is_some()) {
                let st = stop.clone();
                let c = counters;
                if let Ok(h) = std::thread::Builder::new().name("band-wasapi-watch".into()).spawn(move || watch_default(watch, &st, &c)) {
                    threads.push(h);
                }
            }
            Ok((info, Box::new(WasapiGuard { stop, threads })))
        }
        Ok(Err(f)) => {
            let _ = audio.join();
            Err(f)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err(OpenFailure { message: "WASAPI 장치 열기 시간 초과".into(), rts: None })
        }
    }
}

/// 기본 장치 감시(비RT 스레드). 기본 장치로 연 방향의 기본 장치가 바뀌면 `device_lost`.
fn watch_default(watch: [Option<String>; 2], stop: &AtomicBool, counters: &AudioCounters) {
    let _com = ComGuard::init();
    let Ok(en) = enumerator() else {
        return;
    };
    let mut last_check = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        if last_check.elapsed() < Duration::from_millis(500) {
            continue;
        }
        last_check = Instant::now();
        for (is_input, opened) in [(true, &watch[0]), (false, &watch[1])] {
            let Some(opened) = opened else {
                continue;
            };
            let now = default_endpoint_id(&en, is_input);
            if now.as_deref() != Some(opened.as_str()) && !counters.device_lost.swap(true, Ordering::SeqCst) {
                log::warn!("기본 {} 장치가 바뀌었습니다: 엔진 재시작 필요", if is_input { "입력" } else { "출력" });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use windows::core::HRESULT;
    use windows::Win32::Foundation::E_OUTOFMEMORY;
    use windows::Win32::Media::Audio::AUDCLNT_E_DEVICE_INVALIDATED;

    #[test]
    fn chunk_samples_is_channel_multiple() {
        assert_eq!(chunk_samples(SCRATCH_SAMPLES, 2), SCRATCH_SAMPLES);
        assert_eq!(chunk_samples(10, 3), 9);
        assert_eq!(chunk_samples(2, 4), 4); // 최소 한 프레임(호출부가 get_mut으로 거른다)
    }

    // ------------------------------------------------------------------------
    // 오류 정책(H1): 가짜 HRESULT 시퀀스 주입
    // ------------------------------------------------------------------------

    use std::collections::VecDeque;
    use windows::Win32::Media::Audio::{AUDCLNT_E_BUFFER_ERROR, AUDCLNT_E_BUFFER_TOO_LARGE};

    const OK: HRESULT = HRESULT(0);

    #[derive(Clone, Copy)]
    enum Step {
        /// 스트림 i 이벤트, 서비스 결과
        Ev(usize, HRESULT),
        /// 대기 타임아웃, 상태 확인 결과
        Timeout(HRESULT),
    }

    struct FakeIo<'a> {
        n: usize,
        script: VecDeque<Step>,
        restart_results: VecDeque<HRESULT>,
        restarts: Vec<usize>,
        serviced: usize,
        stop: &'a AtomicBool,
        pending: HRESULT,
    }

    impl<'a> FakeIo<'a> {
        fn new(n: usize, script: impl IntoIterator<Item = Step>, stop: &'a AtomicBool) -> Self {
            Self { n, script: script.into_iter().collect(), restart_results: VecDeque::new(), restarts: Vec::new(), serviced: 0, stop, pending: OK }
        }
    }

    impl LoopIo for FakeIo<'_> {
        fn streams(&self) -> usize {
            self.n
        }
        fn wait(&mut self, _ms: u32) -> WaitResult {
            match self.script.pop_front() {
                Some(Step::Ev(i, hr)) => {
                    self.pending = hr;
                    WaitResult::Ready(i)
                }
                Some(Step::Timeout(hr)) => {
                    self.pending = hr;
                    WaitResult::Timeout
                }
                None => {
                    self.stop.store(true, Ordering::SeqCst);
                    self.pending = OK;
                    WaitResult::Timeout
                }
            }
        }
        fn service(&mut self, _idx: usize) -> windows::core::Result<()> {
            self.serviced += 1;
            self.pending.ok()
        }
        fn check(&mut self, _idx: usize) -> windows::core::Result<()> {
            self.pending.ok()
        }
        fn restart(&mut self, idx: usize) -> windows::core::Result<()> {
            self.restarts.push(idx);
            self.restart_results.pop_front().unwrap_or(OK).ok()
        }
    }

    fn run(io: &mut FakeIo<'_>) -> (LoopEnd, u32, u32) {
        let c = AudioCounters::default();
        let stop = io.stop;
        let end = drive_loop(io, stop, &c);
        (end, c.stream_errors.load(Ordering::Relaxed), c.stream_restarts.load(Ordering::Relaxed))
    }

    fn ev(i: usize, hr: HRESULT, n: usize) -> impl Iterator<Item = Step> {
        std::iter::repeat_n(Step::Ev(i, hr), n)
    }

    #[test]
    fn single_transient_error_keeps_running() {
        // 독점 캡처의 AUDCLNT_E_BUFFER_ERROR 한 번(DPC 스파이크): 이전에는 여기서 루프가 끝나 deviceLost였다.
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 100)
            .chain([Step::Ev(0, AUDCLNT_E_BUFFER_ERROR)])
            .chain(ev(0, OK, 100))
            .chain([Step::Ev(1, AUDCLNT_E_BUFFER_TOO_LARGE)])
            .chain(ev(1, OK, 10));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!((errors, restarts), (2, 0));
        assert_eq!(io.serviced, 212, "오류 뒤에도 계속 서비스");
    }

    #[test]
    fn sporadic_errors_never_escalate() {
        // 연속 (RESTART_AFTER-1)회 실패 후 성공을 200번 반복: 재시작도 치명도 없다(성공하면 연속 횟수 초기화).
        let stop = AtomicBool::new(false);
        let mut script = Vec::new();
        for _ in 0..200 {
            script.extend(ev(0, AUDCLNT_E_BUFFER_ERROR, (RESTART_AFTER - 1) as usize));
            script.push(Step::Ev(0, OK));
        }
        let mut io = FakeIo::new(1, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!(errors, 200 * (RESTART_AFTER - 1));
        assert_eq!(restarts, 0);
    }

    #[test]
    fn repeated_errors_restart_stream_then_recover() {
        let stop = AtomicBool::new(false);
        let script = ev(1, OK, 5).chain(ev(1, AUDCLNT_E_BUFFER_ERROR, RESTART_AFTER as usize)).chain(ev(1, OK, 50));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!((errors, restarts), (RESTART_AFTER, 1));
        assert_eq!(io.restarts, vec![1], "실패한 방향만 재시작");
    }

    #[test]
    fn other_direction_success_does_not_reset_failures() {
        // 캡처(0)가 계속 실패하는 동안 렌더(1)는 성공: 캡처 쪽 연속 실패는 그대로 쌓여 캡처만 재시작된다.
        let stop = AtomicBool::new(false);
        let mut script = Vec::new();
        for _ in 0..RESTART_AFTER {
            script.push(Step::Ev(0, AUDCLNT_E_BUFFER_ERROR));
            script.push(Step::Ev(1, OK));
        }
        let mut io = FakeIo::new(2, script, &stop);
        let (end, _, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::Stopped), "{end:?}");
        assert_eq!(restarts, 1);
        assert_eq!(io.restarts, vec![0]);
    }

    #[test]
    fn persistent_errors_become_fatal_after_restarts() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 1000), &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(&end, LoopEnd::Failed(e) if e.code() == AUDCLNT_E_BUFFER_ERROR), "{end:?}");
        assert_eq!(restarts, MAX_RESTARTS);
        assert_eq!(errors, RESTART_AFTER * (MAX_RESTARTS + 1));
        assert!(io.serviced < 100, "반복 실패는 오래 끌지 않는다: {}", io.serviced);
    }

    #[test]
    fn failing_restarts_count_toward_limit() {
        // 재시작 자체가 일시 오류로 실패해도 연속 실패에 더해져 결국 치명으로 끝난다(무한 루프 없음).
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 1000), &stop);
        io.restart_results = std::iter::repeat_n(AUDCLNT_E_BUFFER_ERROR, 100).collect();
        let (end, _, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::Failed(_)), "{end:?}");
        assert!(io.serviced <= FAIL_AFTER as usize);
    }

    #[test]
    fn device_invalidated_is_device_lost_immediately() {
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 10).chain([Step::Ev(1, AUDCLNT_E_DEVICE_INVALIDATED)]).chain(ev(0, OK, 10));
        let mut io = FakeIo::new(2, script, &stop);
        let (end, errors, restarts) = run(&mut io);
        assert!(matches!(&end, LoopEnd::DeviceLost(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED), "{end:?}");
        assert_eq!((errors, restarts), (0, 0), "장치 분실은 재시도하지 않는다");
        assert_eq!(io.serviced, 11);
    }

    #[test]
    fn device_lost_during_restart() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, ev(0, AUDCLNT_E_BUFFER_ERROR, 100), &stop);
        io.restart_results = [AUDCLNT_E_DEVICE_INVALIDATED].into();
        let (end, _, restarts) = run(&mut io);
        assert!(matches!(end, LoopEnd::DeviceLost(_)), "{end:?}");
        assert_eq!(restarts, 1);
    }

    #[test]
    fn stalled_events_surface_invalidation() {
        // 이벤트가 멈춘 비기본 장치 분리(L1): 타임아웃 때 상태 확인이 무효화를 드러낸다.
        let stop = AtomicBool::new(false);
        let script = ev(0, OK, 5).chain([Step::Timeout(OK), Step::Timeout(AUDCLNT_E_DEVICE_INVALIDATED)]);
        let mut io = FakeIo::new(2, script, &stop);
        let (end, _, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::DeviceLost(_)), "{end:?}");
    }

    #[test]
    fn out_of_memory_is_fatal() {
        let stop = AtomicBool::new(false);
        let mut io = FakeIo::new(1, [Step::Ev(0, E_OUTOFMEMORY)], &stop);
        let (end, errors, _) = run(&mut io);
        assert!(matches!(end, LoopEnd::Failed(_)), "{end:?}");
        assert_eq!(errors, 0);
    }

    #[test]
    fn failed_open_resets_audio_priority_to_unmanaged() {
        // 리뷰 M1: 열기 실패 후 cpal로 폴백하면 오디오 스레드 상태가 mmcss로 남으면 안 된다.
        use band_dsp::mixer::MixControls;
        use band_dsp::prio_slot::ThreadPriority;
        use crate::engine::rt::{input_ring, output_path};
        let counters = Arc::new(AudioCounters::default());
        let (p, _c) = input_ring();
        let (out_rt, _feed) = output_path(counters.clone(), Arc::new(MixControls::default()));
        let cfg = EngineConfig {
            input_device: Some("없는-장치-band-test".into()),
            ..Default::default()
        };
        let r = open_engine_streams(
            &cfg,
            (ShareMode::Shared3, ShareMode::Shared3),
            InputRt::new(p, counters.clone(), &[0], 1),
            out_rt,
            counters.clone(),
        );
        let Err(f) = r else { panic!("없는 장치는 열기에 실패해야 함") };
        assert!(f.rts.is_some(), "폴백할 수 있게 RT를 돌려줘야 함: {}", f.message);
        assert_eq!(counters.audio_thread.load(), ThreadPriority::Unmanaged);
    }
}
