//! Opus / PCM16 인코드·디코드 (설계 문서 1.3절). 48kHz, f32 인터리브.

use super::config::{CodecKind, INTERNAL_RATE};

pub enum Encoder {
    Opus { enc: opus::Encoder, channels: usize },
    Pcm16 { channels: usize },
}

impl Encoder {
    pub fn new(codec: CodecKind, channels: usize, bitrate: u32) -> Result<Self, String> {
        match codec {
            CodecKind::Pcm16 => Ok(Encoder::Pcm16 { channels }),
            CodecKind::Opus => {
                let ch = if channels == 2 { opus::Channels::Stereo } else { opus::Channels::Mono };
                // opus 크레이트의 LowDelay = OPUS_APPLICATION_RESTRICTED_LOWDELAY(2051)
                let mut enc = opus::Encoder::new(INTERNAL_RATE, ch, opus::Application::LowDelay)
                    .map_err(|e| format!("Opus 인코더 생성 실패: {e}"))?;
                enc.set_bitrate(opus::Bitrate::Bits(bitrate as i32))
                    .map_err(|e| format!("Opus 비트레이트 설정 실패: {e}"))?;
                enc.set_inband_fec(true).map_err(|e| format!("Opus FEC 설정 실패: {e}"))?;
                enc.set_packet_loss_perc(5).map_err(|e| format!("Opus 손실률 설정 실패: {e}"))?;
                Ok(Encoder::Opus { enc, channels })
            }
        }
    }

    pub fn is_opus(&self) -> bool {
        matches!(self, Encoder::Opus { .. })
    }

    pub fn channels(&self) -> usize {
        match self {
            Encoder::Opus { channels, .. } | Encoder::Pcm16 { channels } => *channels,
        }
    }

    /// `frame`(인터리브)을 인코딩해 `out`에 쓰고 바이트 수를 반환.
    pub fn encode(&mut self, frame: &[f32], out: &mut [u8]) -> Result<usize, String> {
        match self {
            Encoder::Opus { enc, .. } => {
                enc.encode_float(frame, out).map_err(|e| format!("Opus 인코드 실패: {e}"))
            }
            Encoder::Pcm16 { .. } => {
                let need = frame.len() * 2;
                if out.len() < need {
                    return Err("PCM16 출력 버퍼 부족".into());
                }
                for (s, o) in frame.iter().zip(out.as_chunks_mut::<2>().0.iter_mut()) {
                    let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    *o = v.to_le_bytes();
                }
                Ok(need)
            }
        }
    }
}

pub enum Decoder {
    Opus { dec: opus::Decoder, channels: usize },
    Pcm16 { channels: usize },
}

impl Decoder {
    pub fn new(opus: bool, channels: usize) -> Result<Self, String> {
        if opus {
            let ch = if channels == 2 { opus::Channels::Stereo } else { opus::Channels::Mono };
            let dec = opus::Decoder::new(INTERNAL_RATE, ch)
                .map_err(|e| format!("Opus 디코더 생성 실패: {e}"))?;
            Ok(Decoder::Opus { dec, channels })
        } else {
            Ok(Decoder::Pcm16 { channels })
        }
    }

    pub fn matches(&self, opus: bool, channels: usize) -> bool {
        match self {
            Decoder::Opus { channels: c, .. } => opus && *c == channels,
            Decoder::Pcm16 { channels: c } => !opus && *c == channels,
        }
    }

    pub fn channels(&self) -> usize {
        match self {
            Decoder::Opus { channels, .. } | Decoder::Pcm16 { channels } => *channels,
        }
    }

    /// 패킷 디코드. `out`은 정확히 한 프레임(frame_samples × channels) 크기여야 한다.
    /// 반환값은 채널당 샘플 수.
    pub fn decode(&mut self, payload: &[u8], out: &mut [f32]) -> Result<usize, String> {
        match self {
            Decoder::Opus { dec, .. } => {
                dec.decode_float(payload, out, false).map_err(|e| format!("Opus 디코드 실패: {e}"))
            }
            Decoder::Pcm16 { channels } => {
                let n = (payload.len() / 2).min(out.len());
                for (o, b) in out[..n].iter_mut().zip(payload.as_chunks::<2>().0) {
                    *o = i16::from_le_bytes(*b) as f32 / 32768.0;
                }
                out[n..].fill(0.0);
                Ok(n / *channels)
            }
        }
    }

    /// Opus 전용: 다음 패킷의 in-band FEC로 손실 프레임 복원.
    pub fn decode_fec(&mut self, next_payload: &[u8], out: &mut [f32]) -> Result<usize, String> {
        match self {
            Decoder::Opus { dec, .. } => {
                dec.decode_float(next_payload, out, true).map_err(|e| format!("Opus FEC 실패: {e}"))
            }
            Decoder::Pcm16 { .. } => Err("PCM은 FEC 없음".into()),
        }
    }

    /// Opus 전용: 디코더 PLC.
    pub fn decode_plc(&mut self, out: &mut [f32]) -> Result<usize, String> {
        match self {
            Decoder::Opus { dec, .. } => {
                dec.decode_float(&[], out, false).map_err(|e| format!("Opus PLC 실패: {e}"))
            }
            Decoder::Pcm16 { .. } => Err("PCM은 디코더 PLC 없음".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, ch: usize, phase0: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * ch);
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * 440.0 * (i + phase0) as f32 / 48000.0).sin() * 0.5;
            for _ in 0..ch {
                v.push(s);
            }
        }
        v
    }

    #[test]
    fn pcm16_roundtrip_mono_sends_one_channel() {
        let mut e = Encoder::new(CodecKind::Pcm16, 1, 0).unwrap();
        let mut d = Decoder::new(false, 1).unwrap();
        let f = sine(240, 1, 0);
        let mut buf = [0u8; 1200];
        let n = e.encode(&f, &mut buf).unwrap();
        assert_eq!(n, 480, "모노는 1채널만(복제 금지)");
        let mut out = vec![0.0; 240];
        assert_eq!(d.decode(&buf[..n], &mut out).unwrap(), 240);
        for (a, b) in f.iter().zip(&out) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn opus_all_frame_sizes() {
        for &(ch, n) in &[(1usize, 120usize), (1, 240), (1, 480), (2, 120), (2, 240), (2, 480)] {
            let mut e = Encoder::new(CodecKind::Opus, ch, if ch == 2 { 160_000 } else { 96_000 }).unwrap();
            let mut d = Decoder::new(true, ch).unwrap();
            let mut buf = [0u8; 1166];
            let mut out = vec![0.0; n * ch];
            let mut energy = 0.0;
            for k in 0..20 {
                let f = sine(n, ch, k * n);
                let len = e.encode(&f, &mut buf).unwrap();
                assert!(len > 0 && len <= 1166);
                assert_eq!(d.decode(&buf[..len], &mut out).unwrap(), n);
                energy += out.iter().map(|x| x * x).sum::<f32>();
            }
            assert!(energy > 1.0, "디코드 결과가 무음이면 안 됨 ch={ch} n={n}");
            assert_eq!(d.decode_plc(&mut out).unwrap(), n);
        }
    }
}
