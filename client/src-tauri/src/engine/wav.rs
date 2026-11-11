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

/// 스테레오/모노 인터리브 f32를 받아 WAV로 쓴다. 는 테스트에서 실패 주입용으로 바꾼다.
pub struct WavWriter<F: Write + Seek = File> {
    f: BufWriter<F>,
    fmt: RecordFormat,
    channels: u16,
    rate: u32,
    header_len: u64,
    data_bytes: u64,
    buf: Vec<u8>,
    last_header: Instant,
}

impl WavWriter<File> {
    pub fn create(path: &Path, fmt: RecordFormat, channels: u16, rate: u32) -> std::io::Result<Self> {
        Self::new(File::create(path)?, fmt, channels, rate)
    }

    /// 헤더 크기를 확정하고 디스크에 내린다. 실패하면(디스크 가득 등) [`Self::finish_best_effort`]로 닫고 그 오류를 돌려준다.
    pub fn finish(mut self) -> std::io::Result<u64> {
        if let Err(e) = self.write_header().and_then(|_| self.f.flush()) {
            self.finish_best_effort();
            return Err(e);
        }
        self.f.get_ref().sync_all()?;
        Ok(self.header_len + self.data_bytes)
    }

    /// 쓰기 실패 뒤 마무리: 버퍼에 남은(못 쓴) 데이터는 버리고, 디스크에 실제로 있는 프레임 수로 헤더를 고친 뒤
    /// 파일을 거기까지 자른다. 헤더 덮어쓰기는 새 공간이 필요 없어 디스크가 가득 차도 된다. 반환: 파일 크기.
    pub fn finish_best_effort(self) -> u64 {
        let (file, total) = self.close_lossy();
        if file.set_len(total).is_err() {
            log::warn!("녹음 파일 자르기 실패(헤더는 갱신됨)");
        }
        let _ = file.sync_all();
        total
    }
}

impl<F: Write + Seek> WavWriter<F> {
    pub fn new(inner: F, fmt: RecordFormat, channels: u16, rate: u32) -> std::io::Result<Self> {
        let mut f = BufWriter::with_capacity(1 << 20, inner);
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

    /// 버퍼를 비우지 않고(`into_parts`: drop 때 다시 flush하지 않음) 실제 파일 끝에서 프레임 단위로 내린 data 크기로
    /// 헤더를 쓴다. 반환: (내부 파일, 헤더 + 그 data 크기).
    fn close_lossy(self) -> (F, u64) {
        let (mut inner, _unwritten) = self.f.into_parts();
        let block = (self.channels * self.fmt.bytes_per_sample()).max(1) as u64;
        let len = inner.seek(SeekFrom::End(0)).unwrap_or(self.header_len);
        let data = len.saturating_sub(self.header_len) / block * block;
        let h = header_bytes(self.fmt, self.channels, self.rate, data);
        if inner.seek(SeekFrom::Start(0)).and_then(|_| inner.write_all(&h)).and_then(|_| inner.flush()).is_err() {
            log::warn!("녹음 파일 헤더 갱신 실패");
        }
        (inner, self.header_len + data)
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

    /// 용량 `cap` 바이트까지만 쓸 수 있는 가짜 디스크(넘으면 받을 수 있는 만큼 쓰고 StorageFull).
    struct FullDisk {
        data: Vec<u8>,
        pos: usize,
        cap: usize,
    }

    impl Write for FullDisk {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let room = self.cap.saturating_sub(self.pos);
            if room == 0 && !b.is_empty() {
                return Err(std::io::ErrorKind::StorageFull.into());
            }
            let n = b.len().min(room);
            let end = self.pos + n;
            if self.data.len() < end {
                self.data.resize(end, 0);
            }
            self.data[self.pos..end].copy_from_slice(&b[..n]);
            self.pos = end;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Seek for FullDisk {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.pos = match p {
                SeekFrom::Start(o) => o as usize,
                SeekFrom::End(o) => (self.data.len() as i64 + o) as usize,
                SeekFrom::Current(o) => (self.pos as i64 + o) as usize,
            };
            Ok(self.pos as u64)
        }
    }

    /// PR #47 M1: 디스크가 가득 차 쓰기가 실패해도 헤더 크기는 디스크에 실제로 있는 프레임(프레임 경계로 내림)과 같다.
    #[test]
    fn header_matches_disk_after_write_failure() {
        let header_len = header_bytes(RecordFormat::Wav24, 2, 48_000, 0).len();
        let cap = header_len + 1_500_001; // 프레임(6바이트) 경계가 아닌 곳에서 가득 참
        let mut w = WavWriter::new(FullDisk { data: Vec::new(), pos: 0, cap }, RecordFormat::Wav24, 2, 48_000).unwrap();
        let chunk = vec![0.25f32; 9600];
        let mut failed = false;
        for _ in 0..200 {
            if w.write(&chunk).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "용량을 넘었는데 쓰기가 성공함");
        assert!(w.data_bytes() > 1_500_001, "버퍼에만 있는 바이트까지 센다(그래서 헤더를 이 값으로 쓰면 안 됨)");
        let (disk, total) = w.close_lossy();
        let data = (1_500_001 / 6 * 6) as u32;
        assert_eq!(total, (header_len as u32 + data) as u64);
        let b = &disk.data;
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32_at(b, 4), header_len as u32 - 8 + data);
        assert_eq!(&b[header_len - 8..header_len - 4], b"data");
        assert_eq!(u32_at(b, header_len - 4), data);
        // 실제 샘플이 들어 있다(0.25 → 2097152)
        let s0 = i32::from_le_bytes([0, b[header_len], b[header_len + 1], b[header_len + 2]]) >> 8;
        assert_eq!(s0, 2_097_152);
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
