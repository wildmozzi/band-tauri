//! 깊이 측정(`end_burst`)과 데드밴드 보정: 비율 상한 리샘플러(`RateShifter`), 샘플 시프트 유틸.

use super::{Correction, JitterBuffer, State};
use crate::engine::config::INTERNAL_RATE;

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

impl JitterBuffer {
    /// 보정 데드밴드(샘플): (버림 진입, 삽입 진입, 버림 해제, 삽입 해제). 삽입 쪽(깊이 부족 = 은닉 위험)은 좁게,
    /// 버림 쪽(깊이 초과 = 지연)은 넓게 둔다. 삽입 진입 문턱은 목표에 포함된다(`target_samples`).
    pub(super) fn deadbands(&self) -> (f64, f64, f64, f64) {
        let f = self.frame() as f64;
        let spm = Self::samples_per_ms();
        let enter_drop = (f / 2.0).clamp(spm, 2.5 * spm);
        let enter_ins = (f / 8.0).clamp(0.25 * spm, spm);
        (enter_drop, enter_ins, (enter_drop / 4.0).max(4.0), (enter_ins / 4.0).max(4.0))
    }

    /// 소비자가 재생 위치를 `samples`만큼 옮겼음을 알린다(깊이 추정에 바로 반영). 워커가 언더런 뒤 건너뛰기를
    /// 늘어난 목표만큼 덜 하면 깊이가 그만큼 즉시 커진다. EWMA가 이를 천천히 따라가는 동안 오차를 잘못 읽어
    /// 삽입 보정이 끼지 않게 한다.
    pub fn shift_depth(&mut self, samples: f64) {
        if let Some(e) = self.depth_ewma.as_mut() {
            *e += samples;
        }
    }

    /// 현재 재생 위치 기준 깊이(샘플). 재생 위치(소스 링 머리)의 ts = 다음에 꺼낼 샘플의 ts − 이미
    /// 꺼냈지만 재생 전인 양(`ahead`). 링에 있는 샘플이 더 오래되었으므로 깊이는 그만큼 크다.
    pub(super) fn depth_now(&self, now_us: u64) -> f64 {
        let next_ts = self.fifo_end_ts - (self.fifo.len() / 2) as i64;
        (Self::now_samples(now_us) - next_ts - self.min_transit) as f64 + self.ahead
    }

    /// 다음 seq의 시작 타임스탬프 기준 깊이(버퍼링 중 판단용). 소스 링에 이전 스트림이 남아 있으면
    /// 그만큼 늦게 재생되므로 깊이에 더한다.
    pub(super) fn depth_if_start(&self, now_us: u64) -> Option<f64> {
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        if !(slot.valid && slot.seq == self.next_seq) {
            return None;
        }
        Some((Self::now_samples(now_us) - self.ts_ext(slot.ts) - self.min_transit) as f64 + self.ahead)
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
        self.ahead = ahead.max(0.0);
        match self.state {
            State::Buffering => self.try_start(now_us),
            State::Playing => {
                if self.start_pad > 0.0 && now_us >= self.start_pad_until_us {
                    self.start_pad = 0.0;
                    self.after_target_change();
                }
                let dw = self.depth_now(wall_us);
                if wall_us.saturating_sub(self.wall_bucket_start) >= WALL_BUCKET_US {
                    self.wall_prev_min = self.wall_cur_min;
                    self.wall_cur_min = f64::INFINITY;
                    self.wall_bucket_start = wall_us;
                }
                self.wall_cur_min = self.wall_cur_min.min(dw);
                let d = self.depth_now(now_us);
                let e = match self.depth_ewma {
                    None => d,
                    Some(e) => e + (d - e) * DEPTH_ALPHA,
                };
                self.depth_ewma = Some(e);
                self.update_correction(e);
            }
        }
    }

    fn update_correction(&mut self, ewma: f64) {
        let f = self.frame() as f64;
        let target = self.target_samples();
        let err = ewma - target;
        // 데드밴드(비대칭, P1-5): 버림 진입 F/2를 1ms~2.5ms로 제한(10ms 프레임에서 목표 +5ms까지 머물지 않게, PR #5 L-1),
        // 삽입 진입은 F/8을 0.25~1ms로 제한하고 목표에 포함한다(깊이가 목표 − 삽입 진입까지 내려가도 지터 추정 + lead 이상).
        let (enter_drop, enter_ins, exit_drop, exit_ins) = self.deadbands();
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
        let queued_excess = self.queued_samples() - target;
        if !self.trimming && err >= 3.0 * f && queued_excess >= 3.0 * f {
            self.trimming = true;
        } else if self.trimming && (err < f || queued_excess < f) {
            self.trimming = false;
        }
        self.drop_frames_pending = usize::from(self.trimming);
    }

    /// 샘플 단위 보정(H1): 방금 생산한 `dec_buf`의 프레임을 보정 모드에 따라 리샘플한다. 반환: (출력 프레임 수, `shift_buf`를 쓸지).
    pub(super) fn correct_rate(&mut self, frames: usize, ch: usize) -> (usize, bool) {
        let win = self.rate_window();
        self.corr_ring.push(self.correction != Correction::Idle, win);
        // 샘플 단위 보정
        let mut out_frames = frames;
        let mut use_shift = false;
        {
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
                out_frames = self.shifter.process(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, rstep);
                use_shift = true;
                let d = frames as f64 - out_frames as f64;
                if d > 0.0 {
                    self.stats.samples_dropped += d as u64;
                } else {
                    self.stats.samples_inserted += (-d) as u64;
                }
                if let Some(e) = self.depth_ewma.as_mut() {
                    *e -= d;
                }
                let cents = (1200.0 * rstep.log2()).abs() as f32;
                self.max_corr_cents = self.max_corr_cents.max(cents);
            } else {
                for c in 0..ch {
                    self.shifter.prev[c] = self.dec_buf[(frames - 1) * ch + c];
                }
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
    pub fn depth_error(&self) -> f64 {
        match (self.state, self.depth_ewma) {
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
