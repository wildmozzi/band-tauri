//! 캡처 단계(엔진 ②b): 입력 링 → (리샘플) → 48k 블록 → 인코더용 프레임.
//!
//! 조정자가 한 패스에 `fill` → `next_block`(없을 때까지, 블록마다 로컬 경로로) → `next_frame`(없을 때까지, 프레임마다 송신으로)
//! 순으로 부른다. 블록·프레임은 빌려 준다(복사 없음).
//! 바뀌는 이유: 입력 장치 형식·리샘플·프레임 크기. 모르는 것: net, 코덱, 로컬 FIFO.

use std::collections::VecDeque;

use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

use crate::engine::config::INTERNAL_RATE;
use crate::engine::resample::{StreamResampler, CHUNK as RESAMPLE_CHUNK};

pub(super) struct Capture {
    /// 내부 채널 수.
    ich: usize,
    in_cons: HeapCons<f32>,
    in_rs: StreamResampler,
    in_tmp: Vec<f32>,
    /// 48k 입력 블록(리샘플 직후, 내부 채널).
    in48_buf: Vec<f32>,
    /// 인코더용 프레임 누적.
    cap_acc: VecDeque<f32>,
    frame_buf: Vec<f32>,
}

/// `acc`에 프레임 하나(`frame.len()` 샘플)가 차 있으면 꺼내 `frame`에 담는다.
fn pop_frame(acc: &mut VecDeque<f32>, frame: &mut [f32]) -> bool {
    let n = frame.len();
    if n == 0 || acc.len() < n {
        return false;
    }
    for (d, s) in frame.iter_mut().zip(acc.drain(..n)) {
        *d = s;
    }
    true
}

/// `acc`에서 프레임을 가능한 만큼 꺼내 `f(프레임, 시작 시각)`에 넘긴다. 한 패스에서 나온 프레임은 모두 같은 `pass_at`을
/// 받는다(리뷰 M1: 프레임마다 `now()`를 찍으면 앞 프레임 뒤에서 기다린 시간이 빠진다). 조정자의 프레임 루프
/// (`Worker::run`: `pass_at`을 한 번 찍고 `next_frame`마다 같은 값을 넘긴다)와 같은 모양 — 시험의 기준이다.
#[cfg(test)]
pub(super) fn drain_frames(acc: &mut VecDeque<f32>, frame: &mut [f32], pass_at: std::time::Instant, mut f: impl FnMut(&[f32], std::time::Instant)) -> usize {
    let mut count = 0;
    while pop_frame(acc, frame) {
        f(frame, pass_at);
        count += 1;
    }
    count
}

impl Capture {
    /// `frame` = 프레임 크기(48k 프레임 수).
    pub(super) fn new(in_cons: HeapCons<f32>, in_rate: u32, ich: usize, frame: usize) -> Result<Self, String> {
        Ok(Self {
            ich,
            in_cons,
            in_rs: StreamResampler::new(in_rate, INTERNAL_RATE, ich)?,
            in_tmp: vec![0.0; 4096],
            in48_buf: vec![0.0; 2048 * ich],
            cap_acc: VecDeque::with_capacity((frame + 4096) * ich),
            frame_buf: vec![0.0; frame * ich],
        })
    }

    /// 입력 링을 끝까지 비워 리샘플러에 넣는다.
    pub(super) fn fill(&mut self) {
        loop {
            let n = self.in_cons.pop_slice(&mut self.in_tmp);
            if n == 0 {
                break;
            }
            let n = n - n % self.ich;
            self.in_rs.push(&self.in_tmp[..n]);
        }
    }

    /// 다음 48k 블록(내부 채널 인터리브). 리샘플 직후 모니터/탭으로 바로 보낼 수 있게 프레임 누적 전에 내준다(PR #5 M-5).
    /// `muted`(입력 뮤트)면 0으로 채운다: 송신/모니터/탭 모두 무음. 패킷은 계속 보내 수신측 타이밍을 유지한다.
    /// 내준 블록은 이미 프레임 누적에 더해져 있다.
    pub(super) fn next_block(&mut self, muted: bool) -> Option<&[f32]> {
        let frames = self.in_rs.pop(&mut self.in48_buf);
        if frames == 0 {
            return None;
        }
        let block = &mut self.in48_buf[..frames * self.ich];
        if muted {
            block.fill(0.0);
        }
        self.cap_acc.extend(block.iter().copied());
        Some(block)
    }

    /// 완성된 다음 프레임. 프레임 미만 나머지는 다음 패스로 남는다.
    pub(super) fn next_frame(&mut self) -> Option<&[f32]> {
        pop_frame(&mut self.cap_acc, &mut self.frame_buf).then_some(&self.frame_buf[..])
    }

    /// 리샘플러에 남은 48k 프레임(모니터 경로 점유 통계의 입력 쪽 몫).
    pub(super) fn pending48(&self) -> usize {
        self.in_rs.available()
    }

    /// 입력 방출 단위(48k 프레임): 패스스루면 1, 아니면 리샘플러 청크. 생성 뒤 바뀌지 않는다.
    pub(super) fn granularity(&self) -> usize {
        if self.in_rs.is_passthrough() {
            1
        } else {
            RESAMPLE_CHUNK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rt::input_ring;
    use ringbuf::traits::Producer;

    /// 알려진 입력 링 → 블록·프레임 열이 `drain_frames`(전의 `pump_input`이 쓰던 것)와 같다(설계 5.4절).
    /// 48k 패스스루라 블록 = 넣은 샘플 그대로다.
    #[test]
    fn blocks_and_frames_match_reference() {
        for ich in [1usize, 2] {
            let frame = 120;
            let (mut prod, cons) = input_ring();
            let mut cap = Capture::new(cons, 48_000, ich, frame).unwrap();
            assert_eq!(cap.granularity(), 1);
            let mut reference: VecDeque<f32> = VecDeque::new();
            let mut ref_frame = vec![0.0; frame * ich];
            let (mut got, mut want) = (Vec::new(), Vec::new());
            let mut next = 0u32;
            // 패스마다 다른 크기(프레임 경계와 어긋나게), 세 번째 패스는 뮤트
            for (pass, frames_in) in [128usize, 7, 300, 0, 480, 64].into_iter().enumerate() {
                let muted = pass == 2;
                let input: Vec<f32> = (0..frames_in * ich).map(|_| { next += 1; next as f32 }).collect();
                assert_eq!(prod.push_slice(&input), input.len());
                cap.fill();
                let mut blocks = Vec::new();
                while let Some(b) = cap.next_block(muted) {
                    blocks.extend_from_slice(b);
                }
                let expect: Vec<f32> = if muted { vec![0.0; input.len()] } else { input };
                assert_eq!(blocks, expect, "패스 {pass}: 블록을 이으면 넣은 입력(뮤트면 0)");
                while let Some(f) = cap.next_frame() {
                    got.push(f.to_vec());
                }
                reference.extend(expect);
                drain_frames(&mut reference, &mut ref_frame, std::time::Instant::now(), |f, _| want.push(f.to_vec()));
                assert_eq!(cap.pending48(), 0);
            }
            assert_eq!(got, want, "ich {ich}");
            assert_eq!(got.len(), (128 + 7 + 300 + 480 + 64) / frame);
        }
    }

    /// 리샘플이 필요한 입력(44.1k)은 방출 단위가 리샘플러 청크다(로컬 FIFO 설정에 넘기는 값).
    #[test]
    fn resampled_input_reports_chunk_granularity() {
        let (_prod, cons) = input_ring();
        let cap = Capture::new(cons, 44_100, 1, 120).unwrap();
        assert_eq!(cap.granularity(), RESAMPLE_CHUNK);
    }
}
