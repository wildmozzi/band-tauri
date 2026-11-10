//! 실시간 오디오 콜백 (설계 문서 1.1절).
//!
//! 이 모듈의 `process` 함수들은 오디오 장치 스레드에서 호출된다. 금지 사항:
//! 힙 할당, Mutex, 시스템콜, 로그, panic 가능 코드(인덱스 범위는 모두 사전 검증/`get`).
//! 사용하는 것은 사전 할당된 SPSC 링(ringbuf, wait-free)과 원자값뿐이다.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::stats::AudioCounters;

/// 콜백 내부 처리 단위(프레임). 스크래치 버퍼 크기를 결정한다.
const RT_CHUNK_FRAMES: usize = 256;

/// 링 용량: 192kHz × 2ch × 0.25초. 장치 레이트와 무관하게 충분하다.
pub const RING_CAPACITY: usize = 192_000 / 4 * 2;

/// 제곱합 → 0..1 선형 RMS. 샘플이 없으면 0.
#[inline]
pub fn rms_of(sum_sq: f32, count: usize) -> f32 {
    if count == 0 {
        0.0
    } else {
        (sum_sq / count as f32).sqrt().min(1.0)
    }
}

/// 장치 샘플 포맷 ↔ f32 변환.
pub trait RtSample: Copy + Send + 'static {
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
    const SILENCE: Self;
}

impl RtSample for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        v
    }
    const SILENCE: Self = 0.0;
}

impl RtSample for i16 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32 / 32768.0
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        (v.clamp(-1.0, 1.0) * 32767.0) as i16
    }
    const SILENCE: Self = 0;
}

impl RtSample for i32 {
    #[inline]
    fn to_f32(self) -> f32 {
        (self as f64 / 2_147_483_648.0) as f32
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        (v.clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32
    }
    const SILENCE: Self = 0;
}

impl RtSample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        (self as f32 - 32768.0) / 32768.0
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        ((v.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16
    }
    const SILENCE: Self = 32768;
}

/// 입력 콜백 상태. 장치 샘플을 내부 채널(1/2)로 라우팅해 입력 링에 넣는다.
pub struct InputRt {
    prod: HeapProd<f32>,
    counters: Arc<AudioCounters>,
    /// 장치 채널 인덱스(0-based). 길이는 `route_len`.
    route: [usize; 2],
    route_len: usize,
    internal_channels: usize,
    scratch: Box<[f32]>,
}

/// 출력 콜백 상태. 출력 링(스테레오 인터리브)에서 꺼내 장치 채널로 기록한다.
pub struct OutputRt {
    cons: HeapCons<f32>,
    counters: Arc<AudioCounters>,
    scratch: Box<[f32]>,
    primed: bool,
}

/// 입력 링(입력 콜백 → 워커).
pub fn input_ring() -> (HeapProd<f32>, HeapCons<f32>) {
    HeapRb::<f32>::new(RING_CAPACITY).split()
}

/// 출력 링(워커 → 출력 콜백).
pub fn output_ring() -> (HeapProd<f32>, HeapCons<f32>) {
    HeapRb::<f32>::new(RING_CAPACITY).split()
}

impl InputRt {
    pub fn new(
        prod: HeapProd<f32>,
        counters: Arc<AudioCounters>,
        input_channels: &[u16],
        internal_channels: usize,
    ) -> Self {
        let mut route = [0usize; 2];
        let route_len = input_channels.len().clamp(1, 2);
        for (r, c) in route.iter_mut().zip(input_channels) {
            *r = *c as usize;
        }
        Self {
            prod,
            counters,
            route,
            route_len,
            internal_channels: internal_channels.clamp(1, 2),
            scratch: vec![0.0f32; RT_CHUNK_FRAMES * 2].into_boxed_slice(),
        }
    }

    /// 라우팅에 필요한 최소 장치 채널 수.
    pub fn required_device_channels(&self) -> usize {
        self.route[..self.route_len].iter().copied().max().unwrap_or(0) + 1
    }

    #[inline]
    fn sample<T: RtSample>(frame: &[T], idx: usize) -> f32 {
        match frame.get(idx) {
            Some(s) => s.to_f32(),
            None => 0.0,
        }
    }

    /// RT: 장치 입력 버퍼 처리.
    pub fn process<T: RtSample>(&mut self, data: &[T], device_channels: usize) {
        if device_channels == 0 {
            return;
        }
        let frames = data.len() / device_channels;
        self.counters.in_cb_frames.store(frames as u32, Ordering::Relaxed);
        let ich = self.internal_channels;
        let mut sum_sq = 0.0f32;
        let mut count = 0usize;
        let mut overrun = false;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(RT_CHUNK_FRAMES);
            for i in 0..n {
                let base = (done + i) * device_channels;
                let frame = match data.get(base..base + device_channels) {
                    Some(f) => f,
                    None => break,
                };
                let a = Self::sample(frame, self.route[0]);
                let b = if self.route_len == 2 { Self::sample(frame, self.route[1]) } else { a };
                if ich == 1 {
                    let m = if self.route_len == 2 { (a + b) * 0.5 } else { a };
                    if let Some(s) = self.scratch.get_mut(i) {
                        *s = m;
                    }
                    sum_sq += m * m;
                    count += 1;
                } else {
                    if let Some(s) = self.scratch.get_mut(i * 2..i * 2 + 2) {
                        s[0] = a;
                        s[1] = b;
                    }
                    sum_sq += a * a + b * b;
                    count += 2;
                }
            }
            let len = (n * ich).min(self.scratch.len());
            // 오버런 시 프레임 단위로만 기록한다. 부분 기록하면 스테레오 L/R이 영구히 뒤바뀐다(리뷰 L3).
            let room = self.prod.vacant_len() / ich * ich;
            let len_ok = len.min(room);
            self.prod.push_slice(&self.scratch[..len_ok]);
            if len_ok < len {
                overrun = true;
            }
            done += n;
        }
        if overrun {
            self.counters.xruns.fetch_add(1, Ordering::Relaxed);
        }
        // 레벨 = 0..1 선형 RMS (입력 뮤트와 무관하게 계속 갱신)
        self.counters.input_level.store(rms_of(sum_sq, count));
    }
}

impl OutputRt {
    pub fn new(cons: HeapCons<f32>, counters: Arc<AudioCounters>) -> Self {
        Self {
            cons,
            counters,
            scratch: vec![0.0f32; RT_CHUNK_FRAMES * 2].into_boxed_slice(),
            primed: false,
        }
    }

    /// RT: 장치 출력 버퍼 채우기. 링이 부족하면 0으로 채우고 xrun을 센다.
    pub fn process<T: RtSample>(&mut self, data: &mut [T], device_channels: usize) {
        if device_channels == 0 {
            return;
        }
        let frames = data.len() / device_channels;
        self.counters.out_cb_frames.store(frames as u32, Ordering::Relaxed);
        let mut sum_sq = 0.0f32;
        let mut underrun = false;
        let mut consumed = 0u64;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(RT_CHUNK_FRAMES);
            let want = n * 2;
            let got = self.cons.pop_slice(&mut self.scratch[..want]);
            consumed += got as u64;
            if got < want {
                // 받은 꼬리를 짧게 페이드아웃해 클릭을 줄인다(리뷰 L9). got은 스테레오 짝수.
                let got = got & !1;
                let fade = (got / 2).min(32);
                for k in 0..fade {
                    let g = (fade - k) as f32 / (fade + 1) as f32;
                    let idx = got - (fade - k) * 2;
                    if let Some(s) = self.scratch.get_mut(idx..idx + 2) {
                        s[0] *= g;
                        s[1] *= g;
                    }
                }
                if let Some(rest) = self.scratch.get_mut(got..want) {
                    rest.fill(0.0);
                }
                underrun = true;
            }
            for i in 0..n {
                let l = self.scratch[i * 2];
                let r = self.scratch[i * 2 + 1];
                sum_sq += l * l + r * r;
                let base = (done + i) * device_channels;
                let Some(frame) = data.get_mut(base..base + device_channels) else {
                    break;
                };
                if device_channels == 1 {
                    frame[0] = T::from_f32((l + r) * 0.5);
                } else {
                    frame[0] = T::from_f32(l);
                    frame[1] = T::from_f32(r);
                    for s in frame.iter_mut().skip(2) {
                        *s = T::SILENCE;
                    }
                }
            }
            done += n;
        }
        // 남는 꼬리(채널 수로 나눠떨어지지 않는 경우)
        for s in data.iter_mut().skip(frames * device_channels) {
            *s = T::SILENCE;
        }
        if consumed > 0 && !underrun {
            self.primed = true;
        }
        if underrun && self.primed {
            self.counters.xruns.fetch_add(1, Ordering::Relaxed);
        }
        self.counters.out_consumed.fetch_add(consumed, Ordering::Relaxed);
        self.counters.output_level.store(rms_of(sum_sq, frames * 2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_routing_mono_from_ch2() {
        let (prod, mut cons) = input_ring();
        let counters = Arc::new(AudioCounters::default());
        let mut rt = InputRt::new(prod, counters.clone(), &[1], 1);
        assert_eq!(rt.required_device_channels(), 2);
        // 4채널 장치, 3프레임
        let data: Vec<i32> = vec![0, 1 << 30, 0, 0, 0, -(1 << 30), 0, 0, 0, 0, 0, 0];
        rt.process(&data, 4);
        let mut out = [0f32; 8];
        let n = cons.pop_slice(&mut out);
        assert_eq!(n, 3, "모노는 1채널만 링에 들어감");
        assert!((out[0] - 0.5).abs() < 1e-6 && (out[1] + 0.5).abs() < 1e-6 && out[2] == 0.0);
        // RMS([0.5, -0.5, 0]) = sqrt(0.5 / 3)
        assert!((counters.input_level.load() - (0.5f32 / 3.0).sqrt()).abs() < 1e-6);
        assert_eq!(counters.in_cb_frames.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn input_routing_stereo_swap_and_downmix() {
        let (prod, mut cons) = input_ring();
        let counters = Arc::new(AudioCounters::default());
        let mut rt = InputRt::new(prod, counters.clone(), &[1, 0], 2);
        rt.process(&[0.1f32, 0.2, 0.3, 0.4], 2);
        let mut out = [0f32; 4];
        assert_eq!(cons.pop_slice(&mut out), 4);
        assert_eq!(out, [0.2, 0.1, 0.4, 0.3]);

        let (prod, mut cons) = input_ring();
        let mut rt = InputRt::new(prod, counters, &[0, 1], 1);
        rt.process(&[0.2f32, 0.4], 2);
        let mut out = [0f32; 1];
        assert_eq!(cons.pop_slice(&mut out), 1);
        assert!((out[0] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn input_overrun_keeps_stereo_alignment() {
        let (prod, mut cons) = input_ring();
        let counters = Arc::new(AudioCounters::default());
        let mut rt = InputRt::new(prod, counters.clone(), &[0, 1], 2);
        // 링을 L=0.1, R=0.9 프레임으로 넘치게 채운다
        let frame = [0.1f32, 0.9];
        let block: Vec<f32> = frame.iter().copied().cycle().take(2 * 257).collect();
        for _ in 0..(RING_CAPACITY / block.len() + 4) {
            rt.process(&block, 2);
        }
        assert!(counters.xruns.load(Ordering::Relaxed) > 0, "오버런 발생");
        assert_eq!(cons.occupied_len(), RING_CAPACITY);
        // 빈자리를 홀수(3)로 만든 뒤 다시 넘치게 → 한 프레임(2샘플)만 기록되어야 한다
        let mut three = [0f32; 3];
        cons.pop_slice(&mut three);
        rt.process(&block, 2);
        assert_eq!(cons.occupied_len(), RING_CAPACITY - 1, "부분 프레임을 쓰면 안 됨");
    }

    #[test]
    fn output_underrun_fills_zero_and_counts() {
        let (mut prod, cons) = output_ring();
        let counters = Arc::new(AudioCounters::default());
        let mut rt = OutputRt::new(cons, counters.clone());
        let mut dev = vec![1.0f32; 2 * 600]; // 600 프레임 > 청크
        // 처음 비어 있음 → primed 전이므로 xrun 아님
        rt.process(&mut dev, 2);
        assert!(dev.iter().all(|&x| x == 0.0));
        assert_eq!(counters.xruns.load(Ordering::Relaxed), 0);
        prod.push_slice(&vec![0.5f32; 1200]);
        rt.process(&mut dev, 2);
        assert!(dev.iter().all(|&x| x == 0.5));
        assert_eq!(counters.out_consumed.load(Ordering::Relaxed), 1200);
        // 이제 부족 → xrun
        prod.push_slice(&[0.25f32; 10]);
        rt.process(&mut dev, 2);
        assert_eq!(counters.xruns.load(Ordering::Relaxed), 1);
        // 받은 5프레임은 페이드아웃(단조 감소, 0보다 큼), 나머지는 0
        for f in 0..5 {
            assert_eq!(dev[f * 2], dev[f * 2 + 1]);
            assert!(dev[f * 2] > 0.0 && dev[f * 2] < 0.25);
            if f > 0 {
                assert!(dev[f * 2] < dev[(f - 1) * 2]);
            }
        }
        assert!(dev[10..].iter().all(|&x| x == 0.0));
        assert_eq!(prod.occupied_len(), 0);
    }

    #[test]
    fn output_channel_mapping() {
        let (mut prod, cons) = output_ring();
        let counters = Arc::new(AudioCounters::default());
        let mut rt = OutputRt::new(cons, counters);
        prod.push_slice(&[0.5f32, -0.5, 0.2, 0.4]);
        let mut dev = vec![7i16; 8]; // 4채널 2프레임
        rt.process(&mut dev, 4);
        assert_eq!(dev[0], i16::from_f32(0.5));
        assert_eq!(dev[1], i16::from_f32(-0.5));
        assert_eq!(&dev[2..4], &[0, 0]);
        prod.push_slice(&[0.2f32, 0.4]);
        let mut mono = vec![0f32; 1];
        rt.process(&mut mono, 1);
        assert!((mono[0] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn sample_conversions() {
        assert_eq!(i16::from_f32(2.0), 32767);
        assert_eq!(i16::from_f32(-2.0), -32767);
        assert!((i32::from_f32(0.5).to_f32() - 0.5).abs() < 1e-6);
        assert!((u16::from_f32(0.0).to_f32()).abs() < 1e-4);
    }
}
