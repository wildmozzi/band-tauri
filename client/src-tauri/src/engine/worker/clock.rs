//! 출력 콜백 시각 추정: 재생 프레임 카운터 → 콜백 시각(`DeviceClock`), 반복마다의 공급 타이밍(`FeedTiming`).

use std::sync::atomic::Ordering;

use super::super::config::INTERNAL_RATE;
use super::super::stats::AudioCounters;
use super::OUT_CHUNK;

// ============================================================================
// 장치 클럭 추정
// ============================================================================

/// 출력 콜백 시각 추정기.
///
/// 입력은 출력 콜백이 장치에 내보낸 **누적 프레임 수**(`out_played_frames`, 언더런으로 0을 채운 부분 포함)다.
/// 워커가 본 `wall − played/rate`는 콜백 직후에 관측할수록 작으므로, 최근 `WINDOW` 동안의 최솟값이
/// 실제 콜백 시각에 가장 가깝다. 최솟값을 고정 창(0.5초 버킷 4개)에서 구하므로
/// - 워커 기상 지연(값이 커지는 쪽)은 무시되고,
/// - 장치 정지·절전 복귀·긴 xrun처럼 오프셋이 위로 한 번 뛰는 불연속은 짧은 창(`CLOCK_SHORT_US`)의 최솟값이
///   긴 창보다 `CLOCK_GAP_US` 넘게 크면 공백으로 보고 창을 그 값으로 리셋해 0.15~0.3초 안에 따라잡고(PR #34 M3),
/// - 장치-벽시계 드리프트는 창 길이만큼의 바이어스(500ppm에서 1ms 이하)로 따라간다.
///
/// 이전 구현은 소비 샘플 수(`out_consumed`)와 전역 최솟값 + 500ppm 누설을 써서, 출력 언더런마다
/// 결손이 0.5ms/s로만 회복되어 실제 지연이 조용히 쌓였다(PR #5 리뷰 H-1).
pub struct DeviceClock {
    rate: f64,
    bucket_start_us: f64,
    cur_min: f64,
    prev_mins: [f64; CLOCK_BUCKETS - 1],
    started: bool,
    short_start_us: f64,
    short_min: f64,
    /// 공백 감지로 창을 리셋한 횟수(진단·테스트용).
    pub resets: u64,
}

const CLOCK_BUCKET_US: f64 = 500_000.0;
const CLOCK_BUCKETS: usize = 4;
/// 공백 감지용 짧은 창. 콜백이 여러 번 들어올 만큼 길고(10ms 주기에서 15번), 재수렴이 빠를 만큼 짧다.
const CLOCK_SHORT_US: f64 = 150_000.0;
/// 짧은 창 최솟값이 긴 창보다 이만큼 크면 콜백 공백(장치 일시 중단·스트림 재시작)으로 본다.
/// 워커 기상 지연은 짧은 창 안의 다른 관측이 최솟값을 끌어내려 걸리지 않는다. 드리프트(500ppm × 2초 = 1ms)보다 크다.
const CLOCK_GAP_US: f64 = 3_000.0;

impl DeviceClock {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(1) as f64,
            bucket_start_us: 0.0,
            cur_min: f64::INFINITY,
            prev_mins: [f64::INFINITY; CLOCK_BUCKETS - 1],
            started: false,
            short_start_us: 0.0,
            short_min: f64::INFINITY,
            resets: 0,
        }
    }

    /// `played_frames`: 출력 콜백이 지금까지 장치에 내보낸 프레임 수. 반환: 마지막 콜백 시각 추정(µs).
    pub fn update(&mut self, wall_us: u64, played_frames: u64) -> u64 {
        let wall = wall_us as f64;
        if played_frames == 0 {
            return wall_us;
        }
        if !self.started {
            self.started = true;
            self.bucket_start_us = wall;
            self.short_start_us = wall;
        }
        if wall - self.bucket_start_us >= CLOCK_BUCKET_US {
            // 버킷 회전(오래 쉬었으면 여러 칸)
            let steps = (((wall - self.bucket_start_us) / CLOCK_BUCKET_US) as usize).min(CLOCK_BUCKETS);
            for _ in 0..steps {
                self.prev_mins.rotate_right(1);
                self.prev_mins[0] = self.cur_min;
                self.cur_min = f64::INFINITY;
            }
            self.bucket_start_us += steps as f64 * CLOCK_BUCKET_US;
        }
        let dev_t = played_frames as f64 * 1e6 / self.rate;
        let off = wall - dev_t;
        if wall - self.short_start_us >= CLOCK_SHORT_US {
            // 짧은 창 마감: 그 창의 관측이 모두 긴 창 최솟값보다 크게 떠 있으면 공백 뒤다 → 긴 창을 버린다.
            let long_min = self.prev_mins.iter().copied().fold(self.cur_min, f64::min);
            if self.short_min.is_finite() && self.short_min - long_min > CLOCK_GAP_US {
                self.prev_mins = [f64::INFINITY; CLOCK_BUCKETS - 1];
                self.cur_min = self.short_min;
                self.bucket_start_us = wall;
                self.resets += 1;
            }
            self.short_start_us = wall;
            self.short_min = f64::INFINITY;
        }
        self.short_min = self.short_min.min(off);
        self.cur_min = self.cur_min.min(off);
        let m = self.prev_mins.iter().copied().fold(self.cur_min, f64::min);
        (dev_t + m).max(0.0) as u64
    }
}

/// 출력 콜백 타이밍(워커가 매 반복 계산).
#[derive(Debug, Clone, Copy)]
pub struct FeedTiming {
    /// 지난 반복 이후 출력 콜백이 돌았다(이번 반복에서 깊이를 잰다).
    pub new_callback: bool,
    /// 마지막 출력 콜백 시각 추정치(장치 클럭, µs).
    pub dev_now_us: u64,
    /// 워커가 실제로 깨어난 시각(µs).
    pub wall_us: u64,
    /// 다음 출력 콜백 예상 시각(µs).
    pub next_cb_us: u64,
    /// 다음 콜백이 꺼낼 장치 프레임 수.
    pub out_burst: usize,
    /// `dev_now_us`에 대응하는 누적 재생 프레임(그 콜백 끝).
    pub played: u64,
    pub out_rate: u32,
    /// 이 타이밍을 만들 때의 출력 콜백 시작 횟수. 콜백 도중이었으면 `None`(이번 반복은 판단하지 않음).
    pub cb_seq: Option<u64>,
}

impl FeedTiming {
    pub(super) fn dev_to_48(&self, frames: f64) -> f64 {
        frames * INTERNAL_RATE as f64 / self.out_rate.max(1) as f64
    }

    pub(super) fn chunk_dev(&self) -> usize {
        (OUT_CHUNK * self.out_rate as usize).div_ceil(INTERNAL_RATE as usize)
    }
}

/// 출력 콜백 관찰기: 재생 프레임 카운터로 콜백 시각을 추정하고(`DeviceClock`) 반복마다 `FeedTiming`을 만든다.
pub struct CallbackTracker {
    clock: DeviceClock,
    cb_played: u64,
    dev_now_us: u64,
}

impl CallbackTracker {
    pub fn new(out_rate: u32) -> Self {
        Self { clock: DeviceClock::new(out_rate), cb_played: 0, dev_now_us: 0 }
    }

    /// 장치 클럭은 재생 프레임 수(언더런 포함)로 추정한다(PR #5 H-1).
    pub fn observe(&mut self, c: &AudioCounters, wall_us: u64, out_burst: usize, out_rate: u32) -> FeedTiming {
        let begin = c.out_cb_begin.load(Ordering::SeqCst);
        let played = c.out_played_frames.load(Ordering::SeqCst);
        // 콜백 도중이면(시작 ≠ 끝) 재생 카운터와 링 점유량이 어긋날 수 있으므로 다음 반복으로 미룬다.
        let cb_seq = (c.out_cb_end.load(Ordering::SeqCst) == begin).then_some(begin);
        let new_callback = cb_seq.is_some() && played != self.cb_played && played > 0;
        if new_callback {
            self.cb_played = played;
            self.dev_now_us = self.clock.update(wall_us, played);
        }
        let out_rate = out_rate.max(1);
        let next_cb_us = if self.dev_now_us == 0 {
            0
        } else {
            self.dev_now_us + out_burst as u64 * 1_000_000 / out_rate as u64
        };
        FeedTiming { new_callback, dev_now_us: self.dev_now_us, wall_us, next_cb_us, out_burst, played: self.cb_played, out_rate, cb_seq }
    }
}
