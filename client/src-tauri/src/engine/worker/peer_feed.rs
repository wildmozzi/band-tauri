//! 피어 소스 공급(콜백 구동 믹스): 지터 버퍼 → 소스 링, 은닉은 마감 직전에만.

use std::sync::atomic::Ordering;

use ringbuf::traits::{Observer, Producer};
use ringbuf::HeapProd;

use super::super::config::INTERNAL_RATE;
use super::super::jitter::JitterBuffer;
use super::super::measure::EchoProbe;
use super::super::resample::StreamResampler;
use super::super::rt::SourceShared;
use super::clock::FeedTiming;
use super::{OUT_CHUNK, POLL};
use crate::net::media_link::PeerKey;

// 콜백 구동 믹스: 피어 소스 공급(PeerFeed)
// ============================================================================

/// 출력 콜백 마감 여유(µs): 다음 콜백 예상 시각보다 이만큼 앞에서 은닉을 결정한다.
/// 워커 폴링(0.5ms)과 Windows 기상 지연을 덮는다. 지터 목표(`ceil(p95/F)+1`프레임)의 +1프레임(≥ 2.5ms)이
/// 이 여유보다 커서 정상 상태에서는 마감 전에 실제 패킷이 이미 와 있다(engine-notes "콜백 구동 믹스").
pub const DEADLINE_GUARD_US: u64 = 1500;


/// `PeerFeed::service` 한 번의 결과(진단용).
#[derive(Debug, Default, Clone, Copy)]
pub struct FeedEvents {
    /// 마감에 쫓겨 은닉으로 채웠다.
    pub deadline_fill: bool,
    /// 콜백이 0으로 채워 건너뛴 장치 프레임.
    pub skipped_frames: u64,
    /// 링이 다음 콜백에 모자라고 은닉 마감이 한 폴링 주기 안이다. 워커는 짧게 자서 마감을 놓치지 않는다.
    pub urgent: bool,
    /// 이번 반복에 실제(은닉이 아닌) 오디오를 소스 링에 넣었다(P1-20 `firstAudioMs`).
    pub real_audio: bool,
}

/// 피어 하나의 출력 소스: 소스 링 슬롯 + 48k → 장치 레이트 리샘플러.
///
/// 규칙(설계 근거는 engine-notes "콜백 구동 믹스"):
/// 1. **실제 데이터는 미리**: 지터 버퍼가 은닉 없이 꺼낼 수 있으면(`can_pop_real`) 소스 링을
///    `다음 콜백 소비량 + 2청크`까지 채운다. 나머지는 지터 버퍼에 남겨 패킷 단위 버림이 동작하게 한다.
/// 2. **은닉은 마감 직전에만**: 링이 다음 콜백 소비량보다 적고 데이터가 없으면, 다음 콜백 예상 시각
///    − `DEADLINE_GUARD_US`가 지난 뒤에만 `pop`(PLC)한다. 늦게 오는 패킷을 마지막까지 기다린다.
/// 3. **깊이는 재생 위치 기준**: 콜백이 돌 때마다 `end_burst(콜백 시각, 벽시계, 링 점유량)`.
///    그래서 소스 링에 머무는 시간이 지터 버퍼 깊이(`bufferMs`)에 포함되고 목표로 제어된다.
///    믹스된 출력 링이 따로 없으므로 장치 버퍼 1개만큼의 하한이 사라진다.
/// 4. **언더런 때 지연을 쌓지 않음**: 콜백이 링 부족으로 0을 채운 프레임 수만큼 스트림을 건너뛴다.
/// 5. **적응 여유**(PR #11 M4): 언더런이 나면 피어별 여유(`margin`)를 청크 하나씩 늘린다. 여유만큼 선행량 상한·
///    부족 판정·은닉 마감을 앞당겨, 워커 기상이 늦거나 콜백이 격자보다 이르게 오는 환경에서 같은 언더런이 반복되지
///    않게 한다. 여유는 소스 링(재생 위치 앞)에 있으므로 `bufferMs`에 포함되고 지연 회계는 그대로다. 언더런이 없으면 0.
///    상한은 `min(다음 콜백 소비량, (F + 2ms)/2 × 프로필 배율)`이라 목표 추가분 2·여유가 1프레임 + 2ms를 넘지 않는다
///    (P1-5, PR #16 M3: 이전 상한은 콜백 소비량이라 10ms 버퍼에서 +19.6ms). 마지막 언더런 뒤 `MARGIN_HOLD_US` × 배율
///    동안 유지하고, 그 뒤 `MARGIN_STEP_US`마다 청크씩 줄인다(이전: 10초마다 청크 → 480프레임이면 75초).
pub struct PeerFeed {
    pub(super) slot: usize,
    pub(super) key: PeerKey,
    rs: StreamResampler,
    seen_underrun: u64,
    pub(super) seen: bool,
    /// 적응 여유(장치 프레임).
    margin: usize,
    /// 마지막 여유 증가(언더런) 시각(µs).
    margin_at_us: u64,
    /// 마지막 여유 감소 시각(µs).
    margin_step_us: u64,
}

/// 마지막 언더런 뒤 여유를 유지하는 시간(균형 프로필, `JitterTuning::margin_scale`배).
const MARGIN_HOLD_US: u64 = 10_000_000;
/// 유지 시간이 지난 뒤 여유를 청크 하나씩 줄이는 간격.
const MARGIN_STEP_US: u64 = 2_000_000;

impl PeerFeed {
    pub fn new(slot: usize, key: PeerKey, out_rate: u32, shared: &SourceShared) -> Result<Self, String> {
        Ok(Self {
            slot,
            key,
            rs: StreamResampler::new(INTERNAL_RATE, out_rate, 2)?,
            seen_underrun: shared.underrun_frames.load(Ordering::Relaxed),
            seen: true,
            margin: 0,
            margin_at_us: 0,
            margin_step_us: 0,
        })
    }

    /// 여유 상한(장치 프레임): 다음 콜백 소비량과 `(F + 2ms)/2 × 배율` 중 작은 값(최소 청크 하나).
    fn margin_cap(&self, jb: &JitterBuffer, t: &FeedTiming) -> usize {
        let cap48 = (jb.frame_samples() as f64 + 2.0 * INTERNAL_RATE as f64 / 1000.0) / 2.0 * jb.tuning().margin_scale;
        let cap_dev = (cap48 * t.out_rate as f64 / INTERNAL_RATE as f64) as usize;
        cap_dev.max(t.chunk_dev()).min(t.out_burst.max(t.chunk_dev()))
    }

    /// 은닉 판단에 필요한 p95 너머 깊이(샘플, 48k)를 지터 버퍼에 알린다(`JitterBuffer::set_consumer_lead`, P1-13).
    /// 여유는 두 번 쓰인다: 은닉 마감을 여유만큼 앞당기고(`guard_us`), 그 마감에 다음 콜백 소비량보다 여유만큼 더
    /// 채운다(`need`). 마감 시각에 실제 패킷이 와 있으려면 재생 위치 기준 깊이가 `p95 + DEADLINE_GUARD + 2·여유`
    /// 이상이어야 한다(engine-notes "콜백 구동 믹스"의 필요 조건에 여유를 넣은 것). 1배만 반영하면 결정적 스윕에서
    /// 언더런 수 초 뒤 은닉이 남았다.
    fn consumer_lead(&self, t: &FeedTiming) -> f64 {
        DEADLINE_GUARD_US as f64 * INTERNAL_RATE as f64 / 1e6 + t.dev_to_48(2.0 * self.margin as f64)
    }

    /// 현재 적응 여유(장치 프레임, 진단·테스트용).
    pub fn margin(&self) -> usize {
        self.margin
    }

    pub fn slot(&self) -> usize {
        self.slot
    }

    /// 링 + 리샘플러 출력에 있는 장치 프레임.
    pub(super) fn dev_ahead(&self, prod: &HeapProd<f32>) -> usize {
        prod.occupied_len() / 2 + self.rs.available()
    }

    /// 지금 소스 링 꼬리에 넣을 샘플의 재생(장치 쓰기) 예정 시각(µs, 워커 epoch). 링·리샘플러에 앞서 있는
    /// 양만큼 다음 출력 콜백보다 뒤다. 에코 측정의 도착 시각으로 쓴다.
    fn play_time_us(&self, prod: &HeapProd<f32>, t: &FeedTiming) -> f64 {
        let ahead48 = t.dev_to_48(self.dev_ahead(prod) as f64) + self.rs.pending_input() as f64;
        t.next_cb_us as f64 + ahead48 * 1e6 / INTERNAL_RATE as f64
    }

    /// 48k 청크 하나를 리샘플해 소스 링에 넣는다. 링이 가득 차면 리샘플러에 남는다.
    fn push(&mut self, chunk: &[f32], prod: &mut HeapProd<f32>, tmp: &mut [f32]) {
        self.rs.push(chunk);
        self.drain_rs(prod, tmp);
    }

    fn drain_rs(&mut self, prod: &mut HeapProd<f32>, tmp: &mut [f32]) {
        loop {
            let room = (prod.vacant_len() / 2).min(tmp.len() / 2);
            if room == 0 {
                break;
            }
            let n = self.rs.pop(&mut tmp[..room * 2]);
            if n == 0 {
                break;
            }
            prod.push_slice(&tmp[..n * 2]);
        }
    }

    /// 한 반복분 처리. `consistent()`는 `t`를 만든 뒤 링 점유량을 읽을 때까지 출력 콜백이 돌지 않았는지
    /// 확인한다. 돌았다면(링은 이미 비었는데 마감 시각은 이전 콜백 기준) 깊이 측정과 마감 판단을 이번
    /// 반복에서 하지 않는다. 이 검사가 없으면 한 주기 이르게 은닉했다(위상에 따라 수 초에 수 회).
    #[allow(clippy::too_many_arguments)]
    pub fn service(
        &mut self,
        jb: &mut JitterBuffer,
        prod: &mut HeapProd<f32>,
        sh: &SourceShared,
        t: &FeedTiming,
        consistent: &dyn Fn() -> bool,
        chunk: &mut [f32],
        tmp: &mut [f32],
        mut probe: Option<&mut EchoProbe>,
    ) -> FeedEvents {
        let mut ev = FeedEvents::default();
        let chunk = &mut chunk[..OUT_CHUNK * 2];
        self.drain_rs(prod, tmp);
        if t.new_callback {
            // (4) 콜백이 모자라 0을 채운 만큼 스트림을 건너뛴다. 안 하면 그만큼 재생이 영구히 늦어진다.
            let u = sh.underrun_frames.load(Ordering::Relaxed);
            let d = u.saturating_sub(self.seen_underrun);
            self.seen_underrun = u;
            if d > 0 && jb.is_playing() {
                // (5) 여유가 늘면 지터 목표도 깊어진다(`consumer_lead`). 0 채움이 이미 재생을 d만큼 늦췄으므로,
                // 늘어난 목표만큼은 건너뛰지 않고 남겨 두면 보정(삽입 램프) 없이 바로 새 목표 깊이가 된다.
                // 건너뛰기는 상한(max_ms) 적용 뒤의 목표 차이로 계산한다(PR #16 L1).
                let before = jb.target_samples();
                self.margin = (self.margin + t.chunk_dev()).min(self.margin_cap(jb, t));
                self.margin_at_us = t.wall_us;
                self.margin_step_us = t.wall_us;
                jb.set_consumer_lead(self.consumer_lead(t));
                let d48 = t.dev_to_48(d as f64);
                let skip = (d48 - (jb.target_samples() - before)).max(0.0).round();
                // 건너뛰지 않고 남긴 만큼 깊이가 바로 커진다. 깊이 추정에도 바로 반영해 삽입 보정이 끼지 않게 한다.
                jb.shift_depth(d48 - skip);
                let mut left = skip as usize;
                while left > 0 {
                    let k = left.min(OUT_CHUNK);
                    jb.pop(&mut chunk[..k * 2]);
                    left -= k;
                }
                // 실제로 건너뛴 양(장치 프레임, PR #16 L2). 0 채움 중 늘어난 목표로 남긴 몫은 빼고 센다.
                ev.skipped_frames = (skip * t.out_rate as f64 / INTERNAL_RATE as f64).round() as u64;
            } else if self.margin > 0 {
                let hold = (MARGIN_HOLD_US as f64 * jb.tuning().margin_scale) as u64;
                let now = t.wall_us;
                if now.saturating_sub(self.margin_at_us) > hold && now.saturating_sub(self.margin_step_us) > MARGIN_STEP_US {
                    self.margin = self.margin.saturating_sub(t.chunk_dev()).min(self.margin_cap(jb, t));
                    self.margin_step_us = now;
                }
            }
            jb.set_consumer_lead(self.consumer_lead(t));
            // (3) 재생 위치 기준 깊이: 링·리샘플러에 있는 양(재생 위치가 그만큼 과거)을 넘긴다.
            let ahead = t.dev_to_48(self.dev_ahead(prod) as f64) + self.rs.pending_input() as f64;
            if consistent() {
                jb.end_burst(t.dev_now_us, t.wall_us, ahead);
            }
        }
        if !jb.is_playing() {
            sh.playing.store(false, Ordering::Relaxed);
            return ev;
        }
        // (1) 실제 데이터는 미리
        // 패킷 꼬리(< 청크)까지 정확히 꺼낸다. 청크 단위로만 꺼내면 실제 데이터가 FIFO에 남아 있는데도
        // 마감에 은닉이 끼었다(위상에 따라 수 초에 수 회).
        // 유효 여유(PR #16 M1, P1-5): 여유가 늘면 목표가 2·여유만큼 깊어지는데, 0 채움으로 남긴 양이 모자라면 나머지는
        // 삽입 램프(수백 ms)로 채운다. 그동안 늘어난 여유를 마감·채울 양에 그대로 쓰면 깊이가 아직 없어 마감 채움(은닉)이
        // 났다. 깊이 부족분 s의 절반만큼 여유를 덜 써서 `깊이 ≥ 지터 + 1.5ms + 2·유효 여유`를 유지한다.
        let short48 = jb.depth_shortfall();
        let short_dev = (short48 / 2.0 * t.out_rate as f64 / INTERNAL_RATE as f64).ceil() as usize;
        let m_eff = self.margin.saturating_sub(short_dev);
        let lead_cap = t.out_burst + 2 * t.chunk_dev() + m_eff;
        while self.dev_ahead(prod) < lead_cap && prod.vacant_len() >= OUT_CHUNK * 4 {
            let k = jb.real_frames().min(OUT_CHUNK);
            if k == 0 {
                break;
            }
            jb.pop(&mut chunk[..k * 2]);
            if let Some(p) = probe.as_deref_mut() {
                p.record(&chunk[..k * 2], self.play_time_us(prod, t));
            }
            self.push(&chunk[..k * 2], prod, tmp);
            ev.real_audio = true;
        }
        // (2) 은닉은 마감 직전에만, 다음 콜백에 모자란 만큼만(+ 적응 여유)
        let need = t.out_burst + m_eff;
        let guard_us = DEADLINE_GUARD_US + m_eff as u64 * 1_000_000 / t.out_rate.max(1) as u64;
        let short = self.dev_ahead(prod) < need;
        if short && t.wall_us + guard_us >= t.next_cb_us && consistent() {
            ev.deadline_fill = true;
            let mut guard = 0;
            while self.dev_ahead(prod) < need && guard < 64 && jb.is_playing() {
                let deficit = t.dev_to_48((need - self.dev_ahead(prod)) as f64).ceil() as usize;
                let k = deficit.clamp(1, OUT_CHUNK);
                jb.pop(&mut chunk[..k * 2]);
                if let Some(p) = probe.as_deref_mut() {
                    p.record(&chunk[..k * 2], self.play_time_us(prod, t));
                }
                self.push(&chunk[..k * 2], prod, tmp);
                guard += 1;
            }
        } else if short && t.wall_us + guard_us + POLL.as_micros() as u64 >= t.next_cb_us {
            ev.urgent = true;
        }
        sh.playing.store(true, Ordering::Relaxed);
        ev
    }
}
