//! 출력 콜백(RT) 쪽 클릭: 미리 계산한 48kHz 파형(`ClickRt`)과 워커가 올리는 원자값(`ClickShared`).
//! 할당·락·시스템콜·로그 없음(이동만, 엔진 ⑥-3).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use band_dsp::config::INTERNAL_RATE;
use band_dsp::stats::AtomicF32;

/// 다음 클릭 없음.
pub(crate) const NO_CLICK: u64 = u64::MAX;
/// 클릭 파형 길이(48kHz 샘플) = 30ms.
pub(crate) const CLICK_LEN: usize = 1440;

pub(super) fn encode(frame: u64, accent: bool) -> u64 {
    (frame << 1) | accent as u64
}

pub(super) fn decode(v: u64) -> (u64, bool) {
    (v >> 1, v & 1 == 1)
}

/// 클릭 파형(48kHz): 사인 버스트, 1ms 상승 + 6ms 지수 감쇠. 첫 샘플부터 0이 아니다(시작 위치 판정용).
pub(crate) fn click_wave(freq_hz: f32, amp: f32) -> Box<[f32]> {
    let r = INTERNAL_RATE as f32;
    (0..CLICK_LEN)
        .map(|n| {
            let t = (n + 1) as f32 / r;
            let attack = ((n + 1) as f32 / (0.001 * r)).min(1.0);
            let decay = (-(n as f32) / (0.006 * r)).exp();
            amp * attack * decay * (std::f32::consts::TAU * freq_hz * t).sin()
        })
        .collect()
}

/// 워커 ↔ 출력 콜백 공유 상태. 모두 원자값.
#[derive(Debug)]
pub struct ClickShared {
    /// 다음 클릭: `frame << 1 | accent`(`out_played_frames` 기준 장치 프레임). 없으면 `NO_CLICK`.
    /// 워커가 올리고(CAS), 콜백이 시작하면서 `NO_CLICK`으로 되돌린다(CAS).
    pub next: AtomicU64,
    /// 48kHz 파형을 장치 레이트로 읽는 보폭(= 48000/장치 레이트). 엔진이 장치를 연 뒤 한 번 쓴다.
    pub step: AtomicF32,
    /// 울린 클릭 수.
    pub clicks: AtomicU64,
    /// 늦게 시작한 클릭 수(파형 중간부터 울렸거나 통째로 놓침).
    pub late: AtomicU64,
    /// 콜백 공백 뒤 제 시각이 이미 지나 건너뛴 클릭 수(PR #34 M3).
    pub skipped: AtomicU64,
}

impl Default for ClickShared {
    fn default() -> Self {
        Self { next: AtomicU64::new(NO_CLICK), step: AtomicF32::new(1.0), clicks: AtomicU64::new(0), late: AtomicU64::new(0), skipped: AtomicU64::new(0) }
    }
}

/// 출력 콜백 쪽 클릭 생성기(RT). 파형은 생성 시 미리 할당한다.
pub struct ClickRt {
    sh: Arc<ClickShared>,
    accent: Box<[f32]>,
    normal: Box<[f32]>,
    /// 이번 콜백에서 시작할 클릭: (콜백 안 프레임, 강박, 파형 시작 위치).
    start: Option<(usize, bool, f32)>,
    /// 울리는 중인 클릭: (강박, 파형 위치).
    playing: Option<(bool, f32)>,
    step: f32,
}

impl ClickRt {
    pub fn new(sh: Arc<ClickShared>) -> Self {
        Self { sh, accent: click_wave(1760.0, 0.6), normal: click_wave(1100.0, 0.45), start: None, playing: None, step: 1.0 }
    }

    /// RT: 콜백 시작. `before` = 이 콜백 첫 프레임 위치, `frames` = 이 콜백 프레임 수.
    /// `audible = false`(측정 자극이 장치 출력을 대신하는 중)이면 이번 콜백에 걸린 클릭을 소비만 하고 세지 않는다(PR #34 L1).
    pub(crate) fn begin(&mut self, before: u64, frames: usize, audible: bool) {
        self.start = None;
        self.step = self.sh.step.load();
        if self.step.is_nan() || self.step <= 0.0 {
            self.step = 1.0;
        }
        let v = self.sh.next.load(Ordering::Acquire);
        if v == NO_CLICK {
            return;
        }
        let (f, accent) = decode(v);
        if f >= before + frames as u64 {
            return;
        }
        // 이번 콜백에 걸리면 가져간다. 워커가 동시에 바꿨으면(CAS 실패) 다음 콜백에서 새 값을 본다.
        if self.sh.next.compare_exchange(v, NO_CLICK, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return;
        }
        if !audible {
            self.playing = None;
            return;
        }
        if f >= before {
            self.start = Some(((f - before) as usize, accent, 0.0));
            self.sh.clicks.fetch_add(1, Ordering::Relaxed);
        } else {
            // 늦음: 파형이 아직 남아 있으면 그 위치부터(남은 부분의 시각은 정확), 아니면 버린다.
            self.sh.late.fetch_add(1, Ordering::Relaxed);
            let pos = (before - f) as f32 * self.step;
            if pos < CLICK_LEN as f32 {
                self.start = Some((0, accent, pos));
                self.sh.clicks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// RT: 콜백 안 `i`번째 프레임의 클릭 샘플(게인 전).
    #[inline]
    pub(crate) fn sample(&mut self, i: usize) -> f32 {
        if let Some((at, accent, pos)) = self.start {
            if at == i {
                self.playing = Some((accent, pos));
                self.start = None;
            }
        }
        let Some((accent, pos)) = self.playing else {
            return 0.0;
        };
        let w = if accent { &self.accent } else { &self.normal };
        let idx = pos as usize;
        let frac = pos - idx as f32;
        let a = w.get(idx).copied().unwrap_or(0.0);
        let b = w.get(idx + 1).copied().unwrap_or(0.0);
        let next = pos + self.step;
        self.playing = (next < w.len() as f32).then_some((accent, next));
        a + (b - a) * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt_plays_click_at_exact_frame_and_late_start() {
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(1000 + 37, true), Ordering::Release);
        rt.begin(1000, 64, true);
        let out: Vec<f32> = (0..64).map(|i| rt.sample(i)).collect();
        assert!(out[..37].iter().all(|&x| x == 0.0));
        assert!(out[37] != 0.0);
        assert_eq!(sh.next.load(Ordering::Relaxed), NO_CLICK);
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 1);
        // 늦게 본 클릭: 10프레임 지난 위치부터
        let mut rt2 = ClickRt::new(sh.clone());
        sh.next.store(encode(990, false), Ordering::Release);
        rt2.begin(1000, 64, true);
        let x = rt2.sample(0);
        assert!((x - click_wave(1100.0, 0.45)[10]).abs() < 1e-6);
        assert_eq!(sh.late.load(Ordering::Relaxed), 1);
        // 파형이 끝날 만큼 늦으면 버린다
        sh.next.store(encode(0, false), Ordering::Release);
        let mut rt3 = ClickRt::new(sh.clone());
        rt3.begin(100_000, 64, true);
        assert!((0..64).all(|i| rt3.sample(i) == 0.0));
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rt_ignores_future_click() {
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(5000, false), Ordering::Release);
        rt.begin(1000, 64, true);
        assert!((0..64).all(|i| rt.sample(i) == 0.0));
        assert_ne!(sh.next.load(Ordering::Relaxed), NO_CLICK);
    }

    #[test]
    fn rt_swallows_click_during_measurement() {
        // 측정 자극이 출력을 대신하는 중에는 클릭을 소비만 하고 세지 않는다(PR #34 L1)
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(1010, true), Ordering::Release);
        rt.begin(1000, 64, false);
        assert!((0..64).all(|i| rt.sample(i) == 0.0));
        assert_eq!(sh.next.load(Ordering::Relaxed), NO_CLICK);
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 0);
        assert_eq!(sh.late.load(Ordering::Relaxed), 0);
    }
}
