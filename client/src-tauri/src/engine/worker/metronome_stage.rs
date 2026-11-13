//! 메트로놈 단계(엔진 ②b, 공유 메트로놈 P2-3): 다음 클릭 프레임을 올리고(시계 추정이 바뀌면 다시 맞춤), 상태를 0.1초마다 알린다.
//!
//! 순수 스케줄러는 `metronome::BeatScheduler`다. 이 단계는 그 입력(적용한 계획, 기준 피어 시계 추정, 숨은 출력 지연)을
//! 모아 넘기고 상태를 보고한다.
//! 바뀌는 이유: 박 예약 입력(시계 추정·숨은 출력 지연), 상태 보고 필드. 모르는 것: 코덱, 소스 링, 피어 미디어.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{FeedTiming, WorkerStats};
use crate::engine::device::StreamInfo;
use crate::engine::metronome::{choose_hidden_out, hidden_out_latency_us, measured_hidden_out_us, BeatScheduler, MetronomeCtl, MetronomePlan, OutClock};
use crate::engine::stats::AudioCounters;
use crate::net::txlat::LockWait;
use crate::net::NetShared;

pub(super) struct MetronomeStage {
    ctl: Arc<MetronomeCtl>,
    /// 기준 피어 시계 조회(네트워크가 있을 때만).
    net: Option<Arc<NetShared>>,
    beats: BeatScheduler,
    /// 적용한 계획 (세대, 계획).
    plan: (u64, MetronomePlan),
    stats_at: u64,
    stats_gen: u64,
}

impl MetronomeStage {
    pub(super) fn new(ctl: Arc<MetronomeCtl>, net: Option<Arc<NetShared>>) -> Self {
        Self { ctl, net, beats: BeatScheduler::new(), plan: (0, MetronomePlan::default()), stats_at: 0, stats_gen: 0 }
    }

    /// 반복마다, `FeedTiming`을 만든 뒤·피어 공급 앞. `stats`는 락 자리별 대기 기록(진단)에만 쓴다.
    pub(super) fn service(&mut self, info: &StreamInfo, counters: &AudioCounters, stats: &WorkerStats, t: &FeedTiming, lock_wait: &mut LockWait) {
        if t.cb_seq.is_none() {
            return; // 콜백 도중: 재생 카운터와 시각이 어긋날 수 있다
        }
        let sites = &stats.lock_sites;
        {
            let p = lock_wait.lock(&self.ctl.plan, &sites.metro_plan);
            if p.0 != self.plan.0 {
                self.plan = p.clone();
            }
        }
        let (gen, plan) = (self.plan.0, &self.plan.1);
        let est = match (&plan.ref_peer_id, self.net.as_ref()) {
            (Some(id), Some(net)) if plan.running => net.peer_clock_timed(id, t.wall_us, lock_wait, &sites.peer_clock),
            _ => None,
        };
        let offset = match &plan.ref_peer_id {
            None => Some(0.0),
            Some(_) => est.map(|e| e.offset_us),
        };
        let rate = t.out_rate as f64;
        let driver = hidden_out_latency_us(info.out_reported_latency_frames, t.out_burst as u64, rate);
        // 드라이버가 출력 지연을 알려 주지 않으면 루프백 보정값으로(PR #34 M2(a)). 입력 쪽 보고가 있으면 그만큼 뺀다.
        let loopback = (*lock_wait.lock(&self.ctl.loopback_offset_ms, &sites.metro_offset)).map(|o| {
            let in_burst = match counters.in_cb_frames.load(Ordering::Relaxed) {
                0 => info.buffer_frames,
                n => n,
            };
            measured_hidden_out_us(o, hidden_out_latency_us(info.in_reported_latency_frames, in_burst as u64, info.in_rate as f64))
        });
        let hidden = choose_hidden_out(driver, loopback);
        let extra_out_us = hidden.map_or(0.0, |h| h.0);
        let clock = OutClock { dev_now_us: t.dev_now_us as f64, played: t.played, rate, burst: t.out_burst as u64, extra_out_us };
        let beat = self.beats.tick(gen, plan, offset, &clock, &self.ctl.click);
        if t.wall_us.saturating_sub(self.stats_at) >= 100_000 || gen != self.stats_gen {
            self.stats_at = t.wall_us;
            self.stats_gen = gen;
            let mut s = lock_wait.lock(&self.ctl.stats, &sites.metro_stats);
            s.running = plan.running;
            s.synced = offset.is_some();
            s.bpm = plan.bpm;
            s.beats_per_bar = plan.beats_per_bar;
            s.start_us = plan.start_us;
            s.ref_peer_id.clone_from(&plan.ref_peer_id);
            s.clock_offset_ms = offset.map(|o| o / 1000.0);
            s.clock_rtt_ms = est.map(|e| e.rtt_us / 1000.0);
            s.drift_ppm = est.map(|e| e.drift_ppm);
            s.beat = beat;
            s.output_latency_ms = hidden.map(|h| h.0 / 1000.0);
            s.output_latency_source = hidden.map(|h| h.1);
            s.ref_clock_unsupported = match (&plan.ref_peer_id, self.net.as_ref()) {
                (Some(id), Some(net)) if plan.running && est.is_none() => net.peer_clock_unsupported_timed(id, lock_wait, &sites.peer_clock),
                _ => false,
            };
            s.clock_extrapolation_ms = est.map(|e| e.extrapolation_us / 1000.0);
        }
    }
}
