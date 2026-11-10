//! 장치 레이트 ↔ 내부 48kHz 스트리밍 리샘플러 (rubato `FastFixedIn`).
//! 워커(비RT) 스레드에서만 사용한다. 생성 후에는 할당하지 않는다.

use std::collections::VecDeque;

use rubato::{FastFixedIn, PolynomialDegree, Resampler};

/// 리샘플러 입력 청크(프레임). 작을수록 지연이 짧다(48 = 1ms@48k).
pub const CHUNK: usize = 48;

pub struct StreamResampler {
    channels: usize,
    inner: Option<FastFixedIn<f32>>,
    in_planar: Vec<Vec<f32>>,
    in_fill: usize,
    out_planar: Vec<Vec<f32>>,
    out: VecDeque<f32>,
}

impl StreamResampler {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Result<Self, String> {
        let channels = channels.max(1);
        let inner = if from_rate == to_rate {
            None
        } else {
            let ratio = to_rate as f64 / from_rate as f64;
            Some(
                FastFixedIn::<f32>::new(ratio, 1.0, PolynomialDegree::Septic, CHUNK, channels)
                    .map_err(|e| format!("리샘플러 생성 실패: {e}"))?,
            )
        };
        let out_max = inner.as_ref().map(|r| r.output_frames_max()).unwrap_or(0);
        Ok(Self {
            channels,
            in_planar: (0..channels).map(|_| vec![0.0; CHUNK]).collect(),
            in_fill: 0,
            out_planar: (0..channels).map(|_| vec![0.0; out_max]).collect(),
            // 1초 분량까지 여유 있게 미리 할당
            out: VecDeque::with_capacity(to_rate as usize * channels),
            inner,
        })
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// 인터리브 입력 추가. 청크가 찰 때마다 변환한다.
    pub fn push(&mut self, interleaved: &[f32]) {
        let ch = self.channels;
        let Some(rs) = self.inner.as_mut() else {
            self.out.extend(interleaved.iter().copied());
            return;
        };
        for frame in interleaved.chunks_exact(ch) {
            for (c, s) in frame.iter().enumerate() {
                self.in_planar[c][self.in_fill] = *s;
            }
            self.in_fill += 1;
            if self.in_fill == CHUNK {
                self.in_fill = 0;
                match rs.process_into_buffer(&self.in_planar, &mut self.out_planar, None) {
                    Ok((_, n)) => {
                        for i in 0..n {
                            for c in 0..ch {
                                self.out.push_back(self.out_planar[c][i]);
                            }
                        }
                    }
                    Err(e) => log::warn!("리샘플 실패: {e}"),
                }
            }
        }
    }

    /// 청크가 차기를 기다리는 입력 프레임 수(아직 변환되지 않음).
    pub fn pending_input(&self) -> usize {
        if self.inner.is_some() { self.in_fill } else { 0 }
    }

    /// 꺼낼 수 있는 출력 프레임 수.
    pub fn available(&self) -> usize {
        self.out.len() / self.channels
    }

    /// 최대 `out.len()/channels` 프레임을 꺼낸다. 꺼낸 프레임 수 반환.
    pub fn pop(&mut self, out: &mut [f32]) -> usize {
        let frames = (out.len() / self.channels).min(self.available());
        let n = frames * self.channels;
        for (o, s) in out[..n].iter_mut().zip(self.out.drain(..n)) {
            *o = s;
        }
        frames
    }

    /// 출력 FIFO에서 앞쪽 프레임을 버린다(드리프트로 쌓인 경우).
    pub fn discard(&mut self, frames: usize) {
        let n = (frames * self.channels).min(self.out.len());
        self.out.drain(..n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    fn run(from: u32, to: u32, ch: usize, seconds: usize) -> (usize, usize) {
        let mut r = StreamResampler::new(from, to, ch).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        let total_in = from as usize * seconds;
        let mut fed = 0;
        let mut got = 0;
        let mut tmp = vec![0f32; 4096 * ch];
        while fed < total_in {
            let n = rng.gen_range(1..700).min(total_in - fed);
            let chunk: Vec<f32> = (0..n * ch).map(|i| ((fed * ch + i) as f32 * 0.001).sin()).collect();
            r.push(&chunk);
            fed += n;
            got += r.pop(&mut tmp);
        }
        got += r.pop(&mut tmp);
        (got, to as usize * seconds)
    }

    #[test]
    fn length_preserved_up() {
        let (got, want) = run(44_100, 48_000, 2, 10);
        // 청크 하나 + 필터 지연 이내
        assert!((want as i64 - got as i64).abs() < 128, "got {got} want {want}");
    }

    #[test]
    fn length_preserved_down() {
        let (got, want) = run(96_000, 48_000, 1, 10);
        assert!((want as i64 - got as i64).abs() < 128, "got {got} want {want}");
        let (got, want) = run(48_000, 44_100, 2, 10);
        assert!((want as i64 - got as i64).abs() < 128, "got {got} want {want}");
    }

    #[test]
    fn passthrough_exact() {
        let (got, want) = run(48_000, 48_000, 2, 3);
        assert_eq!(got, want);
    }

    #[test]
    fn sine_survives() {
        let mut r = StreamResampler::new(44_100, 48_000, 1).unwrap();
        let input: Vec<f32> = (0..44_100).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44_100.0).sin()).collect();
        r.push(&input);
        let mut out = vec![0f32; 48_000];
        let n = r.pop(&mut out);
        let rms = (out[1000..n].iter().map(|x| x * x).sum::<f32>() / (n - 1000) as f32).sqrt();
        assert!((rms - 0.707).abs() < 0.02, "rms {rms}");
    }
}
