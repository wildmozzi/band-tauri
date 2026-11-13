//! 재생: 시작, 패킷 생산(디코드·FEC/PLC·PCM 페이드), 패킷 버림, 언더런 정산.

use super::late_budget::LATE_SKIP_BLOCKS;
use super::{seq_diff, Correction, JitterBuffer, State, SLOT_COUNT};
use crate::engine::codec::Decoder;

/// 언더런 은닉이 이 시간(ms) 이상 지속되면 재버퍼링.
const STARVE_RESET_MS: f64 = 200.0;
/// 재생 시작 직후 목표에 min(1프레임, 2.5ms)을 더해 두는 시간(µs).
const START_PAD_US: u64 = 1_000_000;
/// 패킷 단위 버림 시 크로스페이드 길이(샘플).
const DROP_XFADE: usize = 48;

impl JitterBuffer {
    /// 은닉 없이(실제 받은 데이터로만) 지금 꺼낼 수 있는 프레임 수(최소 보장치). 콜백 구동 믹스에서 워커는
    /// 이만큼만 미리 꺼내고, 은닉은 출력 콜백 마감 직전에만 한다(늦게 오는 패킷을 기다림).
    /// FIFO에 남은 패킷 꼬리도 포함한다: 청크 단위로만 꺼내면 꼬리(< 청크)가 남아 마감에 은닉이 끼었다.
    pub fn real_frames(&self) -> usize {
        if self.state != State::Playing {
            return 0;
        }
        let fifo = self.fifo.len() / 2;
        // 다음 패킷이 있으면 패킷 하나(보정으로 최대 2샘플 줄어듦)를 더 꺼낼 수 있다. 패킷 단위 버림이
        // 걸려 있으면 그다음 패킷까지 있어야 은닉이 끼지 않는다.
        if self.slot_has(self.next_seq) && (self.drop_frames_pending == 0 || self.slot_has(self.next_seq.wrapping_add(1))) {
            fifo + self.slots[Self::slot_index(self.next_seq)].frame.saturating_sub(2)
        } else {
            fifo
        }
    }

    /// 스테레오 인터리브로 `out.len() / 2` 프레임을 채운다. 버퍼링 중이면 무음.
    pub fn pop(&mut self, out: &mut [f32]) {
        let need = out.len() / 2;
        if self.state == State::Buffering {
            out.fill(0.0);
            return;
        }
        while self.fifo.len() / 2 < need {
            if !self.produce_packet() {
                break;
            }
        }
        let avail = (self.fifo.len() / 2).min(need);
        for (o, s) in out[..avail * 2].iter_mut().zip(self.fifo.drain(..avail * 2)) {
            *o = s;
        }
        out[avail * 2..].fill(0.0);
    }

    /// 버퍼링 → 재생 전환. 시작 깊이를 목표에 정확히 맞춘다(초과분은 건너뜀).
    pub(super) fn try_start(&mut self, now_us: u64) {
        self.skip_missing_head();
        self.start_pad = (self.frame() as f64).min(2.5 * Self::samples_per_ms());
        self.start_pad_until_us = now_us + START_PAD_US;
        self.tail_skip = LATE_SKIP_BLOCKS;
        let target = self.target_samples();
        let Some(depth) = self.depth_if_start(now_us) else {
            return;
        };
        if depth < target {
            return;
        }
        let f = self.frame();
        let mut excess = depth - target;
        // 통째로 건너뛸 수 있는 패킷(뒤 패킷이 이미 있는 경우만)
        while excess >= f as f64 && self.slot_has(self.next_seq.wrapping_add(1)) {
            let idx = Self::slot_index(self.next_seq);
            self.slots[idx].valid = false;
            self.next_seq = self.next_seq.wrapping_add(1);
            excess -= f as f64;
        }
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        self.fifo_end_ts = self.ts_ext(slot.ts);
        self.fifo.clear();
        self.state = State::Playing;
        self.correction = Correction::Idle;
        self.drop_frames_pending = 0;
        self.trimming = false;
        self.conceal_run = 0;
        self.starve_samples = 0;
        self.underrun_debt = 0;
        // 첫 패킷의 앞부분을 잘라 깊이 = 목표
        if self.produce_packet() {
            let cut = (excess.round().max(0.0) as usize).min(self.fifo.len() / 2);
            self.fifo.drain(..cut * 2);
        }
        self.depth_ewma = Some(self.depth_now(now_us));
    }

    /// 버퍼링 중 시작 seq가 없고 뒤 패킷이 있으면 처음 존재하는 seq로 시작점을 옮긴다.
    fn skip_missing_head(&mut self) {
        if !self.has_stream || self.slot_has(self.next_seq) {
            return;
        }
        let span = seq_diff(self.highest, self.next_seq);
        if span <= 0 {
            return;
        }
        for k in 1..=(span as u32).min(SLOT_COUNT as u32) {
            let s = self.next_seq.wrapping_add(k);
            if self.slot_has(s) {
                self.next_seq = s;
                return;
            }
        }
    }

    /// 5초 창 크기(패킷).
    pub(super) fn rate_window(&self) -> usize {
        (5000.0 * Self::samples_per_ms()) as usize / self.frame()
    }

    fn push_loss(&mut self, lost: bool) {
        if lost && self.state == State::Playing {
            self.lost_block = self.lost_block.saturating_add(1);
        }
        let win = self.rate_window();
        self.loss.push(lost, win);
    }

    fn ensure_decoder(&mut self, opus: bool, ch: usize) {
        if !self.decoder.as_ref().is_some_and(|d| d.matches(opus, ch)) {
            // 처음·재시작 뒤·코덱 변경: 풀과 주고받는다(새로 만든 것과 같은 상태, 워커에서 할당·해제 없음 — 엔진 ③c).
            if let Some(old) = self.decoder.take() {
                self.decoders.give(old);
            }
            self.decoder = self.decoders.take(opus, ch);
        }
    }

    /// 패킷 단위 버림: 디코더 상태 유지를 위해 디코드하고, 다음 프레임과 크로스페이드할
    /// 앞부분을 보관한다.
    fn drop_one_packet(&mut self) {
        let seq = self.next_seq;
        let idx = Self::slot_index(seq);
        if self.slot_has(seq) {
            let (opus, stereo, len, pframe) = {
                let s = &self.slots[idx];
                (s.flags & 1 != 0, s.flags & 2 != 0, s.len, s.frame)
            };
            let ch = if stereo { 2 } else { 1 };
            self.ensure_decoder(opus, ch);
            let n = pframe * ch;
            let ok = match self.decoder.as_mut() {
                Some(d) => d.decode(&self.slots[idx].data[..len], &mut self.xfade_buf[..n]).unwrap_or(0) == pframe,
                None => false,
            };
            if ok {
                self.xfade_len = DROP_XFADE.min(pframe);
                self.xfade_ch = ch;
            }
            self.slots[idx].valid = false;
            // 늦게 온 패킷을 버리면(H1 또는 trimming) 앞서 낸 언더런 은닉이 그 자리를 메운 것이다.
            if self.underrun_debt > 0 {
                self.underrun_debt -= 1;
                self.note_late();
            }
        }
        self.next_seq = seq.wrapping_add(1);
        let f = self.frame() as f64;
        if let Some(e) = self.depth_ewma.as_mut() {
            *e -= f;
        }
    }

    /// 이중 은닉 제거(P1-6a): 언더런 은닉(seq k 유지)을 낸 뒤 k 없이 뒤 패킷이 도착해 k가 손실로 판정되면, 그
    /// 언더런 은닉이 이미 k의 재생 구간을 메웠다(타임라인 F 소비). 손실 은닉을 한 번 더 만들면 손실 1건에 2프레임이
    /// 나가고 깊이가 +1F 늘어 샘플 버림으로 되돌려야 했다. 그래서 언더런 은닉을 k의 손실 은닉으로 재해석하고 seq만
    /// 넘긴다(새 프레임·깊이 변화 없음). 늦은 패킷을 버리는 H1(`late_dropped`)의 짝이다. k가 나중에 도착하면
    /// `insert`에서 `late`로 세고 지연 통계에는 그대로 반영된다.
    fn settle_underrun_debt(&mut self) {
        while self.state == State::Playing
            && self.underrun_debt > 0
            && !self.slot_has(self.next_seq)
            && self.has_later_packet()
        {
            self.underrun_debt -= 1;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.stats.concealed_loss += 1;
            self.push_loss(true);
        }
    }

    /// 패킷 하나만큼 FIFO에 생산. 재버퍼링으로 전환하면 false.
    fn produce_packet(&mut self) -> bool {
        self.settle_underrun_debt();
        // 늦은 패킷을 손실로(H1, NetEQ류): 언더런 은닉이 이미 그 자리를 채워 깊이가 늘었고 지금 목표보다 F/2 이상 깊으면,
        // 늦게 온 패킷을 재생하지 않고 버린다(디코드해 디코더 상태 유지 + 크로스페이드). 은닉은 이미 났으므로 은닉률은
        // 그대로이고, 늘어난 1F를 샘플 버림 램프로 되돌리는 구간(피치 편차)이 없어진다.
        // 깊이는 늦은 패킷을 꺼내는 순간(ts가 은닉한 만큼 되감길 때) 한 번에 늘어나므로 그 되감김 `back`으로 판단한다.
        if self.state == State::Playing && self.slot_has(self.next_seq) && self.slot_has(self.next_seq.wrapping_add(1)) {
            let f = self.frame() as f64;
            let back = (self.fifo_end_ts - self.ts_ext(self.slots[Self::slot_index(self.next_seq)].ts)) as f64;
            if back >= f / 2.0 {
                let err = self.depth_ewma.map(|e| e + back - self.target_samples()).unwrap_or(0.0);
                if err >= f / 2.0 {
                    if let Some(e) = self.depth_ewma.as_mut() {
                        *e += back; // drop_one_packet이 F를 뺀다: 순증 back − F ≈ 0
                    }
                    self.drop_one_packet();
                    self.stats.late_dropped += 1;
                }
            }
        }
        if self.drop_frames_pending > 0 && self.has_later_packet() && self.slot_has(self.next_seq) {
            self.drop_frames_pending -= 1;
            self.drop_one_packet();
            self.stats.dropped_frames += 1;
        }
        // 버림(H1·trimming) 뒤 다음 seq가 비어 있고 debt가 남았으면 그 빈 자리도 이미 언더런 은닉이 메웠다
        // (PR #58 L1: 맨 앞에서만 정산하면 드물게 손실 은닉 1프레임 + 깊이 +1F가 더 생겼다).
        self.settle_underrun_debt();

        let f = self.frame();
        let seq = self.next_seq;
        let idx = Self::slot_index(seq);
        let ch;
        let frames;

        if self.slot_has(seq) {
            let (opus, stereo, len, pframe, ts) = {
                let s = &self.slots[idx];
                (s.flags & 1 != 0, s.flags & 2 != 0, s.len, s.frame, s.ts)
            };
            ch = if stereo { 2 } else { 1 };
            self.ensure_decoder(opus, ch);
            let out = &mut self.dec_buf[..pframe * ch];
            let decoded = match self.decoder.as_mut() {
                Some(d) => d.decode(&self.slots[idx].data[..len], out).unwrap_or(0),
                None => 0,
            };
            if decoded != pframe {
                out.fill(0.0);
            }
            // 패킷 버림 직후: 버린 프레임 앞부분 → 새 프레임으로 크로스페이드
            if self.xfade_len > 0 && self.xfade_ch == ch {
                let l = self.xfade_len;
                for i in 0..l {
                    let w = (i + 1) as f32 / (l + 1) as f32;
                    for c in 0..ch {
                        let k = i * ch + c;
                        out[k] = self.xfade_buf[k] * (1.0 - w) + out[k] * w;
                    }
                }
            }
            self.xfade_len = 0;
            frames = pframe;
            self.slots[idx].valid = false;
            self.next_seq = seq.wrapping_add(1);
            self.fifo_end_ts = self.ts_ext(ts) + pframe as i64;
            self.last_frame[..frames * ch].copy_from_slice(&self.dec_buf[..frames * ch]);
            self.last_frame_valid = true;
            self.pkt_channels = ch;
            self.conceal_run = 0;
            self.starve_samples = 0;
            // 늦게 온 패킷을 재생하면 타임라인이 되감기고(깊이 +debt·F) 그 몫은 샘플 보정이 되돌린다.
            if self.underrun_debt > 0 {
                self.underrun_debt = 0;
                self.note_late();
            }
            self.push_loss(false);
            let win = self.rate_window();
            self.conceal.push(false, win);
        } else {
            let lost = self.has_later_packet();
            if !lost {
                // 언더런: seq 전진 없이 은닉 프레임 삽입
                self.starve_samples += f;
                if self.starve_samples as f64 > STARVE_RESET_MS * Self::samples_per_ms() {
                    self.stats.rebuffers += 1;
                    let keep_next = self.next_seq;
                    self.reset_stream();
                    self.has_stream = true;
                    self.next_seq = keep_next;
                    self.highest = keep_next.wrapping_sub(1);
                    return false;
                }
            }
            ch = self.pkt_channels;
            frames = f;
            let out = &mut self.dec_buf[..frames * ch];
            let mut ok = false;
            if let Some(dec) = self.decoder.as_mut() {
                if matches!(dec, Decoder::Opus { .. }) {
                    let next = seq.wrapping_add(1);
                    let ns = &self.slots[Self::slot_index(next)];
                    let r = if lost && ns.valid && ns.seq == next {
                        dec.decode_fec(&ns.data[..ns.len], out)
                    } else {
                        dec.decode_plc(out)
                    };
                    ok = r.map(|n| n == frames).unwrap_or(false);
                }
            }
            if !ok {
                // PCM(또는 디코더 없음): 직전 프레임 페이드아웃 1회, 이후 무음
                if self.conceal_run == 0 && self.last_frame_valid {
                    let lf = &self.last_frame[..frames * ch];
                    for (i, (o, s)) in out.iter_mut().zip(lf).enumerate() {
                        let g = 1.0 - (i / ch) as f32 / frames as f32;
                        *o = s * g;
                    }
                } else {
                    out.fill(0.0);
                }
            }
            self.xfade_len = 0;
            self.conceal_run += 1;
            self.stats.concealed += 1;
            let win = self.rate_window();
            self.conceal.push(true, win);
            if lost {
                self.stats.concealed_loss += 1;
                self.next_seq = seq.wrapping_add(1);
                self.push_loss(true);
            } else {
                self.underrun_debt += 1;
            }
            self.fifo_end_ts += frames as i64;
        }
        self.stats.produced += 1;

        let (out_frames, use_shift) = self.correct_rate(frames, ch);
        let src: &[f32] = if use_shift { &self.shift_buf[..out_frames * ch] } else { &self.dec_buf[..out_frames * ch] };
        // 스테레오로 FIFO에 적재. fifo_end_ts는 원래 프레임 끝 그대로 두므로 1샘플 버리면
        // 재생 위치가 1 앞당겨진다(깊이 -1).
        if ch == 2 {
            self.fifo.extend(src.iter().copied());
        } else {
            for &s in src {
                self.fifo.push_back(s);
                self.fifo.push_back(s);
            }
        }
        true
    }
}
