//! 녹음 FLAC 쓰기(P2-4a): 24비트 무손실, 순수 Rust 인코더 `flacenc`(Apache-2.0)로 블록(4096프레임)마다 프레임을
//! 만들어 이어 쓴다. 비RT(녹음 쓰기 스레드) 전용이다.
//!
//! - 파일 = `fLaC` + STREAMINFO(마지막 메타데이터 블록, 34바이트) + 프레임들. STREAMINFO는 길이가 고정이라
//!   마무리할 때(그리고 쓰는 동안 몇 초마다) 제자리에서 고친다(총 샘플 수·프레임 크기 범위·MD5).
//! - 고정 블록 크기라 마지막 프레임만 짧다. 프레임은 스스로 경계를 가지므로 앱이 죽어도 마지막 헤더 갱신까지는
//!   표준 디코더로 읽힌다.
//! - 쓰기 실패(디스크 가득 등): 인코딩한 바이트는 1MB씩 모아 `write_all`하고, 성공한 곳(프레임 경계)까지를
//!   `committed`로 기억한다. 실패하면 [`FlacWriter::finish_best_effort`]가 파일을 그 경계로 자르고 STREAMINFO의
//!   총 샘플 수를 거기에 맞춘다(MD5는 0 = 모름). WAV의 PR #47 M1과 같은 원칙.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};

/// 블록 크기(프레임). libFLAC 기본(-5)과 같다.
pub const BLOCK_FRAMES: usize = 4096;
const BITS: usize = 24;
/// `fLaC` + 메타데이터 블록 머리(4) + STREAMINFO(34).
pub const FLAC_HEADER_LEN: u64 = 42;
/// 인코딩한 바이트를 이만큼 모아 한 번에 쓴다.
const OUT_BATCH: usize = 1 << 20;
const HEADER_REFRESH: Duration = Duration::from_secs(5);

/// STREAMINFO 포함 파일 머리 42바이트. `frame_size`가 (0, 0)이면 "모름", `md5`가 0이면 "계산 안 함".
pub fn flac_header(rate: u32, channels: u16, block: u16, frame_size: (u32, u32), total: u64, md5: [u8; 16]) -> [u8; 42] {
    let mut h = [0u8; 42];
    h[..4].copy_from_slice(b"fLaC");
    // 마지막 메타데이터 블록(1) + 형식 0(STREAMINFO), 길이 34
    h[4] = 0x80;
    h[5..8].copy_from_slice(&34u32.to_be_bytes()[1..]);
    let s = &mut h[8..];
    s[0..2].copy_from_slice(&block.to_be_bytes());
    s[2..4].copy_from_slice(&block.to_be_bytes());
    s[4..7].copy_from_slice(&frame_size.0.min(0xFF_FFFF).to_be_bytes()[1..]);
    s[7..10].copy_from_slice(&frame_size.1.min(0xFF_FFFF).to_be_bytes()[1..]);
    // 샘플 레이트 20비트 | 채널-1 3비트 | 비트-1 5비트 | 총 샘플 36비트
    let total = total.min((1 << 36) - 1);
    let packed: u64 = (u64::from(rate & 0xF_FFFF) << 44)
        | (u64::from((channels.clamp(1, 8) - 1) as u32) << 41)
        | (((BITS - 1) as u64) << 36)
        | total;
    s[10..18].copy_from_slice(&packed.to_be_bytes());
    s[18..34].copy_from_slice(&md5);
    h
}

pub struct FlacWriter<F: Write + Seek = File> {
    f: F,
    cfg: Verified<flacenc::config::Encoder>,
    info: StreamInfo,
    fb: (FrameBuf, Context),
    channels: u16,
    rate: u32,
    /// 지금 블록에 모은 인터리브 24비트 샘플.
    pending: Vec<i32>,
    /// 인코딩했지만 아직 파일에 안 쓴 바이트(프레임 경계).
    out: Vec<u8>,
    out_frames: u64,
    sink: MemSink<u8>,
    /// 파일에 확실히 들어간 길이(헤더 포함, 프레임 경계)와 그 프레임 수.
    committed_bytes: u64,
    committed_frames: u64,
    /// 인코딩한 프레임 크기 범위(바이트).
    min_frame: u32,
    max_frame: u32,
    last_header: Instant,
}

impl FlacWriter<File> {
    pub fn create(path: &Path, channels: u16, rate: u32) -> std::io::Result<Self> {
        Self::new(File::create(path)?, channels, rate)
    }

    /// 남은 블록을 인코딩해 쓰고 STREAMINFO(총 샘플·MD5)를 확정한 뒤 디스크에 내린다. 실패하면
    /// [`Self::finish_best_effort`]로 닫고 그 오류를 돌려준다. 반환: 파일 크기.
    pub fn finish(mut self) -> std::io::Result<u64> {
        if let Err(e) = self.finish_inner() {
            self.finish_best_effort();
            return Err(e);
        }
        self.f.sync_all()?;
        Ok(self.committed_bytes)
    }

    /// 쓰기 실패 뒤 마무리: 못 쓴 바이트는 버리고 마지막으로 성공한 프레임 경계로 파일을 자른 뒤 STREAMINFO를
    /// 그 길이에 맞춘다(헤더 덮어쓰기는 새 공간이 필요 없다). 반환: 파일 크기.
    pub fn finish_best_effort(self) -> u64 {
        let (file, total) = self.close_lossy();
        if file.set_len(total).is_err() {
            log::warn!("녹음 FLAC 자르기 실패(헤더는 갱신됨)");
        }
        let _ = file.sync_all();
        total
    }
}

fn enc_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("FLAC 인코딩 실패: {e}"))
}

impl<F: Write + Seek> FlacWriter<F> {
    pub fn new(mut f: F, channels: u16, rate: u32) -> std::io::Result<Self> {
        let ch = channels.clamp(1, 8) as usize;
        let cfg = flacenc::config::Encoder::default().into_verified().map_err(|(_, e)| enc_err(e))?;
        let info = StreamInfo::new(rate as usize, ch, BITS).map_err(enc_err)?;
        let fb = (FrameBuf::with_size(ch, BLOCK_FRAMES).map_err(enc_err)?, Context::new(BITS, ch));
        f.write_all(&flac_header(rate, channels, BLOCK_FRAMES as u16, (0, 0), 0, [0; 16]))?;
        Ok(Self {
            f,
            cfg,
            info,
            fb,
            channels: ch as u16,
            rate,
            pending: Vec::with_capacity(BLOCK_FRAMES * ch),
            out: Vec::with_capacity(OUT_BATCH + (1 << 16)),
            out_frames: 0,
            sink: MemSink::new(),
            committed_bytes: FLAC_HEADER_LEN,
            committed_frames: 0,
            min_frame: u32::MAX,
            max_frame: 0,
            last_header: Instant::now(),
        })
    }

    /// 쓴(인코딩해 내보낸) 바이트(헤더 제외). 압축 크기라 녹음 용량 표시에 쓴다.
    pub fn data_bytes(&self) -> u64 {
        self.committed_bytes - FLAC_HEADER_LEN + self.out.len() as u64
    }

    /// 받은 프레임 수(아직 블록에 모으는 중인 것 포함).
    pub fn frames(&self) -> u64 {
        self.committed_frames + self.out_frames + (self.pending.len() / self.channels as usize) as u64
    }

    /// 인터리브 f32 샘플을 쓴다(길이는 채널 수의 배수). ±1 밖은 자르고 24비트로 양자화한다(WAV 24와 같은 값).
    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        let block = BLOCK_FRAMES * self.channels as usize;
        for &v in samples {
            let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
            self.pending.push((v * 8_388_607.0).round() as i32);
            if self.pending.len() == block {
                self.encode_pending()?;
            }
        }
        // 1MB가 모이거나 5초가 지나면 내린다(조용한 트랙은 압축이 커서 1MB가 몇 분 — 앱이 죽어도 5초 전까지는 남게).
        if self.out.len() >= OUT_BATCH || self.last_header.elapsed() >= HEADER_REFRESH {
            self.flush_out()?;
        }
        Ok(())
    }

    /// 무음 `frames`프레임.
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

    fn encode_pending(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let frames = self.pending.len() / self.channels as usize;
        self.fb.fill_interleaved(&self.pending).map_err(enc_err)?;
        let num = self.fb.1.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(&self.cfg, &self.fb.0, num, &self.info).map_err(enc_err)?;
        self.sink.clear();
        frame.write(&mut self.sink).map_err(enc_err)?;
        let bytes = self.sink.as_slice();
        self.out.extend_from_slice(bytes);
        let len = bytes.len() as u32;
        self.min_frame = self.min_frame.min(len);
        self.max_frame = self.max_frame.max(len);
        self.out_frames += frames as u64;
        self.pending.clear();
        Ok(())
    }

    /// 모은 프레임을 파일에 쓴다. 성공하면 그 끝이 새 경계다.
    fn flush_out(&mut self) -> std::io::Result<()> {
        if !self.out.is_empty() {
            self.f.write_all(&self.out)?;
            self.committed_bytes += self.out.len() as u64;
            self.committed_frames += self.out_frames;
            self.out.clear();
            self.out_frames = 0;
        }
        if self.last_header.elapsed() >= HEADER_REFRESH {
            self.last_header = Instant::now();
            self.write_header(self.committed_frames, self.frame_range(), [0; 16])?;
        }
        Ok(())
    }

    fn frame_range(&self) -> (u32, u32) {
        if self.max_frame == 0 {
            (0, 0)
        } else {
            (self.min_frame, self.max_frame)
        }
    }

    fn write_header(&mut self, total: u64, range: (u32, u32), md5: [u8; 16]) -> std::io::Result<()> {
        let h = flac_header(self.rate, self.channels, BLOCK_FRAMES as u16, range, total, md5);
        self.f.flush()?;
        self.f.seek(SeekFrom::Start(0))?;
        self.f.write_all(&h)?;
        self.f.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn finish_inner(&mut self) -> std::io::Result<()> {
        self.encode_pending()?;
        self.flush_out()?;
        let md5 = self.fb.1.md5_digest();
        let range = self.frame_range();
        self.write_header(self.committed_frames, range, md5)?;
        self.f.flush()
    }

    /// 못 쓴 바이트를 버리고 마지막 경계까지로 헤더를 고친다(MD5 0, 프레임 크기 범위 모름). 반환: (내부 파일, 길이).
    fn close_lossy(mut self) -> (F, u64) {
        let h = flac_header(self.rate, self.channels, BLOCK_FRAMES as u16, (0, 0), self.committed_frames, [0; 16]);
        if self.f.seek(SeekFrom::Start(0)).and_then(|_| self.f.write_all(&h)).and_then(|_| self.f.flush()).is_err() {
            log::warn!("녹음 FLAC 헤더 갱신 실패");
        }
        (self.f, self.committed_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn decode(bytes: &[u8]) -> (claxon::metadata::StreamInfo, Vec<i32>) {
        let mut r = claxon::FlacReader::new(Cursor::new(bytes)).expect("표준 디코더(claxon)로 열린다");
        let info = r.streaminfo();
        let s: Vec<i32> = r.samples().map(|x| x.expect("샘플")).collect();
        (info, s)
    }

    fn signal(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / 48_000.0;
                [(t * 440.0 * std::f32::consts::TAU).sin() * 0.5, (t * 660.0 * std::f32::consts::TAU).sin() * 0.25]
            })
            .collect()
    }

    fn q24(v: f32) -> i32 {
        (v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
    }

    /// 표준 디코더로 열고 샘플이 24비트 양자화 값과 정확히 같다(무손실). 마지막 블록은 짧다.
    #[test]
    fn roundtrip_matches_pcm24() {
        let dir = std::env::temp_dir().join(format!("band-flac-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.flac");
        let frames = BLOCK_FRAMES * 3 + 1234;
        let sig = signal(frames);
        let mut w = FlacWriter::create(&path, 2, 48_000).unwrap();
        for c in sig.chunks(480) {
            w.write(c).unwrap();
        }
        w.write(&[1.5, f32::NAN]).unwrap(); // 자름·NaN → 0
        w.write_silence(10).unwrap();
        let total_frames = frames as u64 + 11;
        assert_eq!(w.frames(), total_frames);
        let size = w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len() as u64, size);
        let (info, s) = decode(&b);
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.samples, Some(total_frames));
        assert_eq!(info.max_block_size, BLOCK_FRAMES as u16);
        assert!(info.min_frame_size.is_some() && info.max_frame_size.is_some());
        assert_ne!(info.md5sum, [0; 16]);
        assert_eq!(s.len() as u64, total_frames * 2);
        let mut want: Vec<i32> = sig.iter().map(|&v| q24(v)).collect();
        want.extend([8_388_607, 0]);
        want.extend([0; 20]);
        assert_eq!(s, want);
        // 사인파는 WAV 24보다 확실히 작다
        assert!(size < total_frames * 6 * 3 / 4, "{size}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 인코딩 속도(실시간 배수). 녹음은 트랙 최대 9개(소스 8 + 믹스)를 쓰기 스레드 하나로 인코딩한다.
    /// `cargo test --release --lib flac_encode_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn flac_encode_speed() {
        let secs = 30usize;
        let mut x = 1u32;
        let sig: Vec<f32> = signal(48_000 * secs)
            .into_iter()
            .map(|v| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                v + ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.01
            })
            .collect();
        let t0 = Instant::now();
        let mut w = FlacWriter::new(Cursor::new(Vec::new()), 2, 48_000).unwrap();
        for c in sig.chunks(960) {
            w.write(c).unwrap();
        }
        w.finish_inner().unwrap();
        let dt = t0.elapsed().as_secs_f64();
        let size = w.f.into_inner().len();
        println!("FLAC {secs}s 스테레오 24비트: {dt:.3}s = 실시간 {:.0}배, {:.1}% of WAV24", secs as f64 / dt, size as f64 * 100.0 / (48_000 * secs * 6) as f64);
    }

    #[test]
    fn empty_file_is_valid() {
        let mut w = FlacWriter::new(Cursor::new(Vec::new()), 2, 44_100).unwrap();
        w.finish_inner().unwrap();
        let (b, len) = (w.f.into_inner(), FLAC_HEADER_LEN);
        assert_eq!(b.len() as u64, len);
        let (info, s) = decode(&b);
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.samples.unwrap_or(0), 0, "0 = 모름(claxon은 None)");
        assert!(s.is_empty());
    }

    /// 용량 `cap` 바이트까지만 쓸 수 있는 가짜 디스크(wav 테스트와 같음).
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

    /// 디스크가 가득 차도 마지막으로 성공한 프레임 경계까지 잘라 표준 디코더로 열리고, 총 샘플 수가 그와 같다.
    #[test]
    fn disk_full_truncates_to_frame_boundary() {
        // 잡음(압축이 잘 안 됨)을 써서 1MB 묶음이 두어 번 나가게 한다
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..48_000 * 2)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let cap = 1_500_000 + 7; // 묶음 하나는 들어가고 두 번째에서 가득 참
        let mut w = FlacWriter::new(FullDisk { data: Vec::new(), pos: 0, cap }, 2, 48_000).unwrap();
        let mut failed = false;
        let mut fed = Vec::new();
        'outer: for _ in 0..20 {
            for c in noise.chunks(960) {
                if w.write(c).is_err() {
                    failed = true;
                    break 'outer;
                }
                fed.extend_from_slice(c);
            }
        }
        assert!(failed, "용량을 넘었는데 쓰기가 성공함");
        let committed = w.committed_frames;
        assert!(committed > 0 && committed.is_multiple_of(BLOCK_FRAMES as u64), "{committed}");
        let (disk, len) = w.close_lossy();
        assert!(len as usize <= cap);
        let (info, s) = decode(&disk.data[..len as usize]);
        assert_eq!(info.samples, Some(committed));
        assert_eq!(s.len() as u64, committed * 2);
        let want: Vec<i32> = fed[..s.len()].iter().map(|&v| q24(v)).collect();
        assert_eq!(s, want);
    }
}
