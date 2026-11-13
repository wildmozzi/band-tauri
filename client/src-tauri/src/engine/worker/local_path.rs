//! 로컬 경로 단계(엔진 ②b): 모니터·믹스 탭. 48k 입력 블록 → 로컬 FIFO → (리샘플) → 로컬 소스 링.
//!
//! 로컬 FIFO(`local.rs`)가 입·출력 클럭 차를 흡수하고 자기 수위를 제어하므로, 소스 링은 콜백이 소비한 만큼
//! "장치 버퍼 1개 + 청크 + 적응 여유"로 다시 채운다(모니터 경로는 GOAL 지표 밖).
//!
//! 바뀌는 이유: 모니터·탭 경로, 로컬 여유 적응. 모르는 것: net, 코덱, 피어.
//! 호출 순서(조정자가 지킨다): `configure` → `push`(48k 블록마다) → `service` → `track_occupancy`.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};
use ringbuf::HeapProd;

use super::{chunk_dev, in_burst48, out_burst_dev, to48, LocalFifo, WorkerStats, OUT_CHUNK};
use crate::engine::config::INTERNAL_RATE;
use crate::engine::device::StreamInfo;
use crate::engine::mixer::MixControls;
use crate::engine::resample::StreamResampler;
use crate::engine::rt::{SourceBank, LOCAL_SOURCE};
use crate::engine::stats::AudioCounters;

pub(super) struct LocalPath {
    /// 내부 채널 수(1이면 `push`가 스테레오로 펼친다).
    ich: usize,
    local: LocalFifo,
    /// 로컬(모니터) 소스: 48k → 장치 레이트.
    local_rs: StreamResampler,
    local_buf: Vec<f32>,
    stereo_buf: Vec<f32>,
    tmp: Vec<f32>,
    /// 로컬 소스 링 생산자(슬롯 `LOCAL_SOURCE`).
    prod: HeapProd<f32>,
    safety: u64,
    seen_xruns: u64,
    last_xrun: Duration,
    monitor_ewma_us: f64,
}

impl LocalPath {
    pub(super) fn new(ich: usize, out_rate: u32, prod: HeapProd<f32>) -> Result<Self, String> {
        Ok(Self {
            ich,
            local: LocalFifo::new(),
            local_rs: StreamResampler::new(INTERNAL_RATE, out_rate, 2)?,
            local_buf: vec![0.0; OUT_CHUNK * 2],
            stereo_buf: vec![0.0; 2048 * 2],
            tmp: vec![0.0; 8192],
            prod,
            safety: 0,
            seen_xruns: 0,
            last_xrun: Duration::ZERO,
            monitor_ewma_us: 0.0,
        })
    }

    /// 입·출력 버스트(48k 환산)로 로컬 FIFO를 맞춘다. 값이 같으면 아무것도 안 한다.
    /// `granularity`: 모니터/탭은 프레임 누적 전에 분기하므로 입력 방출 단위는 리샘플러 청크(패스스루면 1)다.
    pub(super) fn configure(&mut self, info: &StreamInfo, counters: &AudioCounters, granularity: usize) {
        let out48 = to48(out_burst_dev(info, counters) as u32, info.out_rate);
        self.local.configure(in_burst48(info, counters), out48, granularity);
    }

    /// 48k 입력 블록(내부 채널 수) 하나. 블록마다 부른다(이어 붙여 넣으면 넘침 횟수가 달라진다).
    pub(super) fn push(&mut self, block: &[f32]) {
        if self.ich == 2 {
            self.local.push(block);
        } else {
            let sb = &mut self.stereo_buf;
            for (d, &s) in sb.as_chunks_mut::<2>().0.iter_mut().zip(block.iter()) {
                d[0] = s;
                d[1] = s;
            }
            let n = block.len() * 2;
            self.local.push(&self.stereo_buf[..n]);
        }
    }

    /// xrun이 나면 로컬 소스 여유를 청크 하나씩 늘리고(최대 장치 버퍼 1개), 10초 동안 없으면 줄인다.
    /// 콜백 간격이 들쭉날쭉하거나 워커 스케줄링이 늦는 환경(느린 CI 러너 포함)에 적응한다.
    fn adapt_safety(&mut self, info: &StreamInfo, counters: &AudioCounters, epoch: Instant, chunk_dev: u64) {
        let xruns = counters.xruns.load(Ordering::Relaxed);
        let now = epoch.elapsed();
        if xruns > self.seen_xruns {
            self.seen_xruns = xruns;
            self.safety = (self.safety + chunk_dev).min(out_burst_dev(info, counters));
            self.last_xrun = now;
        } else if self.safety > 0 && now.saturating_sub(self.last_xrun) > Duration::from_secs(10) {
            self.safety = self.safety.saturating_sub(chunk_dev);
            self.last_xrun = now;
        }
    }

    fn fill(&self) -> u64 {
        (self.prod.occupied_len() / 2 + self.local_rs.available()) as u64
    }

    /// 로컬(모니터 + 탭) 소스 공급. 모니터 게인을 로컬 슬롯에 쓰고(반복마다, 맨 앞), 소스 링을 목표 수위까지 채운다.
    pub(super) fn service(&mut self, info: &StreamInfo, counters: &AudioCounters, controls: &MixControls, bank: &SourceBank, stats: &WorkerStats, epoch: Instant) {
        let mon = if controls.monitor_enabled.load(Ordering::Relaxed) { controls.monitor_gain.load() } else { 0.0 };
        let sh = &bank.slots[LOCAL_SOURCE];
        sh.set_gains((mon, mon), (1.0, 1.0));
        let chunk_dev = chunk_dev(info);
        self.adapt_safety(info, counters, epoch, chunk_dev);
        let target = out_burst_dev(info, counters) + chunk_dev + self.safety;
        if self.fill() >= target {
            return;
        }
        // 한 번에 너무 많이 만들지 않도록 상한(장치 버퍼 4개분)
        let max_chunks = ((out_burst_dev(info, counters) * 4) / chunk_dev.max(1) + 2) as usize;
        let mut produced = 0;
        while self.fill() < target && produced < max_chunks {
            self.local.pull(&mut self.local_buf);
            self.local_rs.push(&self.local_buf);
            loop {
                let room = (self.prod.vacant_len() / 2).min(self.tmp.len() / 2);
                let n = if room == 0 { 0 } else { self.local_rs.pop(&mut self.tmp[..room * 2]) };
                if n == 0 {
                    break;
                }
                self.prod.push_slice(&self.tmp[..n * 2]);
            }
            produced += 1;
        }
        if produced > 0 {
            self.local.end_burst();
            sh.playing.store(true, Ordering::Relaxed);
            stats.local_corrections.store(self.local.corrections, Ordering::Relaxed);
            stats.local_underruns.store(self.local.underruns, Ordering::Relaxed);
        }
    }

    /// 모니터 경로 점유량(시간) 이동 평균(리뷰 M12). `pending48` = 입력 쪽(리샘플러)에 남은 48k 프레임(`Capture` 몫).
    pub(super) fn track_occupancy(&mut self, info: &StreamInfo, stats: &WorkerStats, pending48: usize) {
        let in48 = (pending48 + self.local.frames()) as f64;
        let local_dev = (self.prod.occupied_len() / 2 + self.local_rs.available()) as f64;
        let out_us = local_dev * 1e6 / info.out_rate.max(1) as f64;
        let mon_us = in48 * 1e6 / INTERNAL_RATE as f64 + out_us;
        self.monitor_ewma_us += (mon_us - self.monitor_ewma_us) * 0.01;
        stats.monitor_occupancy_us.store(self.monitor_ewma_us as u64, Ordering::Relaxed);
    }
}
