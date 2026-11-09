//! 피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC (설계 문서 1.2절).
//!
//! ## 깊이(depth)의 정의
//! 버퍼 깊이는 "지금 재생되는 샘플이 이상적인(최소 지연) 도착 시점보다 얼마나 늦게 재생되는가"로
//! 정의한다. 패킷 i의 전송 지연 `transit_i = arrival_i - ts_i`(샘플 단위)에 대해 최근 200개
//! 중 최솟값을 `min_transit`로 두면,
//!
//! ```text
//! depth = now - ts(재생 위치) - min_transit
//! ```
//!
//! 이 값은 지터에 흔들리지 않는 실제 재생 지연(버퍼 지연)이며, 송신측 클럭 드리프트가 있으면
//! 서서히 늘거나 준다. 이를 EWMA로 평활해 목표 깊이와 비교한다.
//!
//! ## 목표 깊이
//! 상대 지연 `transit_i - min_transit`의 95퍼센타일(최근 200개)을 p95라 할 때
//! `target = clamp(ceil(p95 / F) + 1, min_frames, max_frames) × F` (F = 패킷 프레임 길이).
//! 재생 요청 단위(내 프레임)가 F보다 크면 그 차이만큼 더한다.
//!
//! ## 보정
//! - 깊이 > 목표 + F/4 → 디코드한 프레임에서 1샘플 버림(크로스페이드 시프트)
//! - 깊이 < 목표 - F/4 → 1샘플 삽입
//! - 깊이 - 목표 ≥ 3F → 패킷(프레임) 단위로 버림
//!
//! ## 손실/지연
//! - 다음 seq가 없고 그 뒤 seq가 이미 있으면 손실로 보고 은닉(Opus: 다음 패킷 FEC → 없으면
//!   PLC, PCM: 직전 프레임 페이드아웃 1회 후 무음) 후 seq 전진.
//! - 다음 seq도 그 뒤도 없으면(언더런) 은닉 프레임을 끼워 넣되 seq는 전진하지 않는다. 늦게 온
//!   패킷을 그대로 재생하므로 깊이가 자연스럽게 늘어난다. 일정 시간 이상 지속되면 재버퍼링.
//! - 이미 재생 지난 seq는 폐기하고 `late` 증가.

use std::collections::VecDeque;

use super::codec::Decoder;
use super::config::{frame_samples_from_code, INTERNAL_RATE, MAX_PAYLOAD_BYTES};
use crate::net::packet::Header;

/// 슬롯 링 크기: 2초 분량(최소 프레임 2.5ms 기준).
const SLOT_COUNT: usize = 800;
/// 지연 통계 창 크기.
const DELAY_WINDOW: usize = 200;
/// p95/최솟값 재계산 주기(패킷 수).
const STATS_EVERY: usize = 10;
/// 깊이 EWMA 계수.
const DEPTH_ALPHA: f64 = 0.02;
/// 최대 패킷 프레임(10ms).
const MAX_PKT_FRAME: usize = 480;
/// 손실률 창: 최대 2000패킷(2.5ms × 5초).
const LOSS_WINDOW_MAX: usize = 2000;
/// 언더런 은닉이 이 시간(ms) 이상 지속되면 재버퍼링.
const STARVE_RESET_MS: f64 = 200.0;
/// 크로스페이드 시프트 램프 길이 상한.
const SHIFT_RAMP_MAX: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterSettings {
    /// 재생 요청 단위(채널당 샘플, 내 frame_samples).
    pub request_frames: usize,
    /// 0이면 자동.
    pub min_ms: f32,
    pub max_ms: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterStats {
    pub buffer_ms: f32,
    pub target_ms: f32,
    pub jitter_ms: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    /// 생산된 패킷 프레임 수(정상 디코드 + 은닉).
    pub produced: u64,
    pub dropped_frames: u64,
    pub samples_dropped: u64,
    pub samples_inserted: u64,
    pub rebuffers: u64,
}

struct Slot {
    seq: u32,
    valid: bool,
    len: usize,
    flags: u8,
    frame: usize,
    ts: u32,
    data: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 첫 패킷 대기 또는 목표 깊이까지 채우는 중.
    Buffering,
    Playing,
}

pub struct JitterBuffer {
    settings: JitterSettings,
    slots: Vec<Slot>,
    state: State,
    next_seq: u32,
    has_stream: bool,
    /// 마지막으로 본 최대 seq.
    highest: u32,

    decoder: Option<Decoder>,
    /// 디코드된 스테레오 인터리브 샘플 FIFO.
    fifo: VecDeque<f32>,
    /// FIFO 끝 샘플 다음 위치의 타임스탬프(확장).
    fifo_end_ts: i64,

    // 타임스탬프 언랩
    last_ts: u32,
    ts_ext_base: i64,

    // 지연 통계
    transits: [i64; DELAY_WINDOW],
    transit_count: usize,
    transit_pos: usize,
    since_stats: usize,
    scratch: Vec<i64>,
    min_transit: i64,
    p95_rel: f64,
    rfc_jitter: f64,
    last_transit: Option<i64>,

    depth_ewma: Option<f64>,
    pkt_frame: usize,
    pkt_channels: usize,

    // 은닉
    last_frame: Vec<f32>,
    last_frame_valid: bool,
    conceal_run: usize,
    starve_samples: usize,

    // 스크래치
    dec_buf: Vec<f32>,
    shift_buf: Vec<f32>,

    // 손실 창
    loss_ring: Vec<bool>,
    loss_pos: usize,
    loss_len: usize,
    loss_lost: usize,

    stats: JitterStats,
}

fn seq_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

impl JitterBuffer {
    pub fn new(settings: JitterSettings) -> Self {
        let slots = (0..SLOT_COUNT)
            .map(|_| Slot {
                seq: 0,
                valid: false,
                len: 0,
                flags: 0,
                frame: 0,
                ts: 0,
                data: vec![0u8; MAX_PAYLOAD_BYTES].into_boxed_slice(),
            })
            .collect();
        Self {
            settings,
            slots,
            state: State::Buffering,
            next_seq: 0,
            has_stream: false,
            highest: 0,
            decoder: None,
            fifo: VecDeque::with_capacity((MAX_PKT_FRAME * 8 + settings.request_frames * 2) * 2),
            fifo_end_ts: 0,
            last_ts: 0,
            ts_ext_base: 0,
            transits: [0; DELAY_WINDOW],
            transit_count: 0,
            transit_pos: 0,
            since_stats: 0,
            scratch: Vec::with_capacity(DELAY_WINDOW),
            min_transit: 0,
            p95_rel: 0.0,
            rfc_jitter: 0.0,
            last_transit: None,
            depth_ewma: None,
            pkt_frame: settings.request_frames,
            pkt_channels: 1,
            last_frame: vec![0.0; MAX_PKT_FRAME * 2],
            last_frame_valid: false,
            conceal_run: 0,
            starve_samples: 0,
            dec_buf: vec![0.0; MAX_PKT_FRAME * 2],
            shift_buf: vec![0.0; (MAX_PKT_FRAME + 1) * 2],
            loss_ring: vec![false; LOSS_WINDOW_MAX],
            loss_pos: 0,
            loss_len: 0,
            loss_lost: 0,
            stats: JitterStats::default(),
        }
    }

    fn samples_per_ms() -> f64 {
        INTERNAL_RATE as f64 / 1000.0
    }

    fn now_samples(now_us: u64) -> i64 {
        (now_us as i128 * INTERNAL_RATE as i128 / 1_000_000) as i64
    }

    fn unwrap_ts(&mut self, ts: u32) -> i64 {
        let d = ts.wrapping_sub(self.last_ts) as i32 as i64;
        // 첫 패킷이면 그대로
        if self.transit_count == 0 && self.last_transit.is_none() {
            self.last_ts = ts;
            self.ts_ext_base = ts as i64;
            return self.ts_ext_base;
        }
        let ext = self.ts_ext_base + d;
        if d > 0 {
            self.last_ts = ts;
            self.ts_ext_base = ext;
        }
        ext
    }

    /// 이미 본 타임스탬프 근처 값을 확장 타임스탬프로 변환(상태 변경 없음).
    fn ts_ext(&self, ts: u32) -> i64 {
        self.ts_ext_base + ts.wrapping_sub(self.last_ts) as i32 as i64
    }

    fn slot_index(seq: u32) -> usize {
        seq as usize % SLOT_COUNT
    }

    fn reset_stream(&mut self) {
        for s in &mut self.slots {
            s.valid = false;
        }
        self.state = State::Buffering;
        self.has_stream = false;
        self.fifo.clear();
        self.depth_ewma = None;
        self.conceal_run = 0;
        self.starve_samples = 0;
        self.last_frame_valid = false;
    }

    /// 패킷 프레임 길이(샘플).
    fn frame(&self) -> usize {
        self.pkt_frame.max(1)
    }

    pub fn target_samples(&self) -> f64 {
        let f = self.frame() as f64;
        let spm = Self::samples_per_ms();
        let auto = (self.p95_rel / f).ceil() + 1.0;
        let min_f = if self.settings.min_ms > 0.0 {
            (self.settings.min_ms as f64 * spm / f).ceil().max(1.0)
        } else {
            1.0
        };
        let max_f = (self.settings.max_ms as f64 * spm / f).floor().max(min_f);
        let frames = auto.clamp(min_f, max_f);
        frames * f + self.settings.request_frames.saturating_sub(self.frame()) as f64
    }

    /// 수신 패킷 삽입. `payload`는 복호화된 평문.
    pub fn insert(&mut self, hdr: &Header, payload: &[u8], now_us: u64) {
        let Some(frame) = frame_samples_from_code(hdr.frame_code) else {
            return;
        };
        if payload.len() > MAX_PAYLOAD_BYTES {
            return;
        }
        let seq = hdr.seq;

        if self.has_stream {
            let d = seq_diff(seq, self.next_seq);
            if d < 0 {
                if self.state == State::Buffering && -d < (SLOT_COUNT / 4) as i32 {
                    // 시작 직후 순서가 뒤바뀐 이전 패킷: 시작점을 당긴다.
                    self.next_seq = seq;
                } else {
                    self.stats.late += 1;
                    self.record_delay(hdr.timestamp, now_us);
                    return;
                }
            } else if d as usize >= SLOT_COUNT {
                // 너무 먼 미래: 스트림 재시작으로 간주
                self.reset_stream();
            }
        }
        if !self.has_stream {
            self.has_stream = true;
            self.next_seq = seq;
            self.highest = seq;
            self.state = State::Buffering;
        }
        if seq_diff(seq, self.highest) > 0 {
            self.highest = seq;
        }

        let slot = &mut self.slots[Self::slot_index(seq)];
        if slot.valid && slot.seq == seq {
            return; // 중복
        }
        slot.seq = seq;
        slot.valid = true;
        slot.len = payload.len();
        slot.flags = hdr.flags;
        slot.frame = frame;
        slot.ts = hdr.timestamp;
        slot.data[..payload.len()].copy_from_slice(payload);
        self.pkt_frame = frame;

        self.record_delay(hdr.timestamp, now_us);
    }

    fn record_delay(&mut self, ts: u32, now_us: u64) {
        let ts_ext = self.unwrap_ts(ts);
        let transit = Self::now_samples(now_us) - ts_ext;
        if let Some(prev) = self.last_transit {
            let d = (transit - prev).abs() as f64;
            self.rfc_jitter += (d - self.rfc_jitter) / 16.0;
        }
        self.last_transit = Some(transit);
        self.transits[self.transit_pos] = transit;
        self.transit_pos = (self.transit_pos + 1) % DELAY_WINDOW;
        self.transit_count = (self.transit_count + 1).min(DELAY_WINDOW);
        self.since_stats += 1;
        if self.transit_count <= STATS_EVERY || self.since_stats >= STATS_EVERY {
            self.since_stats = 0;
            self.recompute_delay_stats();
        } else if transit < self.min_transit {
            self.min_transit = transit;
        }
    }

    fn recompute_delay_stats(&mut self) {
        let n = self.transit_count;
        if n == 0 {
            return;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.transits[..n]);
        let min = self.scratch.iter().copied().min().unwrap_or(0);
        let k = ((n as f64 * 0.95).ceil() as usize).clamp(1, n) - 1;
        let (_, p, _) = self.scratch.select_nth_unstable(k);
        self.p95_rel = (*p - min) as f64;
        self.min_transit = min;
    }

    /// 현재 재생 위치 기준 깊이(샘플).
    fn depth_now(&self, now_us: u64) -> f64 {
        let play_ts = self.fifo_end_ts - (self.fifo.len() / 2) as i64;
        (Self::now_samples(now_us) - play_ts - self.min_transit) as f64
    }

    /// 다음 seq의 시작 타임스탬프 기준 깊이(버퍼링 중 판단용).
    fn depth_if_start(&mut self, now_us: u64) -> Option<f64> {
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        if !(slot.valid && slot.seq == self.next_seq) {
            return None;
        }
        let ts = slot.ts;
        let ts_ext = self.ts_ext(ts);
        Some((Self::now_samples(now_us) - ts_ext - self.min_transit) as f64)
    }

    /// 스테레오 인터리브로 `out.len() / 2` 프레임을 채운다.
    pub fn pop(&mut self, out: &mut [f32], now_us: u64) {
        let need = out.len() / 2;
        if self.state == State::Buffering {
            self.skip_missing_head();
            let ready = match self.depth_if_start(now_us) {
                Some(d) => d + need as f64 >= self.target_samples(),
                None => false,
            };
            if !ready {
                out.fill(0.0);
                return;
            }
            self.state = State::Playing;
            let slot = &self.slots[Self::slot_index(self.next_seq)];
            let ts = slot.ts;
            self.fifo_end_ts = self.ts_ext(ts);
            self.fifo.clear();
            self.depth_ewma = None;
            self.conceal_run = 0;
            self.starve_samples = 0;
        }

        while self.fifo.len() / 2 < need {
            if !self.produce_packet() {
                // 재버퍼링 진입
                break;
            }
        }

        let avail = (self.fifo.len() / 2).min(need);
        for (i, o) in out.iter_mut().enumerate().take(avail * 2) {
            *o = self.fifo.get(i).copied().unwrap_or(0.0);
        }
        self.fifo.drain(..avail * 2);
        out[avail * 2..].fill(0.0);

        if self.state == State::Playing {
            let d = self.depth_now(now_us);
            self.depth_ewma = Some(match self.depth_ewma {
                None => d,
                Some(e) => e + (d - e) * DEPTH_ALPHA,
            });
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

    fn has_later_packet(&self) -> bool {
        seq_diff(self.highest, self.next_seq) > 0
    }

    fn slot_has(&self, seq: u32) -> bool {
        let s = &self.slots[Self::slot_index(seq)];
        s.valid && s.seq == seq
    }

    fn push_loss(&mut self, lost: bool) {
        let win = ((5000.0 * Self::samples_per_ms()) as usize / self.frame()).clamp(1, LOSS_WINDOW_MAX);
        if self.loss_len >= win {
            // 가장 오래된 항목 제거
            let oldest = (self.loss_pos + LOSS_WINDOW_MAX - self.loss_len) % LOSS_WINDOW_MAX;
            if self.loss_ring[oldest] {
                self.loss_lost -= 1;
            }
            self.loss_len -= 1;
        }
        self.loss_ring[self.loss_pos] = lost;
        self.loss_pos = (self.loss_pos + 1) % LOSS_WINDOW_MAX;
        self.loss_len += 1;
        if lost {
            self.loss_lost += 1;
        }
    }

    /// 패킷 하나만큼 FIFO에 생산. 재버퍼링으로 전환하면 false.
    fn produce_packet(&mut self) -> bool {
        let f = self.frame();
        let target = self.target_samples();

        // 프레임 단위 버림: 깊이가 목표보다 3프레임 이상 크면
        if let Some(e) = self.depth_ewma {
            if e - target >= 3.0 * f as f64 && self.has_later_packet() {
                let idx = Self::slot_index(self.next_seq);
                let skipped_frame = if self.slots[idx].valid && self.slots[idx].seq == self.next_seq {
                    self.slots[idx].valid = false;
                    self.slots[idx].frame
                } else {
                    f
                };
                self.next_seq = self.next_seq.wrapping_add(1);
                self.fifo_end_ts += skipped_frame as i64;
                self.depth_ewma = Some(e - skipped_frame as f64);
                self.stats.dropped_frames += 1;
            }
        }

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
            if !self.decoder.as_ref().is_some_and(|d| d.matches(opus, ch)) {
                self.decoder = Decoder::new(opus, ch).ok();
            }
            let out = &mut self.dec_buf[..pframe * ch];
            let decoded = match self.decoder.as_mut() {
                Some(d) => d.decode(&self.slots[idx].data[..len], out).unwrap_or(0),
                None => 0,
            };
            if decoded != pframe {
                out.fill(0.0);
            }
            frames = pframe;
            self.slots[idx].valid = false;
            self.next_seq = seq.wrapping_add(1);
            self.fifo_end_ts = self.ts_ext(ts) + pframe as i64;
            self.last_frame[..frames * ch].copy_from_slice(&self.dec_buf[..frames * ch]);
            self.last_frame_valid = true;
            self.pkt_channels = ch;
            self.conceal_run = 0;
            self.starve_samples = 0;
            self.push_loss(false);
        } else {
            let lost = self.has_later_packet();
            if !lost {
                // 언더런: seq 전진 없이 은닉 프레임 삽입
                self.starve_samples += f;
                if self.starve_samples as f64 > STARVE_RESET_MS * Self::samples_per_ms() {
                    self.stats.rebuffers += 1;
                    // 다음 패킷이 오면 그 seq부터 다시 버퍼링
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
                    let nidx = Self::slot_index(next);
                    let ns = &self.slots[nidx];
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
            self.conceal_run += 1;
            self.stats.concealed += 1;
            if lost {
                self.next_seq = seq.wrapping_add(1);
                self.push_loss(true);
            }
            self.fifo_end_ts += frames as i64;
        }
        self.stats.produced += 1;

        // 샘플 단위 드리프트 보정 (불감대 ±F/4)
        let half = f as f64 / 4.0;
        let mut out_frames = frames;
        let mut use_shift = false;
        if let Some(e) = self.depth_ewma {
            if frames > 2 {
                if e > target + half {
                    shift_one(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, true);
                    out_frames = frames - 1;
                    use_shift = true;
                    self.stats.samples_dropped += 1;
                    self.depth_ewma = Some(e - 1.0);
                } else if e < target - half {
                    shift_one(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, false);
                    out_frames = frames + 1;
                    use_shift = true;
                    self.stats.samples_inserted += 1;
                    self.depth_ewma = Some(e + 1.0);
                }
            }
        }
        let src: &[f32] = if use_shift { &self.shift_buf[..out_frames * ch] } else { &self.dec_buf[..out_frames * ch] };
        // 스테레오로 FIFO에 적재. 보정으로 바뀐 길이는 재생 위치 계산에 반영된다
        // (fifo_end_ts는 원래 프레임 끝 그대로 두므로 1샘플 버리면 재생 위치가 1 앞당겨짐).
        for i in 0..out_frames {
            if ch == 2 {
                self.fifo.push_back(src[i * 2]);
                self.fifo.push_back(src[i * 2 + 1]);
            } else {
                self.fifo.push_back(src[i]);
                self.fifo.push_back(src[i]);
            }
        }
        true
    }

    pub fn stats(&self) -> JitterStats {
        let spm = Self::samples_per_ms() as f32;
        let mut s = self.stats;
        s.buffer_ms = if self.state == State::Playing {
            self.depth_ewma.unwrap_or(0.0) as f32 / spm
        } else {
            0.0
        };
        s.target_ms = self.target_samples() as f32 / spm;
        s.jitter_ms = self.rfc_jitter as f32 / spm;
        s.loss_pct = if self.loss_len > 0 {
            self.loss_lost as f32 * 100.0 / self.loss_len as f32
        } else {
            0.0
        };
        s
    }

    pub fn is_playing(&self) -> bool {
        self.state == State::Playing
    }
}

/// 1샘플 버림(`drop=true`) 또는 삽입. 프레임 중앙에서 짧은 램프로 읽기 위치를 ±1 시프트한다
/// (선형 보간 크로스페이드). `dst`는 (frames±1)×ch 이상이어야 한다.
pub fn shift_one(src: &[f32], ch: usize, frames: usize, dst: &mut [f32], drop: bool) {
    let out_frames = if drop { frames - 1 } else { frames + 1 };
    let ramp = (frames / 2).clamp(1, SHIFT_RAMP_MAX);
    let p = ((frames - ramp) / 2) as f32;
    let l = ramp as f32;
    let last = (frames - 1) as f32;
    for j in 0..out_frames {
        let r = ((j as f32 - p) / l).clamp(0.0, 1.0);
        let pos = if drop { j as f32 + r } else { j as f32 - r }.clamp(0.0, last);
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(frames - 1);
        let w = pos - i0 as f32;
        for c in 0..ch {
            dst[j * ch + c] = src[i0 * ch + c] * (1.0 - w) + src[i1 * ch + c] * w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::codec::Encoder;
    use crate::engine::config::{frame_code_from_samples, CodecKind};
    use crate::net::packet::{PacketType, FLAG_OPUS};
    use rand::{Rng, SeedableRng};

    struct Sender {
        enc: Encoder,
        frame: usize,
        seq: u32,
        ts: u32,
        phase: f64,
        opus: bool,
    }

    impl Sender {
        fn new(codec: CodecKind, frame: usize, seq0: u32, ts0: u32) -> Self {
            Self {
                enc: Encoder::new(codec, 1, 96_000).unwrap(),
                frame,
                seq: seq0,
                ts: ts0,
                phase: 0.0,
                opus: codec == CodecKind::Opus,
            }
        }
        fn next(&mut self) -> (Header, Vec<u8>) {
            let mut pcm = vec![0f32; self.frame];
            for s in pcm.iter_mut() {
                *s = (self.phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                self.phase = (self.phase + 440.0 / 48000.0).fract();
            }
            let mut buf = vec![0u8; MAX_PAYLOAD_BYTES];
            let n = self.enc.encode(&pcm, &mut buf).unwrap();
            buf.truncate(n);
            let h = Header {
                ptype: PacketType::Audio,
                ssrc: 1,
                seq: self.seq,
                timestamp: self.ts,
                flags: if self.opus { FLAG_OPUS } else { 0 },
                frame_code: frame_code_from_samples(self.frame).unwrap(),
            };
            self.seq = self.seq.wrapping_add(1);
            self.ts = self.ts.wrapping_add(self.frame as u32);
            (h, buf)
        }
    }

    struct SimResult {
        stats: JitterStats,
        /// (초, buffer_ms) 샘플
        trace: Vec<(f64, f32)>,
        silent_pops_after_start: usize,
    }

    /// 이벤트 시뮬레이션. 송신 클럭은 (1+drift_ppm)배 빠르게, 네트워크 지연은 base + U(0,jitter),
    /// 손실 확률 loss. 수신측은 request 단위로 정확한 주기로 pop.
    #[allow(clippy::too_many_arguments)]
    fn simulate(
        codec: CodecKind,
        frame: usize,
        request: usize,
        drift_ppm: f64,
        jitter_ms: f64,
        loss: f64,
        seconds: f64,
        seed: u64,
    ) -> SimResult {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut jb = JitterBuffer::new(JitterSettings { request_frames: request, min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(codec, frame, u32::MAX - 50, u32::MAX - 10_000);
        let send_period_us = frame as f64 / 48000.0 * 1e6 / (1.0 + drift_ppm * 1e-6);
        let pop_period_us = request as f64 / 48000.0 * 1e6;
        let base_us = 3000.0;

        // 미리 도착 이벤트 생성
        let mut arrivals: Vec<(f64, Header, Vec<u8>)> = Vec::new();
        let mut t = 0.0;
        while t < seconds * 1e6 {
            let (h, p) = tx.next();
            if rng.gen::<f64>() >= loss {
                let d = base_us + rng.gen::<f64>() * jitter_ms * 1000.0;
                arrivals.push((t + d, h, p));
            }
            t += send_period_us;
        }
        arrivals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut out = vec![0f32; request * 2];
        let mut ai = 0;
        let mut pop_t = 0.0;
        let mut trace = Vec::new();
        let mut silent = 0;
        let mut started = false;
        while pop_t < seconds * 1e6 {
            while ai < arrivals.len() && arrivals[ai].0 <= pop_t {
                let (at, h, p) = &arrivals[ai];
                jb.insert(h, p, *at as u64);
                ai += 1;
            }
            jb.pop(&mut out, pop_t as u64);
            if jb.is_playing() {
                started = true;
            }
            if started && pop_t > 1e6 {
                let e: f32 = out.iter().map(|x| x * x).sum();
                if e < 1e-6 {
                    silent += 1;
                }
            }
            if (pop_t as u64) % 250_000 < pop_period_us as u64 {
                trace.push((pop_t / 1e6, jb.stats().buffer_ms));
            }
            pop_t += pop_period_us;
        }
        SimResult { stats: jb.stats(), trace, silent_pops_after_start: silent }
    }

    fn mean_buffer(trace: &[(f64, f32)], from: f64, to: f64) -> f32 {
        let v: Vec<f32> = trace.iter().filter(|(t, _)| *t >= from && *t < to).map(|x| x.1).collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    }

    #[test]
    fn clean_network_low_latency() {
        let r = simulate(CodecKind::Pcm16, 240, 240, 0.0, 0.0, 0.0, 5.0, 1);
        assert_eq!(r.stats.concealed, 0, "{:?}", r.stats);
        assert_eq!(r.stats.late, 0);
        assert!(r.stats.buffer_ms < 12.0, "깨끗한 망에서는 지연이 작아야 함: {:?}", r.stats);
        assert_eq!(r.silent_pops_after_start, 0);
    }

    #[test]
    fn jitter_and_loss_opus() {
        let r = simulate(CodecKind::Opus, 240, 240, 0.0, 20.0, 0.05, 20.0, 2);
        let s = r.stats;
        let ratio = s.concealed as f64 / s.produced as f64;
        assert!(ratio < 0.12, "은닉 비율 {ratio} {s:?}");
        assert!((2.0..12.0).contains(&s.loss_pct), "손실률 추정 {s:?}");
        assert!(s.target_ms >= 15.0 && s.target_ms <= 35.0, "목표 깊이가 지터를 반영해야 함 {s:?}");
        let late = mean_buffer(&r.trace, 10.0, 20.0);
        assert!((late - s.target_ms).abs() < 8.0, "깊이 {late} vs 목표 {}", s.target_ms);
    }

    #[test]
    fn reordering_handled() {
        // 지터 20ms에 2.5ms 프레임이면 순서가 많이 뒤바뀐다
        let r = simulate(CodecKind::Pcm16, 120, 120, 0.0, 20.0, 0.0, 10.0, 3);
        let ratio = r.stats.concealed as f64 / r.stats.produced as f64;
        assert!(ratio < 0.05, "순서 뒤바뀜에서 은닉 비율 {ratio} {:?}", r.stats);
    }

    fn drift_case(ppm: f64) {
        let r = simulate(CodecKind::Pcm16, 240, 240, ppm, 5.0, 0.0, 60.0, 4);
        let s = r.stats;
        let mid = mean_buffer(&r.trace, 20.0, 30.0);
        let end = mean_buffer(&r.trace, 50.0, 60.0);
        println!("drift {ppm}ppm: target {:.2}ms mid {mid:.2}ms end {end:.2}ms stats {s:?}", s.target_ms);
        assert!((end - s.target_ms).abs() < 3.0, "드리프트 {ppm}ppm에서 목표 근처 수렴: end={end} target={}", s.target_ms);
        assert!((end - mid).abs() < 2.0, "지연이 누적되면 안 됨: mid={mid} end={end}");
        // 200ppm × 60s ≈ 576샘플. 시작 직후 목표까지 채우는 삽입이 섞이므로 방향별 하한만 본다.
        if ppm > 0.0 {
            assert!(s.samples_dropped + s.dropped_frames * 240 >= 250, "{s:?}");
        } else {
            assert!(s.samples_inserted >= 250, "{s:?}");
        }
        assert_eq!(r.silent_pops_after_start, 0, "드리프트 보정 중 끊김 없음");
    }

    #[test]
    fn drift_plus_200ppm_converges() {
        drift_case(200.0);
    }

    #[test]
    fn drift_minus_200ppm_converges() {
        drift_case(-200.0);
    }

    #[test]
    fn mismatched_frame_sizes() {
        // 피어는 2.5ms, 나는 10ms 단위로 꺼냄
        let r = simulate(CodecKind::Opus, 120, 480, 0.0, 5.0, 0.0, 5.0, 5);
        assert_eq!(r.silent_pops_after_start, 0, "{:?}", r.stats);
        let r = simulate(CodecKind::Opus, 480, 120, 0.0, 5.0, 0.0, 5.0, 6);
        assert_eq!(r.silent_pops_after_start, 0, "{:?}", r.stats);
    }

    #[test]
    fn late_packets_discarded() {
        let mut jb = JitterBuffer::new(JitterSettings { request_frames: 240, min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 10, 0);
        let pkts: Vec<_> = (0..20).map(|_| tx.next()).collect();
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        for (h, p) in pkts.iter().take(10) {
            jb.insert(h, p, t);
            jb.pop(&mut out, t);
            t += 5000;
        }
        for _ in 0..5 {
            jb.pop(&mut out, t);
            t += 5000;
        }
        let before = jb.stats().late;
        jb.insert(&pkts[0].0, &pkts[0].1, t);
        assert_eq!(jb.stats().late, before + 1);
    }

    #[test]
    fn pcm_concealment_fades_then_silence() {
        let mut jb = JitterBuffer::new(JitterSettings { request_frames: 240, min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        // 정상 재생
        for _ in 0..20 {
            let (h, p) = tx.next();
            jb.insert(&h, &p, t);
            jb.pop(&mut out, t);
            t += 5000;
        }
        // 3개 손실 후 다음 패킷 도착
        let lost: Vec<_> = (0..3).map(|_| tx.next()).collect();
        drop(lost);
        let (h, p) = tx.next();
        jb.insert(&h, &p, t);
        let c0 = jb.stats().concealed;
        let mut energies = vec![];
        for _ in 0..6 {
            jb.pop(&mut out, t);
            energies.push(out.iter().map(|x| x * x).sum::<f32>());
            t += 5000;
            let (h, p) = tx.next();
            jb.insert(&h, &p, t);
        }
        assert!(jb.stats().concealed >= c0 + 3);
        assert!(energies.iter().any(|&e| e < 1e-9), "페이드 1회 후 무음 프레임이 있어야 함: {energies:?}");
    }

    #[test]
    fn shift_one_lengths_and_continuity() {
        let src: Vec<f32> = (0..240).map(|i| i as f32).collect();
        let mut dst = vec![0f32; 241];
        shift_one(&src, 1, 240, &mut dst, true);
        assert_eq!(dst[0], 0.0);
        assert!((dst[238] - 239.0).abs() < 1e-4);
        for w in dst[..239].windows(2) {
            assert!(w[1] - w[0] >= 1.0 - 1e-4 && w[1] - w[0] <= 2.0 + 1e-4);
        }
        shift_one(&src, 1, 240, &mut dst, false);
        assert_eq!(dst[0], 0.0);
        assert!((dst[240] - 239.0).abs() < 1e-4);
        for w in dst.windows(2) {
            assert!(w[1] - w[0] >= -1e-4 && w[1] - w[0] <= 1.0 + 1e-4);
        }
    }
}
