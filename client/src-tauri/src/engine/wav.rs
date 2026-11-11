//! 녹음 파일 쓰기(P2-4): WAV(24비트 PCM / 32비트 float), 4GB를 넘으면 RF64(EBU Tech 3306)로 바꾼다.
//!
//! 헤더 앞쪽에 `JUNK` 청크(28바이트)를 미리 둔다. 마무리할 때 RIFF 크기가 u32를 넘으면 `RIFF`→`RF64`,
//! `JUNK`→`ds64`(RIFF·data 크기와 프레임 수 u64)로 바꾸고 32비트 크기 칸은 0xFFFFFFFF로 둔다. 4GB 아래면 보통 WAV다
//! (`JUNK`는 모든 리더가 건너뛴다). 쓰는 동안에도 몇 초마다 헤더 크기를 고쳐 써서, 앱이 죽어도 그때까지는 읽힌다.
//! 비RT(녹음 쓰기 스레드) 전용이다.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 녹음 샘플 형식.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordFormat {
    /// 24비트 정수 PCM(기본, float보다 25% 작다).
    #[default]
    Wav24,
    /// 32비트 IEEE float(클리핑 없이 원본 그대로).
    WavF32,
}

impl RecordFormat {
    pub fn bytes_per_sample(self) -> u16 {
        match self {
            RecordFormat::Wav24 => 3,
            RecordFormat::WavF32 => 4,
        }
    }
}

const JUNK_PAYLOAD: u32 = 28;
const HEADER_REFRESH: Duration = Duration::from_secs(5);

/// 헤더 바이트와 data 크기 칸 위치를 만든다. `data_bytes`가 크면 RF64 헤더가 된다.
/// 반환: (헤더, 헤더 길이 = data 시작 위치).
pub fn header_bytes(fmt: RecordFormat, channels: u16, rate: u32, data_bytes: u64) -> Vec<u8> {
    let bps = fmt.bytes_per_sample();
    let block_align = channels * bps;
    let float = fmt == RecordFormat::WavF32;
    let frames = data_bytes / block_align.max(1) as u64;
    // fmt: PCM 16바이트, float는 cbSize를 더한 18바이트 + fact 청크(비PCM 필수)
    let fmt_len: u32 = if float { 18 } else { 16 };
    let header_len = 12 + (8 + JUNK_PAYLOAD) + (8 + fmt_len) + if float { 12 } else { 0 } + 8;
    let riff_size = header_len as u64 - 8 + data_bytes;
    let rf64 = riff_size > u32::MAX as u64;
    let mut h = Vec::with_capacity(header_len as usize);
    h.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
    h.extend_from_slice(&(if rf64 { u32::MAX } else { riff_size as u32 }).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    if rf64 {
        h.extend_from_slice(b"ds64");
        h.extend_from_slice(&JUNK_PAYLOAD.to_le_bytes());
        h.extend_from_slice(&riff_size.to_le_bytes());
        h.extend_from_slice(&data_bytes.to_le_bytes());
        h.extend_from_slice(&frames.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // table length
    } else {
        h.extend_from_slice(b"JUNK");
        h.extend_from_slice(&JUNK_PAYLOAD.to_le_bytes());
        h.extend_from_slice(&[0u8; JUNK_PAYLOAD as usize]);
    }
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&fmt_len.to_le_bytes());
    h.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&(bps * 8).to_le_bytes());
    if float {
        h.extend_from_slice(&0u16.to_le_bytes()); // cbSize
        h.extend_from_slice(b"fact");
        h.extend_from_slice(&4u32.to_le_bytes());
        h.extend_from_slice(&(if rf64 || frames > u32::MAX as u64 { u32::MAX } else { frames as u32 }).to_le_bytes());
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(if rf64 { u32::MAX } else { data_bytes as u32 }).to_le_bytes());
    debug_assert_eq!(h.len(), header_len as usize);
    h
}

/// 스테레오/모노 인터리브 f32를 받아 WAV로 쓴다.
pub struct WavWriter {
    f: BufWriter<File>,
    fmt: RecordFormat,
    channels: u16,
    rate: u32,
    header_len: u64,
    data_bytes: u64,
    buf: Vec<u8>,
    last_header: Instant,
}

impl WavWriter {
    pub fn create(path: &Path, fmt: RecordFormat, channels: u16, rate: u32) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut f = BufWriter::with_capacity(1 << 20, file);
        let h = header_bytes(fmt, channels, rate, 0);
        f.write_all(&h)?;
        Ok(Self { f, fmt, channels, rate, header_len: h.len() as u64, data_bytes: 0, buf: Vec::new(), last_header: Instant::now() })
    }

    /// 쓴 오디오 바이트(헤더 제외).
    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    pub fn frames(&self) -> u64 {
        self.data_bytes / (self.channels * self.fmt.bytes_per_sample()) as u64
    }

    /// 인터리브 샘플을 쓴다(길이는 채널 수의 배수).
    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        self.buf.clear();
        match self.fmt {
            RecordFormat::Wav24 => {
                self.buf.reserve(samples.len() * 3);
                for &v in samples {
                    let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
                    let i = (v * 8_388_607.0).round() as i32;
                    self.buf.extend_from_slice(&i.to_le_bytes()[..3]);
                }
            }
            RecordFormat::WavF32 => {
                self.buf.reserve(samples.len() * 4);
                for &v in samples {
                    self.buf.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        self.f.write_all(&self.buf)?;
        self.data_bytes += self.buf.len() as u64;
        if self.last_header.elapsed() >= HEADER_REFRESH {
            self.last_header = Instant::now();
            self.write_header()?;
        }
        Ok(())
    }

    /// 무음 `frames`프레임을 쓴다.
    pub fn write_silence(&mut self, mut frames: u64) -> std::io::Result<()> {
        const Z: [f32; 4096] = [0.0; 4096];
        let ch = self.channels as u64;
        let step = Z.len() as u64 / ch;
        while frames > 0 {
            let n = frames.min(step);
            self.write(&Z[..(n * ch) as usize])?;
            frames -= n;
        }
        Ok(())
    }

    fn write_header(&mut self) -> std::io::Result<()> {
        let h = header_bytes(self.fmt, self.channels, self.rate, self.data_bytes);
        if h.len() as u64 != self.header_len {
            return Err(std::io::Error::other("WAV 헤더 길이 불일치"));
        }
        self.f.flush()?;
        let file = self.f.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&h)?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    /// 헤더 크기를 확정하고 디스크에 내린다.
    pub fn finish(mut self) -> std::io::Result<u64> {
        self.write_header()?;
        self.f.flush()?;
        self.f.get_ref().sync_all()?;
        Ok(self.header_len + self.data_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }
    fn u64_at(b: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
    }

    #[test]
    fn pcm24_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("band-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let mut w = WavWriter::create(&path, RecordFormat::Wav24, 2, 48_000).unwrap();
        w.write(&[0.5, -0.5, 1.5, -1.0]).unwrap();
        w.write_silence(3).unwrap();
        assert_eq!(w.frames(), 5);
        let total = w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len() as u64, total);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32_at(&b, 4) as usize, b.len() - 8);
        assert_eq!(&b[12..16], b"JUNK");
        assert_eq!(&b[48..52], b"fmt ");
        assert_eq!(u32_at(&b, 76), 30); // 5프레임 × 6바이트
        let s0 = i32::from_le_bytes([0, b[80], b[81], b[82]]) >> 8;
        assert_eq!(s0, 4_194_304); // 0.5 × 8388607 반올림
        let s2 = i32::from_le_bytes([0, b[86], b[87], b[88]]) >> 8;
        assert_eq!(s2, 8_388_607); // 1.5는 1로 자름
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn float_header_has_fact() {
        let h = header_bytes(RecordFormat::WavF32, 2, 48_000, 800);
        assert_eq!(&h[48..52], b"fmt ");
        assert_eq!(u32_at(&h, 52), 18);
        assert_eq!(u32_at(&h, 56) & 0xffff, 3);
        assert_eq!(&h[74..78], b"fact");
        assert_eq!(u32_at(&h, 82), 100);
        assert_eq!(&h[86..90], b"data");
        assert_eq!(u32_at(&h, 90), 800);
        assert_eq!(h.len(), 94);
    }

    #[test]
    fn large_data_switches_to_rf64() {
        let data = 5_000_000_000u64; // 4GB 초과
        let h = header_bytes(RecordFormat::Wav24, 2, 48_000, data);
        assert_eq!(&h[0..4], b"RF64");
        assert_eq!(u32_at(&h, 4), u32::MAX);
        assert_eq!(&h[12..16], b"ds64");
        assert_eq!(u32_at(&h, 16), 28);
        assert_eq!(u64_at(&h, 20), h.len() as u64 - 8 + data);
        assert_eq!(u64_at(&h, 28), data);
        assert_eq!(u64_at(&h, 36), data / 6);
        assert_eq!(u32_at(&h, h.len() - 4), u32::MAX);
        // 같은 길이라 제자리에서 바꿀 수 있다
        assert_eq!(h.len(), header_bytes(RecordFormat::Wav24, 2, 48_000, 0).len());
    }
}
