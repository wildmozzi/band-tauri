//! 재생: 시작, 패킷 생산(슬롯 → 디코드·FEC/PLC·PCM 페이드 → 보정 → FIFO), 패킷 버림, 언더런 정산.
//!
//! 디코드·은닉 상태는 `Concealer`(`conceal.rs`), 깊이·보정 상태는 `DepthCtl`(`correction.rs`)에 있다. 여기의
//! `impl JitterBuffer`는 슬롯·seq·FIFO와 두 묶음을 잇는 조립이다(엔진 ⑧(b)).

use super::shape::{ConcealKind, SeqOutcome};
use super::{samples_per_ms, seq_diff, JitterBuffer, State, SLOT_COUNT};

/// 언더런 은닉이 이 시간(ms) 이상 지속되면 재버퍼링.
const STARVE_RESET_MS: f64 = 200.0;
/// 재생 시작 직후 목표에 min(1프레임, 2.5ms)을 더해 두는 시간(µs).
const START_PAD_US: u64 = 1_000_000;
/// 재생 시작 페이드인 길이(샘플, 1ms). 재합류·버림 크로스페이드와 같다.
pub(super) const START_FADE: usize = 48;

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
        if self.slot_has(self.next_seq) && (!self.depth.drop_pending() || self.slot_has(self.next_seq.wrapping_add(1))) {
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
            self.clock_popped += need as f64;
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
        // 꺼낸 양은 소비자 링에 들어가 다음 패킷 머리를 그만큼 뒤로 민다(도착 여유 마감 계산, P2-17h).
        self.clock_popped += need as f64;
    }

    /// 버퍼링 → 재생 전환. 시작 깊이를 목표에 정확히 맞춘다(초과분은 건너뜀).
    pub(super) fn try_start(&mut self, now_us: u64) {
        self.skip_missing_head();
        self.depth.begin_start_pad((self.frame() as f64).min(2.5 * samples_per_ms()), now_us + START_PAD_US);
        self.late.skip_start();
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
        self.stalled = false;
        self.depth.reset_mode();
        self.conceal.reset_run();
        self.debt_deadline = None;
        // 첫 패킷의 앞부분을 잘라 깊이 = 목표. 자르기 전에는 페이드를 걸지 않는다(끝난 상태로 두고 생산).
        self.start_fade_done = START_FADE;
        if self.produce_packet() {
            let cut = (excess.round().max(0.0) as usize).min(self.fifo.len() / 2);
            self.fifo.drain(..cut * 2);
        }
        // 재생 시작(처음·재버퍼·스트림 리셋 뒤): 시작 직전 출력은 언제나 무음이다(Buffering 동안 `pop`이 0을 채운다). 첫 샘플부터
        // 신호면 계단(클릭)이 되므로 자른 뒤 남은 머리부터 `START_FADE` 프레임을 페이드인한다(P2-17d ①). 첫 패킷이 짧게 남았거나
        // 다 잘려 FIFO가 비었으면 나머지는 다음 패킷들에서 이어 건다(`produce_packet`, #196 리뷰 L1). 깊이·타이밍은 그대로다.
        self.start_fade_done = 0;
        self.continue_start_fade(0);
        let d = self.depth_now(now_us);
        self.depth.set_ewma(d);
    }

    /// FIFO의 프레임 `from`부터 시작 페이드인을 이어 건다(남은 프레임이 없으면 아무것도 안 함, 할당 없음).
    pub(super) fn continue_start_fade(&mut self, from: usize) {
        let total = self.fifo.len() / 2;
        let mut f = from;
        while self.start_fade_done < START_FADE && f < total {
            let g = (self.start_fade_done + 1) as f32 / (START_FADE + 1) as f32;
            self.fifo[f * 2] *= g;
            self.fifo[f * 2 + 1] *= g;
            self.start_fade_done += 1;
            f += 1;
        }
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
        (5000.0 * samples_per_ms()) as usize / self.frame()
    }

    fn push_loss(&mut self, lost: bool) {
        if lost && self.state == State::Playing {
            self.late.note_lost();
        }
        let win = self.rate_window();
        self.conceal.push_loss(lost, win);
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
            self.conceal.decode_dropped(&self.slots[idx].data[..len], opus, ch, pframe);
            self.slots[idx].valid = false;
            // 늦게 온 패킷을 버리면(H1 또는 trimming) 앞서 낸 언더런 은닉이 그 자리를 메운 것이다.
            let late = self.conceal.pay_one();
            self.note_consumed(idx, late);
            if late {
                self.note_late();
                self.note_outcome(seq, SeqOutcome::Late, None);
                self.advance_debt_deadline();
            } else {
                self.note_outcome(seq, SeqOutcome::Played, None);
            }
        }
        self.next_seq = seq.wrapping_add(1);
        let f = self.frame() as f64;
        self.depth.sub_ewma(f);
    }

    /// 이중 은닉 제거(P1-6a): 언더런 은닉(seq k 유지)을 낸 뒤 k 없이 뒤 패킷이 도착해 k가 손실로 판정되면, 그
    /// 언더런 은닉이 이미 k의 재생 구간을 메웠다(타임라인 F 소비). 손실 은닉을 한 번 더 만들면 손실 1건에 2프레임이
    /// 나가고 깊이가 +1F 늘어 샘플 버림으로 되돌려야 했다. 그래서 언더런 은닉을 k의 손실 은닉으로 재해석하고 seq만
    /// 넘긴다(새 프레임·깊이 변화 없음). 늦은 패킷을 버리는 H1(`late_dropped`)의 짝이다. k가 나중에 도착하면
    /// `insert`에서 `late`로 세고 지연 통계에는 그대로 반영된다.
    fn settle_underrun_debt(&mut self) {
        while self.state == State::Playing
            && self.conceal.has_debt()
            && !self.slot_has(self.next_seq)
            && self.has_later_packet()
        {
            self.conceal.pay_one();
            self.note_outcome(self.next_seq, SeqOutcome::Missing, self.debt_deadline);
            self.advance_debt_deadline();
            self.next_seq = self.next_seq.wrapping_add(1);
            self.stats.concealed_loss += 1;
            self.push_loss(true);
        }
    }

    /// 빚 1을 갚은 뒤: 다음 빚 seq의 마감은 한 프레임 뒤다(빚이 없으면 지운다).
    fn advance_debt_deadline(&mut self) {
        let f_us = self.frame() as f64 * 1000.0 / samples_per_ms();
        self.debt_deadline = if self.conceal.has_debt() { self.debt_deadline.map(|d| d + f_us) } else { None };
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
                // 지연 계단 감시 중(판정 전 0.5초)에는 깊이를 옛 최소 지연이 아니라 감시 중 최소 지연 기준으로 본다(P2-16e). 옛 기준
                // 깊이는 계단 크기만큼 부풀어(+130ms 계단에서 ≈128ms, 목표 상한 80ms) 늦은 패킷을 버리고 다음 프레임을 은닉하는
                // 교대가 판정까지 이어졌다(실제 쌓인 양은 0). 잠정 바닥 기준이면 늦은 패킷을 재생해 그만큼 쿠션을 쌓는다.
                let shift = self.target.step_floor_shift(self.delay.min_transit());
                let err = self.depth.ewma().map(|e| e - shift + back - self.target_samples()).unwrap_or(0.0);
                if err >= f / 2.0 {
                    self.depth.add_ewma(back); // drop_one_packet이 F를 뺀다: 순증 back − F ≈ 0
                    self.drop_one_packet();
                    self.stats.late_dropped += 1;
                }
            }
        }
        if self.depth.drop_pending() && self.has_later_packet() && self.slot_has(self.next_seq) {
            self.depth.take_drop();
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
            // 디코드(실패면 무음) + 버림 직후 크로스페이드 + 직전 프레임 갱신
            self.conceal.decode_packet(&self.slots[idx].data[..len], opus, ch, pframe);
            frames = pframe;
            self.slots[idx].valid = false;
            self.next_seq = seq.wrapping_add(1);
            self.fifo_end_ts = self.ts_ext(ts) + pframe as i64;
            // 늦게 온 패킷을 재생하면 타임라인이 되감기고(깊이 +debt·F) 그 몫은 샘플 보정이 되돌린다.
            // 그 자리에서 언더런 은닉이 났으면 마감을 놓친 seq다(P2-17h).
            let late = self.conceal.clear_debt();
            self.note_consumed(idx, late);
            self.debt_deadline = None;
            if late {
                self.note_late();
                self.note_outcome(seq, SeqOutcome::Late, None);
            } else {
                self.note_outcome(seq, SeqOutcome::Played, None);
            }
            self.push_loss(false);
            let win = self.rate_window();
            self.conceal.push_conceal(false, win);
        } else {
            let lost = self.has_later_packet();
            // 언더런: seq 전진 없이 은닉 프레임 삽입
            if !lost && self.conceal.starve(f, STARVE_RESET_MS * samples_per_ms()) {
                self.stats.rebuffers += 1;
                let keep_next = self.next_seq;
                self.reset_stream();
                self.has_stream = true;
                self.next_seq = keep_next;
                self.highest = keep_next.wrapping_sub(1);
                return false;
            }
            frames = f;
            // Opus 손실 은닉은 다음 패킷이 있으면 그 FEC로
            let next = seq.wrapping_add(1);
            let ns = &self.slots[Self::slot_index(next)];
            let fec = (lost && ns.valid && ns.seq == next).then(|| &ns.data[..ns.len]);
            let (c, kind) = self.conceal.conceal_frame(frames, fec);
            ch = c;
            self.stats.concealed += 1;
            let sh = &mut self.stats.shape;
            match kind {
                ConcealKind::Fec => sh.conceal_fec += 1,
                ConcealKind::Plc => sh.conceal_plc += 1,
                ConcealKind::Pcm => sh.conceal_pcm += 1,
            }
            let win = self.rate_window();
            self.conceal.push_conceal(true, win);
            if lost {
                self.stats.concealed_loss += 1;
                let d = self.head_deadline();
                self.note_outcome(seq, SeqOutcome::Missing, d);
                self.next_seq = seq.wrapping_add(1);
                self.push_loss(true);
            } else {
                self.stats.shape.conceal_underrun += 1;
                // 빚의 맨 앞 seq는 지금 마감을 놓쳤다(처음 은닉한 시각 = 그 마감). 이어지는 언더런 은닉은 같은 seq를 기다린다.
                if !self.conceal.has_debt() {
                    self.debt_deadline = self.head_deadline();
                }
                self.conceal.add_debt();
            }
            self.fifo_end_ts += frames as i64;
        }
        self.stats.produced += 1;

        let (out_frames, use_shift) = self.correct_rate(frames, ch);
        let src: &[f32] = if use_shift { self.depth.shifted(out_frames * ch) } else { self.conceal.frame_out(out_frames * ch) };
        // 스테레오로 FIFO에 적재. fifo_end_ts는 원래 프레임 끝 그대로 두므로 1샘플 버리면
        // 재생 위치가 1 앞당겨진다(깊이 -1).
        let appended_at = self.fifo.len() / 2;
        if ch == 2 {
            self.fifo.extend(src.iter().copied());
        } else {
            for &s in src {
                self.fifo.push_back(s);
                self.fifo.push_back(s);
            }
        }
        // 재생 시작 페이드인이 아직 남았으면(첫 패킷이 짧게 남았거나 다 잘림) 방금 넣은 프레임에 이어 건다.
        if self.start_fade_done < START_FADE {
            self.continue_start_fade(appended_at);
        }
        true
    }
}
