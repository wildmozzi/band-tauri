//! 믹서: 피어별 gain/mute, 모니터, 마스터, soft clip (설계 문서 1절).
//! 내부 버스는 48kHz 스테레오 인터리브 f32.

use std::sync::atomic::{AtomicBool, Ordering};

use super::stats::AtomicF32;

/// 피어별 믹스 제어값(커맨드 스레드 → 워커).
#[derive(Debug)]
pub struct PeerMixCtl {
    pub gain: AtomicF32,
    pub muted: AtomicBool,
    /// -1(왼쪽)..1(오른쪽), 0=중앙.
    pub pan: AtomicF32,
    /// 워커가 갱신하는 피어 레벨(0..1 선형 RMS).
    pub level: AtomicF32,
}

impl Default for PeerMixCtl {
    fn default() -> Self {
        Self {
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            pan: AtomicF32::new(0.0),
            level: AtomicF32::new(0.0),
        }
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

    /// gain·mute·pan을 반영한 (L, R) 게인.
    pub fn lr_gains(&self) -> (f32, f32) {
        let g = self.effective_gain();
        let (l, r) = pan_gains(self.pan.load());
        (g * l, g * r)
    }
}

pub fn clamp_pan(p: f32) -> f32 {
    if p.is_finite() {
        p.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Equal-power 팬. L² + R² = 2로 일정하며 중앙(0)에서 (1, 1)이 되도록 √2로 정규화했다
/// (팬을 쓰지 않는 기본 상태의 음량이 바뀌지 않게). 끝(±1)에서는 한쪽 √2, 반대쪽 0.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (clamp_pan(pan) + 1.0) * std::f32::consts::FRAC_PI_4;
    (theta.cos() * std::f32::consts::SQRT_2, theta.sin() * std::f32::consts::SQRT_2)
}

/// 스테레오 버스에 스테레오 소스를 L/R 게인으로 더한다.
pub fn add_stereo_lr(bus: &mut [f32], src: &[f32], gl: f32, gr: f32) {
    if gl == 0.0 && gr == 0.0 {
        return;
    }
    for (b, s) in bus.as_chunks_mut::<2>().0.iter_mut().zip(src.as_chunks::<2>().0) {
        b[0] += s[0] * gl;
        b[1] += s[1] * gr;
    }
}

/// 엔진 전역 믹스 제어값. 엔진 재시작 간에도 유지된다.
#[derive(Debug)]
pub struct MixControls {
    /// 내 입력 뮤트: 송신/모니터/믹스 구독에서 무음(레벨 미터는 계속).
    pub input_muted: AtomicBool,
    pub monitor_enabled: AtomicBool,
    pub monitor_gain: AtomicF32,
    pub master_gain: AtomicF32,
}

impl Default for MixControls {
    fn default() -> Self {
        Self {
            input_muted: AtomicBool::new(false),
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

/// 0..1 선형 RMS.
pub fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt().min(1.0)
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
    fn rms_level() {
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[3.0, 3.0]), 1.0, "0..1로 제한");
    }

    #[test]
    fn equal_power_pan() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6, "중앙은 단위 게인");
        let (l, r) = pan_gains(-1.0);
        assert!((l - 2f32.sqrt()).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = pan_gains(1.0);
        assert!(l.abs() < 1e-6 && (r - 2f32.sqrt()).abs() < 1e-6);
        for i in -10..=10 {
            let (l, r) = pan_gains(i as f32 / 10.0);
            assert!((l * l + r * r - 2.0).abs() < 1e-5, "파워 일정");
        }
        assert_eq!(pan_gains(f32::NAN), pan_gains(0.0));
        assert_eq!(pan_gains(5.0), pan_gains(1.0));

        let ctl = PeerMixCtl::default();
        ctl.pan.store(1.0);
        let mut bus = vec![0.0f32; 4];
        let (gl, gr) = ctl.lr_gains();
        add_stereo_lr(&mut bus, &[0.5, 0.5, 0.5, 0.5], gl, gr);
        assert!(bus[0].abs() < 1e-6 && (bus[1] - 0.5 * 2f32.sqrt()).abs() < 1e-6);
        ctl.muted.store(true, Ordering::Relaxed);
        assert_eq!(ctl.lr_gains(), (0.0, 0.0));
    }
}
