//! WASAPI 스모크 프로브: 한 방향 스트림을 열어 콜백 간격을 잰다. `examples/wasapi_probe.rs`만 쓴다(이동만, 엔진 ⑥-5).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::enumerate::{enumerator, ComGuard};
use super::format::{ShareMode, StreamFormat};
use super::stream::{run_loop, DevBuf, DirStream, LoopEnd, SCRATCH_SAMPLES};
use crate::engine::stats::AudioCounters;

// ============================================================================
// 스모크 테스트(examples/wasapi_probe.rs)
// ============================================================================

/// 한 방향 스트림을 열어 `seconds` 동안 돌리며 콜백 간격을 잰다. 출력은 무음(SILENT 플래그)이다.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub name: String,
    pub format: StreamFormat,
    pub mode: ShareMode,
    pub period_frames: u32,
    pub buffer_frames: u32,
    /// 콜백 간격(µs).
    pub intervals_us: Vec<u32>,
    pub frames: u64,
    pub error: Option<String>,
    /// `IAudioClient::GetStreamLatency`(ms): 드라이버/코덱 FIFO 등 장치 주기 밖의 지연(estLatencyMs에 없음).
    pub stream_latency_ms: f64,
    /// 캡처 DATA_DISCONTINUITY(시작 직후 첫 패킷 제외), SILENT 패킷, 일시 오류, 재시작 횟수.
    pub discontinuities: u64,
    pub silent_packets: u64,
    pub stream_errors: u32,
    pub restarts: u32,
}

pub fn probe_stream(is_input: bool, device: Option<&str>, mode: ShareMode, seconds: f64) -> Result<ProbeReport, String> {
    let device = device.map(|s| s.to_string());
    std::thread::Builder::new()
        .name("band-wasapi-probe".into())
        .spawn(move || {
            let _com = ComGuard::init();
            let en = enumerator()?;
            let mut s = DirStream::open(&en, is_input, device.as_deref(), mode, 1)?;
            s.start()?;
            let _prio = crate::sys::prio::promote_current_thread();
            let stop = AtomicBool::new(false);
            let ch = s.format.channels as usize;
            // (마지막 콜백 시각, 간격 목록, 프레임 수). 프로브는 RT 규칙 대상이 아니다(측정 도구).
            let state = std::cell::RefCell::new((None::<Instant>, Vec::with_capacity((seconds * 2000.0) as usize + 16), 0u64));
            let t0 = Instant::now();
            let tick = |n: usize| {
                let mut st = state.borrow_mut();
                let now = Instant::now();
                if let Some(l) = st.0 {
                    st.1.push((now - l).as_micros() as u32);
                }
                st.0 = Some(now);
                st.2 += (n / ch.max(1)) as u64;
                if t0.elapsed().as_secs_f64() >= seconds {
                    stop.store(true, Ordering::Relaxed);
                }
            };
            let len = |b: &DevBuf<'_>| match b {
                DevBuf::F32(x) => x.len(),
                DevBuf::I32(x) => x.len(),
                DevBuf::I16(x) => x.len(),
            };
            let (input, output) = if is_input { (Some(&s), None) } else { (None, Some(&s)) };
            let mut scratch = vec![0f32; SCRATCH_SAMPLES];
            let counters = AudioCounters::default();
            let end = run_loop(input, output, &stop, &counters, &mut scratch, |b| tick(len(&b)), |mut b| {
                // 무음: 내용을 0으로 채우고 SILENT 플래그도 켠다(소리 내지 않음).
                match &mut b {
                    DevBuf::F32(x) => x.fill(0.0),
                    DevBuf::I32(x) => x.fill(0),
                    DevBuf::I16(x) => x.fill(0),
                }
                tick(len(&b));
                true
            });
            let (_, intervals, frames) = state.into_inner();
            Ok(ProbeReport {
                name: s.name.clone(),
                format: s.format,
                mode,
                period_frames: s.period_frames,
                buffer_frames: s.buffer_frames,
                intervals_us: intervals,
                frames,
                error: match end {
                    LoopEnd::Stopped => None,
                    LoopEnd::DeviceLost(e) => Some(format!("장치 분실: {e}")),
                    LoopEnd::Failed(e) => Some(format!("복구 실패: {e}")),
                },
                stream_latency_ms: s.stream_latency_ms(),
                discontinuities: counters.input_discontinuities.load(Ordering::Relaxed),
                silent_packets: counters.input_silent_packets.load(Ordering::Relaxed),
                stream_errors: counters.stream_errors.load(Ordering::Relaxed),
                restarts: counters.stream_restarts.load(Ordering::Relaxed),
            })
        })
        .map_err(|e| format!("프로브 스레드 생성 실패: {e}"))?
        .join()
        .map_err(|_| "프로브 스레드 패닉".to_string())?
}
