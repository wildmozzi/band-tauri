//! 실시간 오디오 콜백 (설계 문서 1.1절).
//!
//! 이 모듈의 `process` 함수들은 오디오 장치 스레드에서 호출된다. 금지 사항:
//! 힙 할당, Mutex, 시스템콜, 로그, panic 가능 코드(인덱스 범위는 모두 사전 검증/`get`).
//! 사용하는 것은 사전 할당된 SPSC 링(ringbuf, wait-free)과 원자값뿐이다.
//!
//! ## 콜백 구동 믹스 (P1-0)
//! 출력 콜백은 소스(로컬 모니터 1개 + 피어별 1개)마다 있는 **디코드된 PCM 링**에서 필요한 프레임만큼
//! 직접 꺼내 게인·팬을 곱해 섞는다. 믹스된 출력 링이 따로 없으므로 "출력 링 ≈ 장치 버퍼 1개" 하한이 없다.
//! 디코드·지터 제어·은닉은 워커(비RT)가 소스 링에 미리 넣는다(`worker.rs`, engine-notes "콜백 구동 믹스").
//! 게인은 워커가 원자값으로 넣어 두고, 소스 레벨과 링 부족(언더런 프레임 수)은 콜백이 원자값으로 알린다.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::mixer::{soft_clip, MixControls};
use super::stats::{AtomicF32, AudioCounters};

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

/// 출력 소스 슬롯 수: 0 = 로컬(모니터), 1.. = 피어(원격 최대 5명 + 교체 중인 슬롯 여유).
pub const MAX_SOURCES: usize = 8;
/// 로컬 입력(모니터) 소스 슬롯.
pub const LOCAL_SOURCE: usize = 0;
/// 소스 링 용량(장치 레이트 스테레오 샘플): 192kHz에서 0.1초.
pub const SOURCE_RING_CAPACITY: usize = 192_000 / 10 * 2;
/// 믹스 탭 링 용량(장치 레이트 스테레오 샘플): 192kHz에서 0.5초.
pub const TAP_RING_CAPACITY: usize = 192_000 / 2 * 2;

/// 슬롯 상태: 비어 있음(콜백이 읽지 않음).
pub const SLOT_FREE: u8 = 0;
/// 슬롯 상태: 콜백이 읽어 섞음.
pub const SLOT_ACTIVE: u8 = 1;
/// 슬롯 상태: 워커가 해제를 요청함. 콜백이 남은 샘플을 버리고 `SLOT_FREE`로 바꾼다.
pub const SLOT_DRAIN: u8 = 2;

/// 소스 하나의 공유 상태(워커 ↔ 출력 콜백). 모두 원자값이다.
#[derive(Debug, Default)]
pub struct SourceShared {
    pub state: AtomicU8,
    /// 출력 버스 게인(L, R). 로컬은 모니터 게인, 피어는 gain·pan·mute.
    pub bus_l: AtomicF32,
    pub bus_r: AtomicF32,
    /// 믹스 탭 게인(L, R). 로컬은 1(모니터 설정 무관), 피어는 버스와 같음.
    pub tap_l: AtomicF32,
    pub tap_r: AtomicF32,
    /// true일 때만 링 부족을 언더런(xrun)으로 센다. 워커가 재생 중인 소스에만 켠다.
    pub playing: AtomicBool,
    /// 재생 중 링이 모자라 0으로 채운 누적 장치 프레임. 워커는 같은 양을 스트림에서 건너뛰어
    /// 재생 시각을 장치 시각에 붙여 둔다(언더런 때 지연이 쌓이지 않음).
    pub underrun_frames: AtomicU64,
    /// 콜백 1회분 소스 RMS(0..1).
    pub level: AtomicF32,
}

impl SourceShared {
    pub fn set_gains(&self, bus: (f32, f32), tap: (f32, f32)) {
        self.bus_l.store(bus.0);
        self.bus_r.store(bus.1);
        self.tap_l.store(tap.0);
        self.tap_r.store(tap.1);
    }
}

/// 모든 소스 슬롯의 공유 상태.
#[derive(Debug, Default)]
pub struct SourceBank {
    pub slots: [SourceShared; MAX_SOURCES],
}

/// 워커 쪽 출력 끝점: 소스 링 생산자들과 믹스 탭 링 소비자.
pub struct OutputFeed {
    pub prods: Vec<HeapProd<f32>>,
    pub bank: Arc<SourceBank>,
    pub tap: HeapCons<f32>,
}

/// 출력 콜백 상태. 소스 링들에서 꺼내 섞고 장치 채널로 기록한다(콜백 구동 믹스).
pub struct OutputRt {
    cons: Vec<HeapCons<f32>>,
    bank: Arc<SourceBank>,
    controls: Arc<MixControls>,
    tap: HeapProd<f32>,
    counters: Arc<AudioCounters>,
    scratch: Box<[f32]>,
    bus: Box<[f32]>,
    tap_bus: Box<[f32]>,
    level_acc: [f32; MAX_SOURCES],
}

/// 입력 링(입력 콜백 → 워커).
pub fn input_ring() -> (HeapProd<f32>, HeapCons<f32>) {
    HeapRb::<f32>::new(RING_CAPACITY).split()
}

/// 출력 경로 생성: (출력 콜백 상태, 워커 쪽 끝점). 링은 모두 여기서 미리 할당한다.
pub fn output_path(counters: Arc<AudioCounters>, controls: Arc<MixControls>) -> (OutputRt, OutputFeed) {
    let mut prods = Vec::with_capacity(MAX_SOURCES);
    let mut cons = Vec::with_capacity(MAX_SOURCES);
    for _ in 0..MAX_SOURCES {
        let (p, c) = HeapRb::<f32>::new(SOURCE_RING_CAPACITY).split();
        prods.push(p);
        cons.push(c);
    }
    let (tap_p, tap_c) = HeapRb::<f32>::new(TAP_RING_CAPACITY).split();
    let bank = Arc::new(SourceBank::default());
    let rt = OutputRt {
        cons,
        bank: bank.clone(),
        controls,
        tap: tap_p,
        counters,
        scratch: vec![0.0f32; RT_CHUNK_FRAMES * 2].into_boxed_slice(),
        bus: vec![0.0f32; RT_CHUNK_FRAMES * 2].into_boxed_slice(),
        tap_bus: vec![0.0f32; RT_CHUNK_FRAMES * 2].into_boxed_slice(),
        level_acc: [0.0; MAX_SOURCES],
    };
    (rt, OutputFeed { prods, bank, tap: tap_c })
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
        self.counters.in_captured_frames.fetch_add(frames as u64, Ordering::Relaxed);
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
    /// RT: 소스 하나를 `n`프레임 꺼내 버스/탭에 더한다. 링이 모자라면 꼬리를 페이드아웃하고 0으로 채운다.
    /// 반환: 재생 중인 소스가 모자랐는지.
    #[inline]
    fn mix_source(&mut self, s: usize, n: usize) -> bool {
        let want = n * 2;
        let Some(sh) = self.bank.slots.get(s) else {
            return false;
        };
        let Some(cons) = self.cons.get_mut(s) else {
            return false;
        };
        match sh.state.load(Ordering::Acquire) {
            SLOT_ACTIVE => {}
            SLOT_DRAIN => {
                // 워커가 슬롯을 해제했다: 남은 샘플을 버리고 비운다(다음 소스가 이전 피어 소리를 이어받지 않게).
                cons.clear();
                sh.state.store(SLOT_FREE, Ordering::Release);
                return false;
            }
            _ => return false,
        }
        let Some(scratch) = self.scratch.get_mut(..want) else {
            return false;
        };
        let got = cons.pop_slice(scratch) & !1;
        let mut short = false;
        if got < want {
            // 받은 꼬리를 짧게 페이드아웃해 클릭을 줄인다(리뷰 L9).
            let fade = (got / 2).min(32);
            for k in 0..fade {
                let g = (fade - k) as f32 / (fade + 1) as f32;
                let idx = got - (fade - k) * 2;
                if let Some(x) = scratch.get_mut(idx..idx + 2) {
                    x[0] *= g;
                    x[1] *= g;
                }
            }
            if let Some(rest) = scratch.get_mut(got..) {
                rest.fill(0.0);
            }
            if sh.playing.load(Ordering::Relaxed) {
                sh.underrun_frames.fetch_add(((want - got) / 2) as u64, Ordering::Relaxed);
                short = true;
            }
        }
        let (bl, br, tl, tr) = (sh.bus_l.load(), sh.bus_r.load(), sh.tap_l.load(), sh.tap_r.load());
        let mut sq = 0.0f32;
        for ((x, b), t) in scratch
            .as_chunks::<2>()
            .0
            .iter()
            .zip(self.bus.as_chunks_mut::<2>().0.iter_mut())
            .zip(self.tap_bus.as_chunks_mut::<2>().0.iter_mut())
        {
            sq += x[0] * x[0] + x[1] * x[1];
            b[0] += x[0] * bl;
            b[1] += x[1] * br;
            t[0] += x[0] * tl;
            t[1] += x[1] * tr;
        }
        if let Some(a) = self.level_acc.get_mut(s) {
            *a += sq;
        }
        short
    }

    /// RT: 장치 출력 버퍼 채우기. 소스 링에서 필요한 만큼만 꺼내 섞는다(콜백 구동 믹스).
    /// 재생 중인 소스가 모자라면 그 부분을 0으로 채우고 xrun을 센다(콜백당 1회).
    pub fn process<T: RtSample>(&mut self, data: &mut [T], device_channels: usize) {
        if device_channels == 0 {
            return;
        }
        let frames = data.len() / device_channels;
        // 워커가 "콜백 도중/직후"에 읽은 링 점유량을 걸러내도록 시작·끝을 알린다(SeqCst: 링 인덱스 갱신과 순서 고정).
        self.counters.out_cb_begin.fetch_add(1, Ordering::SeqCst);
        self.counters.out_cb_frames.store(frames as u32, Ordering::Relaxed);
        let master = self.controls.master_gain.load();
        self.level_acc = [0.0; MAX_SOURCES];
        let mut sum_sq = 0.0f32;
        let mut underrun = false;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(RT_CHUNK_FRAMES);
            let want = n * 2;
            if let Some(b) = self.bus.get_mut(..want) {
                b.fill(0.0);
            }
            if let Some(t) = self.tap_bus.get_mut(..want) {
                t.fill(0.0);
            }
            for s in 0..MAX_SOURCES {
                underrun |= self.mix_source(s, n);
            }
            for i in 0..n {
                let (Some(&l), Some(&r)) = (self.bus.get(i * 2), self.bus.get(i * 2 + 1)) else {
                    break;
                };
                let l = soft_clip(l * master);
                let r = soft_clip(r * master);
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
            // 믹스 탭(관람자 브릿지): 구독자가 없으면 링이 가득 차 그냥 버려진다. 프레임 단위로만 기록.
            if let Some(t) = self.tap_bus.get_mut(..want) {
                for x in t.iter_mut() {
                    *x = soft_clip(*x);
                }
                let room = self.tap.vacant_len() & !1;
                let k = want.min(room);
                self.tap.push_slice(&t[..k]);
            }
            done += n;
        }
        // 남는 꼬리(채널 수로 나눠떨어지지 않는 경우)
        for s in data.iter_mut().skip(frames * device_channels) {
            *s = T::SILENCE;
        }
        if underrun {
            self.counters.xruns.fetch_add(1, Ordering::Relaxed);
        }
        for (sh, acc) in self.bank.slots.iter().zip(self.level_acc.iter()) {
            sh.level.store(rms_of(*acc, frames * 2));
        }
        self.counters.out_played_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.counters.output_level.store(rms_of(sum_sq, frames * 2));
        self.counters.out_cb_end.fetch_add(1, Ordering::SeqCst);
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

    fn out_rt() -> (OutputRt, OutputFeed, Arc<AudioCounters>, Arc<MixControls>) {
        let counters = Arc::new(AudioCounters::default());
        let controls = Arc::new(MixControls::default());
        let (rt, feed) = output_path(counters.clone(), controls.clone());
        (rt, feed, counters, controls)
    }

    fn activate(feed: &OutputFeed, s: usize, playing: bool) {
        let sh = &feed.bank.slots[s];
        sh.set_gains((1.0, 1.0), (1.0, 1.0));
        sh.playing.store(playing, Ordering::Relaxed);
        sh.state.store(SLOT_ACTIVE, Ordering::Release);
    }

    #[test]
    fn output_underrun_fills_zero_and_counts() {
        let (mut rt, mut feed, counters, _) = out_rt();
        activate(&feed, LOCAL_SOURCE, false);
        let mut dev = vec![1.0f32; 2 * 600]; // 600 프레임 > 청크
        // 처음 비어 있음 → 재생 중이 아니므로 xrun 아님
        rt.process(&mut dev, 2);
        assert!(dev.iter().all(|&x| x == 0.0));
        assert_eq!(counters.xruns.load(Ordering::Relaxed), 0);
        feed.bank.slots[LOCAL_SOURCE].playing.store(true, Ordering::Relaxed);
        feed.prods[LOCAL_SOURCE].push_slice(&vec![0.5f32; 1200]);
        rt.process(&mut dev, 2);
        assert!(dev.iter().all(|&x| x == 0.5));
        // 이제 부족 → xrun, 부족분은 소스별 언더런 프레임으로 알린다
        feed.prods[LOCAL_SOURCE].push_slice(&[0.25f32; 10]);
        rt.process(&mut dev, 2);
        assert_eq!(counters.xruns.load(Ordering::Relaxed), 1);
        assert_eq!(feed.bank.slots[LOCAL_SOURCE].underrun_frames.load(Ordering::Relaxed), 595);
        // 장치 재생 시간은 언더런과 무관하게 콜백 프레임 수만큼 흐른다(PR #5 H-1)
        assert_eq!(counters.out_played_frames.load(Ordering::Relaxed), 3 * 600);
        // 받은 5프레임은 페이드아웃(단조 감소, 0보다 큼), 나머지는 0
        for f in 0..5 {
            assert_eq!(dev[f * 2], dev[f * 2 + 1]);
            assert!(dev[f * 2] > 0.0 && dev[f * 2] < 0.25);
            if f > 0 {
                assert!(dev[f * 2] < dev[(f - 1) * 2]);
            }
        }
        assert!(dev[10..].iter().all(|&x| x == 0.0));
        assert_eq!(feed.prods[LOCAL_SOURCE].occupied_len(), 0);
    }

    #[test]
    fn output_channel_mapping() {
        let (mut rt, mut feed, _, _) = out_rt();
        activate(&feed, LOCAL_SOURCE, true);
        feed.prods[LOCAL_SOURCE].push_slice(&[0.5f32, -0.5, 0.2, 0.4]);
        let mut dev = vec![7i16; 8]; // 4채널 2프레임
        rt.process(&mut dev, 4);
        assert_eq!(dev[0], i16::from_f32(0.5));
        assert_eq!(dev[1], i16::from_f32(-0.5));
        assert_eq!(&dev[2..4], &[0, 0]);
        feed.prods[LOCAL_SOURCE].push_slice(&[0.2f32, 0.4]);
        let mut mono = vec![0f32; 1];
        rt.process(&mut mono, 1);
        assert!((mono[0] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn output_mixes_sources_with_gains_master_and_tap() {
        let (mut rt, mut feed, _, controls) = out_rt();
        activate(&feed, LOCAL_SOURCE, true);
        activate(&feed, 3, true);
        // 로컬: 모니터 꺼짐(버스 0), 탭 1. 피어: 버스/탭 (0.5, 0.25)
        feed.bank.slots[LOCAL_SOURCE].set_gains((0.0, 0.0), (1.0, 1.0));
        feed.bank.slots[3].set_gains((0.5, 0.25), (0.5, 0.25));
        controls.master_gain.store(2.0);
        feed.prods[LOCAL_SOURCE].push_slice(&[0.1f32; 8]);
        feed.prods[3].push_slice(&[0.4f32; 8]);
        let mut dev = vec![0f32; 8];
        rt.process(&mut dev, 2);
        for f in dev.as_chunks::<2>().0 {
            assert!((f[0] - 0.4).abs() < 1e-6, "L = 0.4·0.5·2 = {}", f[0]);
            assert!((f[1] - 0.2).abs() < 1e-6, "R = 0.4·0.25·2 = {}", f[1]);
        }
        let mut tap = [0f32; 8];
        assert_eq!(feed.tap.pop_slice(&mut tap), 8);
        // 탭 = 로컬 0.1 + 피어(0.2, 0.1), 마스터 무관
        assert!((tap[0] - 0.3).abs() < 1e-6 && (tap[1] - 0.2).abs() < 1e-6, "{tap:?}");
        assert!((feed.bank.slots[3].level.load() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn drained_slot_discards_leftover_samples() {
        let (mut rt, mut feed, counters, _) = out_rt();
        activate(&feed, 2, true);
        feed.prods[2].push_slice(&[0.7f32; 100]);
        feed.bank.slots[2].state.store(SLOT_DRAIN, Ordering::Release);
        let mut dev = vec![0f32; 8];
        rt.process(&mut dev, 2);
        assert_eq!(feed.bank.slots[2].state.load(Ordering::Acquire), SLOT_FREE);
        assert_eq!(feed.prods[2].occupied_len(), 0, "이전 피어 소리가 남으면 안 됨");
        assert!(dev.iter().all(|&x| x == 0.0));
        // 비활성 슬롯은 언더런으로 세지 않는다
        rt.process(&mut dev, 2);
        assert_eq!(counters.xruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn sample_conversions() {
        assert_eq!(i16::from_f32(2.0), 32767);
        assert_eq!(i16::from_f32(-2.0), -32767);
        assert!((i32::from_f32(0.5).to_f32() - 0.5).abs() < 1e-6);
        assert!((u16::from_f32(0.0).to_f32()).abs() < 1e-4);
    }
}
