//! 장치 레이트 ↔ 내부 48kHz 스트리밍 리샘플러. 품질 둘(P2-17e, `config::ResampleQuality`):
//! - 저지연 = rubato `FastFixedIn` Septic 다항식. 필터 지연 0.1ms 안, 고역이 깎이고(18kHz −1.5~2.4dB) 23kHz 에일리어싱이 크다(−3.7dB).
//! - 고음질 = rubato `SincFixedIn`(`SINC_LEN`탭). 18kHz까지 평탄하고 에일리어싱·이미지 −100dB대, 단계당 필터 지연
//!   `SINC_LEN`/2 입력 프레임(≈ 1.3~1.5ms).
//!
//! 장치가 48kHz면 둘 다 통과(변환·지연 없음). 측정표: 설계 문서 1절, `tests::frequency_response_table`.
//! 워커(비RT) 스레드에서만 사용한다. 생성 후에는 할당하지 않는다(sinc 보간표·버퍼 모두 `new`에서). 피어 공급용은 제어
//! 평면(`PeerMedia::new`, 커맨드 스레드)이 만들어 명령 큐로 워커에 넘기고 반환 큐로 돌려받아 해제한다(엔진 ⑧ 후속).

use std::collections::VecDeque;

use rubato::{
    FastFixedIn, PolynomialDegree, ResampleResult, Resampler, ResamplerConstructionError, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

use crate::config::ResampleQuality;

/// 리샘플러 입력 청크(프레임). 청크가 찰 때까지 입력이 묵으므로(0 ~ 청크−1 프레임) 작을수록 지연이 짧다. 16(0.33ms@48k):
/// 48(1ms)보다 44.1k 장치 종단 지연이 약 0.5ms 짧고(버퍼 128·2.5ms, 가짜 장치 A/B 3회) CPU 차이는 0.01%p 안이다(P2-17d ②).
/// 워커 출력 청크(`OUT_CHUNK` 64)·흔한 장치 버퍼(64·128·256)의 약수라 그 경로에서는 묵는 프레임이 0이다.
pub const CHUNK: usize = 16;

/// 고음질(sinc) 필터 길이(탭). 128이면 44.1k↔48k에서 18kHz까지 평탄하고, 단계당 64 입력 프레임(44.1k 1.45ms, 48k 1.33ms)
/// 지연이다(P2-17d 측정: 96탭은 18kHz −1.0dB).
pub(crate) const SINC_LEN: usize = 128;
/// sinc 차단 주파수(낮은 쪽 나이퀴스트 대비).
const SINC_CUTOFF: f32 = 0.92;
/// sinc 보간표 밀도. 표는 생성 때 한 번 만든다.
const SINC_OVERSAMPLING: usize = 256;

/// rubato `FastFixedIn`의 다항식 창 길이(`POLYNOMIAL_LEN_U`, 지연 계산용).
const POLY_LEN: usize = 8;

/// `from` → `to` 변환의 필터 지연(ms, 출력 기준 군지연). 같은 레이트면 0. 리샘플러를 만들지 않고 계산한다(통계·추정 지연용,
/// 만든 리샘플러의 `delay_ms`와 같은 값 — `tests::filter_delay_matches_resampler`). 식은 rubato의 `output_delay`와 같다.
pub fn filter_delay_ms(from_rate: u32, to_rate: u32, quality: ResampleQuality) -> f32 {
    if from_rate == to_rate || from_rate == 0 || to_rate == 0 {
        return 0.0;
    }
    let taps = if quality == ResampleQuality::LowLatency { POLY_LEN } else { SINC_LEN };
    let frames = (taps as f64 * to_rate as f64 / from_rate as f64 / 2.0) as usize;
    frames as f32 * 1000.0 / to_rate as f32
}

enum Inner {
    Poly(FastFixedIn<f32>),
    Sinc(SincFixedIn<f32>),
}

impl Inner {
    fn process_into_buffer(&mut self, input: &[Vec<f32>], output: &mut [Vec<f32>]) -> ResampleResult<(usize, usize)> {
        match self {
            Inner::Poly(r) => r.process_into_buffer(input, output, None),
            Inner::Sinc(r) => r.process_into_buffer(input, output, None),
        }
    }

    fn output_frames_max(&self) -> usize {
        match self {
            Inner::Poly(r) => r.output_frames_max(),
            Inner::Sinc(r) => r.output_frames_max(),
        }
    }

    #[cfg(test)]
    fn output_delay(&self) -> usize {
        match self {
            Inner::Poly(r) => r.output_delay(),
            Inner::Sinc(r) => r.output_delay(),
        }
    }
}

pub struct StreamResampler {
    channels: usize,
    /// 출력 레이트(만들 때 정한다). 넘겨받은 쪽이 자기 장치 레이트와 맞는지 확인한다.
    to_rate: u32,
    inner: Option<Inner>,
    in_planar: Vec<Vec<f32>>,
    in_fill: usize,
    out_planar: Vec<Vec<f32>>,
    out: VecDeque<f32>,
}

impl StreamResampler {
    /// `quality`: `LowLatency` = 다항식, 그 밖(`HighQuality`, 풀지 않은 `Auto`) = sinc. `Auto`는 보통 호출자가
    /// `EngineConfig::resample_quality()`로 풀어서 넘긴다.
    pub fn new(from_rate: u32, to_rate: u32, channels: usize, quality: ResampleQuality) -> Result<Self, String> {
        let channels = channels.max(1);
        let quality = match quality {
            ResampleQuality::LowLatency => ResampleQuality::LowLatency,
            ResampleQuality::Auto | ResampleQuality::HighQuality => ResampleQuality::HighQuality,
        };
        let inner = if from_rate == to_rate {
            None
        } else {
            let ratio = to_rate as f64 / from_rate as f64;
            let err = |e: ResamplerConstructionError| format!("리샘플러 생성 실패: {e}");
            Some(if quality == ResampleQuality::LowLatency {
                Inner::Poly(FastFixedIn::<f32>::new(ratio, 1.0, PolynomialDegree::Septic, CHUNK, channels).map_err(err)?)
            } else {
                let params = SincInterpolationParameters {
                    sinc_len: SINC_LEN,
                    f_cutoff: SINC_CUTOFF,
                    oversampling_factor: SINC_OVERSAMPLING,
                    interpolation: SincInterpolationType::Linear,
                    window: WindowFunction::BlackmanHarris2,
                };
                Inner::Sinc(SincFixedIn::<f32>::new(ratio, 1.0, params, CHUNK, channels).map_err(err)?)
            })
        };
        let out_max = inner.as_ref().map(|r| r.output_frames_max()).unwrap_or(0);
        Ok(Self {
            channels,
            to_rate,
            in_planar: (0..channels).map(|_| vec![0.0; CHUNK]).collect(),
            in_fill: 0,
            out_planar: (0..channels).map(|_| vec![0.0; out_max]).collect(),
            // 1초 분량까지 여유 있게 미리 할당
            out: VecDeque::with_capacity(to_rate as usize * channels),
            inner,
        })
    }

    /// 출력 레이트(Hz).
    pub(crate) fn out_rate(&self) -> u32 {
        self.to_rate
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// 필터 지연(ms, 출력 기준 군지연, rubato `output_delay`). 통과면 0. 청크 대기(0 ~ `CHUNK`−1 입력 프레임)는 들어 있지 않다.
    /// 시험용 — 엔진은 리샘플러 없이 같은 값을 내는 `filter_delay_ms`를 쓴다.
    #[cfg(test)]
    fn delay_ms(&self) -> f32 {
        self.inner.as_ref().map_or(0.0, |r| r.output_delay() as f32 * 1000.0 / self.to_rate as f32)
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
                match rs.process_into_buffer(&self.in_planar, &mut self.out_planar) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    const BOTH: [ResampleQuality; 2] = [ResampleQuality::LowLatency, ResampleQuality::HighQuality];

    fn run(from: u32, to: u32, ch: usize, seconds: usize, q: ResampleQuality) -> (usize, usize) {
        let mut r = StreamResampler::new(from, to, ch, q).unwrap();
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
        for q in BOTH {
            let (got, want) = run(44_100, 48_000, 2, 10, q);
            // 청크 하나 + 필터 지연 이내
            assert!((want as i64 - got as i64).abs() < 128, "{q:?} got {got} want {want}");
        }
    }

    #[test]
    fn length_preserved_down() {
        for q in BOTH {
            let (got, want) = run(96_000, 48_000, 1, 10, q);
            assert!((want as i64 - got as i64).abs() < 128, "{q:?} got {got} want {want}");
            let (got, want) = run(48_000, 44_100, 2, 10, q);
            assert!((want as i64 - got as i64).abs() < 128, "{q:?} got {got} want {want}");
        }
    }

    #[test]
    fn passthrough_exact() {
        for q in BOTH {
            let (got, want) = run(48_000, 48_000, 2, 3, q);
            assert_eq!(got, want);
            let r = StreamResampler::new(48_000, 48_000, 2, q).unwrap();
            assert!(r.is_passthrough());
            assert_eq!(r.delay_ms(), 0.0, "48k 장치는 품질과 상관없이 지연 0");
        }
    }

    #[test]
    fn sine_survives() {
        for q in BOTH {
            let mut r = StreamResampler::new(44_100, 48_000, 1, q).unwrap();
            let input: Vec<f32> = (0..44_100).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44_100.0).sin()).collect();
            r.push(&input);
            let mut out = vec![0f32; 48_000];
            let n = r.pop(&mut out);
            let rms = (out[1000..n].iter().map(|x| x * x).sum::<f32>() / (n - 1000) as f32).sqrt();
            assert!((rms - 0.707).abs() < 0.02, "{q:?} rms {rms}");
        }
    }

    #[test]
    fn filter_delay_matches_resampler() {
        for q in [ResampleQuality::LowLatency, ResampleQuality::HighQuality, ResampleQuality::Auto] {
            for (from, to) in [(44_100, 48_000), (48_000, 44_100), (96_000, 48_000), (48_000, 96_000), (48_000, 48_000), (22_050, 48_000)] {
                let r = StreamResampler::new(from, to, 2, q).unwrap();
                assert_eq!(filter_delay_ms(from, to, q), r.delay_ms(), "{q:?} {from}→{to}");
            }
        }
    }

    #[test]
    fn unresolved_auto_is_sinc() {
        let auto = StreamResampler::new(44_100, 48_000, 2, ResampleQuality::Auto).unwrap().delay_ms();
        let hq = StreamResampler::new(44_100, 48_000, 2, ResampleQuality::HighQuality).unwrap().delay_ms();
        assert_eq!(auto, hq, "풀지 않은 Auto는 고음질(sinc)");
        assert_eq!(filter_delay_ms(44_100, 48_000, ResampleQuality::Auto), hq);
    }

    /// 사인 `hz`(진폭 0.5)를 `from` → `to`로 1초 변환해 출력에서 (`probe_hz` 성분 진폭, 전체 피크 환산 RMS)를 입력 진폭 대비
    /// dB로 돌려준다. 성분 진폭은 최소제곱 사인 맞춤. 결정적(입력·리샘플러 모두 고정).
    fn tone(q: ResampleQuality, from: u32, to: u32, hz: f64, probe_hz: f64) -> (f64, f64) {
        let mut r = StreamResampler::new(from, to, 1, q).unwrap();
        let input: Vec<f32> = (0..from as usize).map(|i| (0.5 * (i as f64 * 2.0 * std::f64::consts::PI * hz / from as f64).sin()) as f32).collect();
        let mut out = vec![0f32; to as usize * 2];
        let mut n = 0;
        for c in input.chunks(100) {
            r.push(c);
            n += r.pop(&mut out[n..]);
        }
        // 앞뒤 0.1초(필터 과도·청크 꼬리)를 빼고 맞춘다.
        let seg = &out[to as usize / 10..n - to as usize / 10];
        let w = 2.0 * std::f64::consts::PI * probe_hz / to as f64;
        let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &y) in seg.iter().enumerate() {
            let (s, c) = (w * i as f64).sin_cos();
            ss += s * s;
            sc += s * c;
            cc += c * c;
            ys += y as f64 * s;
            yc += y as f64 * c;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let amp = (a * a + b * b).sqrt();
        let rms = (seg.iter().map(|&y| y as f64 * y as f64).sum::<f64>() / seg.len() as f64).sqrt();
        let db = |x: f64| 20.0 * (x / 0.5).max(1e-12).log10();
        (db(amp), db(rms * std::f64::consts::SQRT_2))
    }

    /// 44.1k ↔ 48k 주파수 응답(P2-17e 결정의 근거, 설계 문서 1절 표). 고음질은 18kHz까지 평탄하고 23kHz 에일리어싱·이미지를
    /// 크게 줄인다. 저지연(다항식)은 지금 성질(큰 23kHz 에일리어싱)을 고정해 둔다. 표 출력: `-- --nocapture`.
    #[test]
    fn frequency_response_table() {
        let mut rows = Vec::new();
        for q in BOTH {
            // 입력 단계(장치 44.1k → 48k): 통과 이득, 21.1kHz 입력의 이미지(44.1k 기준 반사 44.1 − 21.1 = 23kHz)
            let up15 = tone(q, 44_100, 48_000, 15_000.0, 15_000.0).0;
            let up18 = tone(q, 44_100, 48_000, 18_000.0, 18_000.0).0;
            let up20 = tone(q, 44_100, 48_000, 20_000.0, 20_000.0).0;
            let up_img23 = tone(q, 44_100, 48_000, 21_100.0, 23_000.0).0;
            // 출력 단계(48k → 장치 44.1k): 통과 이득, 23kHz 입력의 에일리어싱(44.1k에서 21.1kHz로 접힌다 — 출력 전체가 에일리어싱)
            let dn15 = tone(q, 48_000, 44_100, 15_000.0, 15_000.0).0;
            let dn18 = tone(q, 48_000, 44_100, 18_000.0, 18_000.0).0;
            let dn20 = tone(q, 48_000, 44_100, 20_000.0, 20_000.0).0;
            let dn_alias23 = tone(q, 48_000, 44_100, 23_000.0, 21_100.0).1;
            let d_up = StreamResampler::new(44_100, 48_000, 1, q).unwrap().delay_ms();
            let d_dn = StreamResampler::new(48_000, 44_100, 1, q).unwrap().delay_ms();
            println!(
                "{q:?}: 지연 입력 {d_up:.2}ms 출력 {d_dn:.2}ms | 44.1→48 15k {up15:.2} 18k {up18:.2} 20k {up20:.2} 23k이미지 {up_img23:.1}dB | \
                 48→44.1 15k {dn15:.2} 18k {dn18:.2} 20k {dn20:.2} 23k에일리어싱 {dn_alias23:.1}dB"
            );
            rows.push((d_up, d_dn, [up15, up18, dn15, dn18], up_img23, dn_alias23));
        }
        let (pd_up, pd_dn, _, p_img, p_alias) = rows[0];
        let (sd_up, sd_dn, s_pass, s_img, s_alias) = rows[1];
        // 고음질: 18kHz까지 평탄(±0.2dB), 23kHz 에일리어싱·이미지 −60dB 아래
        for g in s_pass {
            assert!(g.abs() < 0.2, "sinc 통과대역 {g:.2}dB");
        }
        assert!(s_img < -60.0 && s_alias < -60.0, "sinc 이미지 {s_img:.1} 에일리어싱 {s_alias:.1}");
        // 저지연(다항식): 23kHz 에일리어싱이 크다(−20dB 위) — 고음질이 고치는 것.
        assert!(p_alias > -20.0, "다항식 에일리어싱 {p_alias:.1}(이미지 {p_img:.1})");
        // 지연: sinc 단계당 ≈ SINC_LEN/2 입력 프레임, 다항식은 0.2ms 아래
        assert!(pd_up < 0.2 && pd_dn < 0.2, "다항식 지연 {pd_up} {pd_dn}");
        assert!((1.2..1.6).contains(&sd_up) && (1.2..1.6).contains(&sd_dn), "sinc 지연 {sd_up} {sd_dn}");
    }

    /// CPU(1코어, 실시간 대비): 스테레오 10초. 측정용.
    #[test]
    #[ignore = "측정용: cargo test -p band-dsp --release --lib resample::tests::cpu_bench -- --ignored --nocapture"]
    fn cpu_bench() {
        for q in BOTH {
            for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100)] {
                let mut r = StreamResampler::new(from, to, 2, q).unwrap();
                let secs = 10usize;
                let input: Vec<f32> = (0..from as usize * 2).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
                let mut out = vec![0f32; 8192];
                let t = std::time::Instant::now();
                // 장치 버퍼 128프레임 한 번(push + pop)의 최악 시간 — 워커 반복 하나에서 이 단계가 차지하는 몫
                let mut worst = std::time::Duration::ZERO;
                for _ in 0..secs {
                    for c in input.chunks(128 * 2) {
                        let t1 = std::time::Instant::now();
                        r.push(c);
                        while r.pop(&mut out) > 0 {}
                        worst = worst.max(t1.elapsed());
                    }
                }
                let pct = t.elapsed().as_secs_f64() / secs as f64 * 100.0;
                let period_us = 128.0 * 1e6 / from as f64;
                println!(
                    "{q:?} {from}→{to} 스테레오: CPU {pct:.3}% (1코어, 실시간 대비), 128프레임당 평균 {:.1}µs 최악 {:.1}µs (주기 {period_us:.0}µs)",
                    pct / 100.0 * period_us,
                    worst.as_secs_f64() * 1e6
                );
            }
        }
    }
}
