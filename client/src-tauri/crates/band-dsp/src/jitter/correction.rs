//! 깊이 측정(`end_burst`)과 데드밴드 보정: 상태는 `DepthCtl`(엔진 ⑧(b)), 비율 상한 리샘플러(`RateShifter`), 샘플 시프트 유틸.

use super::stats::RateRing;
use super::{samples_per_ms, JitterBuffer, State, LOSS_WINDOW_MAX, MAX_PKT_FRAME};
use crate::config::INTERNAL_RATE;

/// 깊이 EWMA 계수(버스트당 1회 측정).
const DEPTH_ALPHA: f64 = 0.05;
/// 샘플 단위 보정(버림·삽입)의 최대 속도(비율, PR #20 H1). 0.25% = 1200·log2(1.0025) ≈ 4.3센트: 지속음 피치 JND
/// (약 5–10센트) 아래다. 드리프트(±200ppm = 0.02%)는 이 속도의 1/12로 충분히 덮는다. 이전에는 패킷마다 1–2샘플을
/// 그 패킷 안에서 보정해 2.5ms 프레임에서 14–29센트였다. 더 큰 오차는 패킷 단위 버림(트리밍)과 늦은 패킷 손실 처리가 맡는다.
pub const MAX_CORR_RATE: f64 = 0.0025;

/// 샘플 보정으로 깊이를 `samples`(48k)만큼 옮기는 데 걸리는 이론 시간(초): `samples / (MAX_CORR_RATE × 48000)`.
/// 테스트가 목표 변경 뒤 수렴 시간 상한과 보정 제외 창을 이 값으로 계산한다(엔진 사양을 그대로 따름).
pub fn correction_catch_up_secs(samples: f64) -> f64 {
    samples.abs() / (MAX_CORR_RATE * INTERNAL_RATE as f64)
}

/// 프레임 `frame`(샘플)에서 보정 해제 문턱(ms): (버림 해제, 삽입 해제). 수렴 판정 기준(테스트용).
pub fn correction_exit_ms(frame: usize) -> (f64, f64) {
    let f = frame as f64;
    let spm = INTERNAL_RATE as f64 / 1000.0;
    let enter_drop = (f / 2.0).clamp(spm, 2.5 * spm);
    let enter_ins = (f / 8.0).clamp(0.25 * spm, spm);
    ((enter_drop / 4.0).max(4.0) / spm, (enter_ins / 4.0).max(4.0) / spm)
}
/// 보정이 끝날 때 읽기 위상을 정수로 되돌리는 허용 오차(샘플). 이보다 가까우면 바로 맞춘다.
const PHASE_SNAP: f64 = 0.1;

/// 패킷 경계를 넘어 이어지는 분수 읽기 위치 리샘플러(선형 보간). 보정 중에는 읽기 간격 `step` = 1 ± MAX_CORR_RATE로
/// 출력 길이가 패킷마다 조금씩 달라진다. 보정이 끝나도 위상이 정수가 될 때까지(`PHASE_SNAP`) 같은 방향으로 이어 간 뒤
/// 멈춰, 평상시에는 보간(고역 감쇠) 없이 원 샘플을 그대로 낸다.
#[derive(Default)]
pub(super) struct RateShifter {
    /// 다음 출력 샘플의 읽기 위치(현재 패킷 시작 기준, −1 이상: −1..0은 직전 패킷 마지막 샘플과의 사이).
    pos: f64,
    /// 직전 패킷의 마지막 입력 샘플(채널별).
    prev: [f32; 2],
    /// 마지막으로 쓴 읽기 간격(위상 정리용).
    last_step: f64,
    /// 위상 정리 중 경과 패킷 수.
    settle: u32,
}

impl RateShifter {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// 위상이 0이면 원 샘플을 그대로 쓴다(복사 없음).
    fn idle(&self) -> bool {
        self.pos == 0.0
    }

    /// `src`(frames×ch)를 읽기 간격 `step`으로 리샘플해 `dst`에 쓰고 출력 프레임 수를 돌려준다.
    pub(super) fn process(&mut self, src: &[f32], ch: usize, frames: usize, dst: &mut [f32], step: f64) -> usize {
        let last = (frames - 1) as f64;
        let cap = dst.len() / ch;
        let mut n = 0;
        let mut pos = self.pos;
        while pos <= last && n < cap {
            let i0 = pos.floor();
            let w = (pos - i0) as f32;
            let i0 = i0 as i64;
            for c in 0..ch {
                let a = if i0 < 0 { self.prev[c] } else { src[i0 as usize * ch + c] };
                let b = if w == 0.0 { a } else { src[(i0 + 1) as usize * ch + c] };
                dst[n * ch + c] = a + (b - a) * w;
            }
            n += 1;
            pos += step;
        }
        self.pos = pos - frames as f64;
        for c in 0..ch {
            self.prev[c] = src[(frames - 1) * ch + c];
        }
        self.last_step = step;
        n
    }
}
/// 벽시계 깊이 최솟값 버킷 길이(µs). 보고값은 직전+현재 버킷의 최솟값(0.5~1초).
const WALL_BUCKET_US: u64 = 500_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Correction {
    Idle,
    Drop,
    Insert,
}

/// `DepthCtl`의 힙 버퍼 묶음(`Buffers`의 일부). 재시작은 `DepthCtl::reclaim`으로 지금 것을 되돌려 다시 쓴다(할당·해제 없음).
pub(super) struct DepthBufs {
    shift_buf: Vec<f32>,
    corr_ring: RateRing,
}

impl DepthBufs {
    pub(super) fn alloc() -> Self {
        Self { shift_buf: vec![0.0; (MAX_PKT_FRAME + 4) * 2], corr_ring: RateRing::new(LOSS_WINDOW_MAX) }
    }
}

/// 깊이 측정·보정 상태(엔진 ⑧(b)): 깊이 EWMA, 보정 모드(데드밴드·`chase`), 패킷 단위 버림(trimming), 비율 리샘플러,
/// 벽시계 깊이 최솟값, 재생 시작 여유. 메서드는 필요한 값(측정 깊이, 오차·문턱, 창 크기, 입력 프레임)을 인자로 받고
/// `JitterBuffer`를 모른다.
pub(super) struct DepthCtl {
    depth_ewma: Option<f64>,
    /// 이미 꺼냈지만 아직 재생되지 않은 샘플 수(48k). 콜백 구동 믹스에서 소스 링·리샘플러에 있는 양.
    /// 깊이는 실제 재생 위치(소스 링의 머리)를 기준으로 잰다.
    ahead: f64,
    correction: Correction,
    drop_frames_pending: usize,
    trimming: bool,
    /// 목표가 내려가거나 소비자 요구가 바뀐 뒤 해제 문턱까지 보정 중(데드밴드 무시). 목표 쪽(`TargetEst`의
    /// `note_target`·`set_consumer_lead` 반환값)이 `set_chase`로 켜고 `update`가 해제 문턱 안에서 끈다(⑧(a)에서 정함).
    chase: bool,
    shifter: RateShifter,
    /// 리샘플러 출력 스크래치.
    shift_buf: Vec<f32>,
    /// 보정 모드였던 패킷 비율 창(H1 통계).
    corr_ring: RateRing,
    max_corr_cents: f32,
    wall_bucket_start: u64,
    wall_cur_min: f64,
    wall_prev_min: f64,
    /// 재생 시작 직후 여유(샘플)와 그 끝 시각(µs, 장치 클럭). 출력 콜백 시각 추정(`DeviceClock`)이 이른 콜백을
    /// 아직 못 본 첫 콜백들에서 소스 링이 모자라지 않게 한다. 끝나면 `chase`로 목표까지 내려간다.
    start_pad: f64,
    start_pad_until_us: u64,
}

impl DepthCtl {
    pub(super) fn new(b: DepthBufs) -> Self {
        Self {
            depth_ewma: None,
            ahead: 0.0,
            correction: Correction::Idle,
            drop_frames_pending: 0,
            trimming: false,
            chase: false,
            shifter: RateShifter::default(),
            shift_buf: b.shift_buf,
            corr_ring: b.corr_ring,
            max_corr_cents: 0.0,
            wall_bucket_start: 0,
            wall_cur_min: f64::INFINITY,
            wall_prev_min: f64::INFINITY,
            start_pad: 0.0,
            start_pad_until_us: 0,
        }
    }

    /// 재시작: 버퍼를 꺼내 `DepthBufs::alloc()`과 같은 내용으로 되돌린다(할당·해제 없음).
    pub(super) fn reclaim(&mut self) -> DepthBufs {
        let mut shift_buf = std::mem::take(&mut self.shift_buf);
        shift_buf.fill(0.0);
        DepthBufs { shift_buf, corr_ring: RateRing::take_reset(&mut self.corr_ring) }
    }

    /// 스트림 리셋(재버퍼·먼 미래 seq): 리샘플러·벽시계 최솟값·깊이 EWMA·보정 모드·패킷 버림을 버린다.
    pub(super) fn reset_stream(&mut self) {
        self.shifter.reset();
        self.wall_cur_min = f64::INFINITY;
        self.wall_prev_min = f64::INFINITY;
        self.depth_ewma = None;
        self.reset_mode();
    }

    /// 보정 모드·패킷 버림만 대기로(재생 시작).
    pub(super) fn reset_mode(&mut self) {
        self.correction = Correction::Idle;
        self.drop_frames_pending = 0;
        self.trimming = false;
    }

    pub(super) fn ewma(&self) -> Option<f64> {
        self.depth_ewma
    }

    pub(super) fn set_ewma(&mut self, d: f64) {
        self.depth_ewma = Some(d);
    }

    /// 깊이 EWMA에 `x`를 더한다(측정 전이면 그대로).
    pub(super) fn add_ewma(&mut self, x: f64) {
        if let Some(e) = self.depth_ewma.as_mut() {
            *e += x;
        }
    }

    /// 깊이 EWMA에서 `x`를 뺀다(측정 전이면 그대로).
    pub(super) fn sub_ewma(&mut self, x: f64) {
        if let Some(e) = self.depth_ewma.as_mut() {
            *e -= x;
        }
    }

    /// 측정 깊이 `d`로 EWMA를 갱신하고 새 값을 돌려준다.
    pub(super) fn measure(&mut self, d: f64) -> f64 {
        let e = match self.depth_ewma {
            None => d,
            Some(e) => e + (d - e) * DEPTH_ALPHA,
        };
        self.depth_ewma = Some(e);
        e
    }

    pub(super) fn ahead(&self) -> f64 {
        self.ahead
    }

    pub(super) fn set_ahead(&mut self, ahead: f64) {
        self.ahead = ahead.max(0.0);
    }

    /// 벽시계 기준 깊이 `dw`(벽시계 시각 `wall_us`)를 0.5초 버킷 최솟값에 넣는다.
    pub(super) fn note_wall(&mut self, dw: f64, wall_us: u64) {
        if wall_us.saturating_sub(self.wall_bucket_start) >= WALL_BUCKET_US {
            self.wall_prev_min = self.wall_cur_min;
            self.wall_cur_min = f64::INFINITY;
            self.wall_bucket_start = wall_us;
        }
        self.wall_cur_min = self.wall_cur_min.min(dw);
    }

    /// 직전+현재 버킷의 벽시계 깊이 최솟값(없으면 무한대).
    pub(super) fn wall_min(&self) -> f64 {
        self.wall_cur_min.min(self.wall_prev_min)
    }

    pub(super) fn start_pad(&self) -> f64 {
        self.start_pad
    }

    /// 재생 시작 여유 `pad`(샘플)를 `until_us`까지 둔다.
    pub(super) fn begin_start_pad(&mut self, pad: f64, until_us: u64) {
        self.start_pad = pad;
        self.start_pad_until_us = until_us;
    }

    /// 재생 시작 여유가 끝났으면 걷고 `true`(목표가 바뀐다 — 호출한 쪽이 `after_target_change`).
    pub(super) fn expire_start_pad(&mut self, now_us: u64) -> bool {
        if self.start_pad > 0.0 && now_us >= self.start_pad_until_us {
            self.start_pad = 0.0;
            return true;
        }
        false
    }

    pub(super) fn set_chase(&mut self) {
        self.chase = true;
    }

    /// 보정 판단(버스트당 1회): 오차 `err` = 깊이 EWMA − 목표, 데드밴드 `bands` = (버림 진입, 삽입 진입, 버림 해제,
    /// 삽입 해제), 프레임 `f`, 실제 쌓인 양의 목표 초과분 `queued_excess`.
    pub(super) fn update(&mut self, err: f64, bands: (f64, f64, f64, f64), f: f64, queued_excess: f64) {
        // 데드밴드(비대칭, P1-5): 버림 진입 F/2를 1ms~2.5ms로 제한(10ms 프레임에서 목표 +5ms까지 머물지 않게, PR #5 L-1),
        // 삽입 진입은 F/8을 0.25~1ms로 제한하고 목표에 포함한다(깊이가 목표 − 삽입 진입까지 내려가도 지터 추정 + lead 이상).
        let (enter_drop, enter_ins, exit_drop, exit_ins) = bands;
        // 소비자 요구로 목표가 바뀐 직후에는 해제 문턱 밖의 오차부터 따라간다(P1-13, `set_consumer_lead`).
        // 목표가 내려간 직후(`after_target_change`)에도 같은 방식으로 버림 데드밴드 안에 지연을 남기지 않는다(P1-5).
        if self.chase && ((err >= 0.0 && err <= exit_drop) || (err < 0.0 && -err <= exit_ins)) {
            self.chase = false;
        }
        let (enter_drop, enter_ins) = if self.chase { (exit_drop, exit_ins) } else { (enter_drop, enter_ins) };
        self.correction = match self.correction {
            Correction::Idle if err > enter_drop => Correction::Drop,
            Correction::Idle if err < -enter_ins => Correction::Insert,
            Correction::Drop if err <= exit_drop => Correction::Idle,
            Correction::Insert if err >= -exit_ins => Correction::Idle,
            c => c,
        };
        // 패킷 단위 버림(히스테리시스): 추정 깊이와 실제 쌓인 양이 모두 목표를 3F 이상 넘으면
        // 시작하고, 둘 중 하나가 목표 + F 안으로 들어오면 멈춘다. 버스트당 최대 1패킷.
        if !self.trimming && err >= 3.0 * f && queued_excess >= 3.0 * f {
            self.trimming = true;
        } else if self.trimming && (err < f || queued_excess < f) {
            self.trimming = false;
        }
        self.drop_frames_pending = usize::from(self.trimming);
    }

    /// 패킷 단위 버림이 걸려 있나.
    pub(super) fn drop_pending(&self) -> bool {
        self.drop_frames_pending > 0
    }

    /// 걸린 패킷 버림 하나를 쓴다.
    pub(super) fn take_drop(&mut self) {
        self.drop_frames_pending -= 1;
    }

    /// 샘플 단위 보정(H1): 방금 생산한 프레임 `src`(frames×ch)를 보정 모드에 따라 리샘플한다(창 크기 `win`패킷).
    /// 반환: (출력 프레임 수, `shifted`를 쓸지, 줄어든 샘플 수 d(늘었으면 음수)).
    pub(super) fn correct_rate(&mut self, src: &[f32], frames: usize, ch: usize, win: usize) -> (usize, bool, f64) {
        self.corr_ring.push(self.correction != Correction::Idle, win);
        // 비율 상한 리샘플러(H1): 버림 1 + r, 삽입 1 − r. 보정이 끝나면 위상이 정수가 될 때까지 같은 방향으로 이어 간다.
        let rstep = match self.correction {
            Correction::Drop => 1.0 + MAX_CORR_RATE,
            Correction::Insert => 1.0 - MAX_CORR_RATE,
            Correction::Idle => {
                let fr = self.shifter.pos - self.shifter.pos.round();
                // 정수 위상(−1 또는 0)이면 보간 없이 원 샘플을 그대로 낸다(−1은 1샘플 지연된 무손실 상태).
                if fr.abs() < PHASE_SNAP || self.shifter.settle > 64 {
                    self.shifter.pos = self.shifter.pos.round();
                    self.shifter.settle = 0;
                    1.0
                } else {
                    self.shifter.settle += 1;
                    self.shifter.last_step
                }
            }
        };
        if rstep != 1.0 || !self.shifter.idle() {
            if self.correction != Correction::Idle {
                self.shifter.settle = 0;
            }
            let out_frames = self.shifter.process(src, ch, frames, &mut self.shift_buf, rstep);
            let d = frames as f64 - out_frames as f64;
            if let Some(e) = self.depth_ewma.as_mut() {
                *e -= d;
            }
            let cents = (1200.0 * rstep.log2()).abs() as f32;
            self.max_corr_cents = self.max_corr_cents.max(cents);
            (out_frames, true, d)
        } else {
            for c in 0..ch {
                self.shifter.prev[c] = src[(frames - 1) * ch + c];
            }
            (frames, false, 0.0)
        }
    }

    /// 리샘플러 출력(`n`샘플).
    pub(super) fn shifted(&self, n: usize) -> &[f32] {
        &self.shift_buf[..n]
    }

    pub(super) fn corr_pct(&self) -> f32 {
        self.corr_ring.pct()
    }

    pub(super) fn max_corr_cents(&self) -> f32 {
        self.max_corr_cents
    }

    /// 시험: 보정 비율 창이 `new` 직후와 같은가.
    #[cfg(test)]
    pub(super) fn is_fresh(&self) -> bool {
        self.corr_ring.is_fresh()
    }

    /// 시험: 패킷 단위 버림 하나를 건다.
    #[cfg(test)]
    pub(super) fn force_drop(&mut self) {
        self.drop_frames_pending = 1;
    }
}

impl JitterBuffer {
    /// 보정 데드밴드(샘플): (버림 진입, 삽입 진입, 버림 해제, 삽입 해제). 삽입 쪽(깊이 부족 = 은닉 위험)은 좁게,
    /// 버림 쪽(깊이 초과 = 지연)은 넓게 둔다. 삽입 진입 문턱은 목표에 포함된다(`target_samples`).
    pub(super) fn deadbands(&self) -> (f64, f64, f64, f64) {
        let f = self.frame() as f64;
        let spm = samples_per_ms();
        let enter_drop = (f / 2.0).clamp(spm, 2.5 * spm);
        let enter_ins = (f / 8.0).clamp(0.25 * spm, spm);
        (enter_drop, enter_ins, (enter_drop / 4.0).max(4.0), (enter_ins / 4.0).max(4.0))
    }

    /// 소비자가 재생 위치를 `samples`만큼 옮겼음을 알린다(깊이 추정에 바로 반영). 워커가 언더런 뒤 건너뛰기를
    /// 늘어난 목표만큼 덜 하면 깊이가 그만큼 즉시 커진다. EWMA가 이를 천천히 따라가는 동안 오차를 잘못 읽어
    /// 삽입 보정이 끼지 않게 한다.
    pub fn shift_depth(&mut self, samples: f64) {
        self.depth.add_ewma(samples);
    }

    /// 현재 재생 위치 기준 깊이(샘플). 재생 위치(소스 링 머리)의 ts = 다음에 꺼낼 샘플의 ts − 이미
    /// 꺼냈지만 재생 전인 양(`ahead`). 링에 있는 샘플이 더 오래되었으므로 깊이는 그만큼 크다.
    pub(super) fn depth_now(&self, now_us: u64) -> f64 {
        let next_ts = self.fifo_end_ts - (self.fifo.len() / 2) as i64;
        (Self::now_samples(now_us) - next_ts - self.delay.min_transit()) as f64 + self.depth.ahead()
    }

    /// 다음 seq의 시작 타임스탬프 기준 깊이(버퍼링 중 판단용). 소스 링에 이전 스트림이 남아 있으면
    /// 그만큼 늦게 재생되므로 깊이에 더한다.
    pub(super) fn depth_if_start(&self, now_us: u64) -> Option<f64> {
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        if !(slot.valid && slot.seq == self.next_seq) {
            return None;
        }
        Some((Self::now_samples(now_us) - self.ts_ext(slot.ts) - self.delay.min_transit()) as f64 + self.depth.ahead())
    }

    /// 출력 버스트 종료. `now_us`는 출력 콜백 시각 추정치(장치 클럭 기준).
    /// `wall_us`는 워커가 실제로 깨어난 벽시계 시각이다. 이것으로 잰 깊이는 기상 지연만큼 크게 나오므로,
    /// 1초 창의 최솟값이 장치 클럭 추정과 **독립적인** 실제 재생 지연이 된다(PR #5 H-1: 클럭 추정이
    /// 틀려 지연이 몰래 늘어나는 경우를 통계로 드러낸다).
    ///
    /// `ahead`: 이미 꺼냈지만 아직 재생되지 않은 샘플 수(48k 환산). 콜백 구동 믹스에서는 소스 링과
    /// 출력 리샘플러에 있는 양이다. 깊이는 실제 재생 위치를 기준으로 재므로 이것을 뺀다. 그래서
    /// `bufferMs`는 소스 링까지 포함한 수신측 버퍼 지연이다.
    pub fn end_burst(&mut self, now_us: u64, wall_us: u64, ahead: f64) {
        self.depth.set_ahead(ahead);
        match self.state {
            State::Buffering => self.try_start(now_us),
            State::Playing => {
                if self.depth.expire_start_pad(now_us) {
                    self.after_target_change();
                }
                let dw = self.depth_now(wall_us);
                self.depth.note_wall(dw, wall_us);
                let d = self.depth_now(now_us);
                let e = self.depth.measure(d);
                self.update_correction(e);
            }
        }
    }

    fn update_correction(&mut self, ewma: f64) {
        let f = self.frame() as f64;
        let target = self.target_samples();
        let err = ewma - target;
        let bands = self.deadbands();
        let queued_excess = self.queued_samples() - target;
        self.depth.update(err, bands, f, queued_excess);
    }

    /// 샘플 단위 보정(H1): 방금 생산한 프레임(`Concealer`의 출력)을 보정 모드에 따라 리샘플한다.
    /// 반환: (출력 프레임 수, `DepthCtl::shifted`를 쓸지).
    pub(super) fn correct_rate(&mut self, frames: usize, ch: usize) -> (usize, bool) {
        let win = self.rate_window();
        let (out_frames, use_shift, d) = self.depth.correct_rate(self.conceal.frame_out(frames * ch), frames, ch, win);
        if use_shift {
            if d > 0.0 {
                self.stats.samples_dropped += d as u64;
            } else {
                self.stats.samples_inserted += (-d) as u64;
            }
        }
        (out_frames, use_shift)
    }

    /// 깊이 부족분(샘플): 정상 범위(목표 − 삽입 진입 문턱, 이미 목표에 포함)보다 얕은 만큼. 재생 중이 아니면 0.
    /// 워커의 유효 여유 계산용(PR #20 L4: 정상 데드밴드까지 부족분으로 세면 유효 여유가 늘 조금 깎였다).
    pub fn depth_shortfall(&self) -> f64 {
        let (_, enter_ins, _, _) = self.deadbands();
        (-(self.depth_error() + enter_ins)).max(0.0)
    }

    /// 깊이 오차(샘플, 깊이 EWMA − 목표). 재생 중이 아니면 0.
    pub(crate) fn depth_error(&self) -> f64 {
        match (self.state, self.depth.ewma()) {
            (State::Playing, Some(e)) => e - self.target_samples(),
            _ => 0.0,
        }
    }
}

/// 1샘플 버림(`drop=true`) 또는 삽입. 읽기 위치를 프레임 전체에 걸쳐 선형으로 ±1 시프트한다
/// (선형 보간). 국소 피치 편차가 1/frames로 작다(240샘플이면 약 7센트, 리뷰 M2).
/// `dst`는 (frames±1)×ch 이상이어야 한다. `frames > 2`.
pub fn shift_one(src: &[f32], ch: usize, frames: usize, dst: &mut [f32], drop: bool) {
    shift_n(src, ch, frames, dst, if drop { -1 } else { 1 });
}

/// `delta`샘플만큼 늘이거나(+) 줄인다(−). 출력 길이 = frames + delta. 읽기 위치를 프레임 전체에 걸쳐
/// 선형으로 |delta|만큼 시프트한다. 국소 피치 편차는 |delta|/frames. `frames > |delta| + 1`.
pub fn shift_n(src: &[f32], ch: usize, frames: usize, dst: &mut [f32], delta: i32) {
    let n = delta.unsigned_abs() as usize;
    let drop = delta < 0;
    let out_frames = if drop { frames - n } else { frames + n };
    // 마지막 출력 샘플이 마지막 입력 샘플에 정확히 닿도록 램프 길이를 잡는다.
    let l = if drop { (frames - n - 1) as f32 } else { (frames + n - 1) as f32 };
    let last = (frames - 1) as f32;
    let nf = n as f32;
    for j in 0..out_frames {
        let r = (j as f32 / l).clamp(0.0, 1.0) * nf;
        let pos = if drop { j as f32 + r } else { j as f32 - r }.clamp(0.0, last);
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(frames - 1);
        let w = pos - i0 as f32;
        for c in 0..ch {
            dst[j * ch + c] = src[i0 * ch + c] * (1.0 - w) + src[i1 * ch + c] * w;
        }
    }
}
