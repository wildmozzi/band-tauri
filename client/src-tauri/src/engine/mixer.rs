//! 믹서: 피어별 gain/mute, 모니터, 마스터, soft clip (설계 문서 1절).
//! 내부 버스는 48kHz 스테레오 인터리브 f32.

use std::sync::atomic::{AtomicBool, Ordering};

use super::stats::AtomicF32;

/// 피어별 믹스 제어값(커맨드 스레드 → 워커).
#[derive(Debug)]
pub struct PeerMixCtl {
    pub gain: AtomicF32,
    pub muted: AtomicBool,
    /// 워커가 갱신하는 피어 레벨(피크).
    pub level: AtomicF32,
}

impl Default for PeerMixCtl {
    fn default() -> Self {
        Self { gain: AtomicF32::new(1.0), muted: AtomicBool::new(false), level: AtomicF32::new(0.0) }
    }
}

impl PeerMixCtl {
    pub fn effective_gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            self.gain.load()
        }
    }
}

/// 엔진 전역 믹스 제어값. 엔진 재시작 간에도 유지된다.
#[derive(Debug)]
pub struct MixControls {
    pub monitor_enabled: AtomicBool,
    pub monitor_gain: AtomicF32,
    pub master_gain: AtomicF32,
}

impl Default for MixControls {
    fn default() -> Self {
        Self {
            monitor_enabled: AtomicBool::new(false),
            monitor_gain: AtomicF32::new(1.0),
            master_gain: AtomicF32::new(1.0),
        }
    }
}

pub fn clamp_gain(g: f32) -> f32 {
    if g.is_finite() {
        g.clamp(0.0, 4.0)
    } else {
        0.0
    }
}

/// 스테레오 버스에 스테레오 소스를 gain으로 더한다.
pub fn add_stereo(bus: &mut [f32], src: &[f32], gain: f32) {
    if gain == 0.0 {
        return;
    }
    for (b, s) in bus.iter_mut().zip(src) {
        *b += s * gain;
    }
}

/// 내부 채널 수(1/2) 소스를 스테레오 버스에 더한다.
pub fn add_internal(bus: &mut [f32], src: &[f32], src_channels: usize, gain: f32) {
    if gain == 0.0 {
        return;
    }
    if src_channels == 2 {
        add_stereo(bus, src, gain);
    } else {
        for (b, s) in bus.as_chunks_mut::<2>().0.iter_mut().zip(src) {
            b[0] += s * gain;
            b[1] += s * gain;
        }
    }
}

pub fn scale(buf: &mut [f32], gain: f32) {
    if gain != 1.0 {
        for x in buf {
            *x *= gain;
        }
    }
}

const CLIP_KNEE: f32 = 0.8;

/// 부드러운 클리핑: |x| ≤ 0.8은 그대로, 그 위는 tanh로 1.0에 점근.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_KNEE {
        x
    } else {
        let room = 1.0 - CLIP_KNEE;
        let y = CLIP_KNEE + room * ((a - CLIP_KNEE) / room).tanh();
        y.copysign(x)
    }
}

pub fn soft_clip_buf(buf: &mut [f32]) {
    for x in buf {
        *x = soft_clip(*x);
    }
}

pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_properties() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.8), -0.8);
        for i in 0..1000 {
            let x = i as f32 * 0.01;
            let y = soft_clip(x);
            assert!((0.0..=1.0).contains(&y));
            assert_eq!(soft_clip(-x), -y);
            // 단조 증가
            assert!(soft_clip(x + 0.001) >= y);
        }
        // knee에서 연속
        assert!((soft_clip(0.8001) - 0.8001).abs() < 1e-3);
        assert!(soft_clip(100.0) <= 1.0);
    }

    #[test]
    fn mixing_with_gain_and_mute() {
        let ctl = PeerMixCtl::default();
        let mut bus = vec![0.0f32; 8];
        let a = vec![0.5f32; 8];
        add_stereo(&mut bus, &a, ctl.effective_gain());
        assert!(bus.iter().all(|&x| (x - 0.5).abs() < 1e-6));
        ctl.gain.store(0.5);
        add_stereo(&mut bus, &a, ctl.effective_gain());
        assert!(bus.iter().all(|&x| (x - 0.75).abs() < 1e-6));
        ctl.muted.store(true, Ordering::Relaxed);
        add_stereo(&mut bus, &a, ctl.effective_gain());
        assert!(bus.iter().all(|&x| (x - 0.75).abs() < 1e-6));
    }

    #[test]
    fn mono_to_stereo_bus() {
        let mut bus = vec![0.0f32; 6];
        add_internal(&mut bus, &[0.1, 0.2, 0.3], 1, 2.0);
        assert_eq!(bus, vec![0.2, 0.2, 0.4, 0.4, 0.6, 0.6]);
    }

    #[test]
    fn gain_clamping() {
        assert_eq!(clamp_gain(f32::NAN), 0.0);
        assert_eq!(clamp_gain(-1.0), 0.0);
        assert_eq!(clamp_gain(10.0), 4.0);
        assert_eq!(clamp_gain(0.7), 0.7);
    }

    #[test]
    fn peak_level() {
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
    }
}
