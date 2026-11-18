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
use band_dsp::config::{ResampleQuality, INTERNAL_RATE};
use crate::engine::device::StreamInfo;
use band_dsp::mixer::MixControls;
use band_dsp::resample::StreamResampler;
use crate::engine::rt::SourceShared;
use band_dsp::stats::AudioCounters;

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
    /// 마지막으로 본 로컬 슬롯의 언더런 누적(장치 프레임, `SourceShared.underrun_frames`).
    seen_underrun: u64,
    last_xrun: Duration,
    monitor_ewma_us: f64,
}

impl LocalPath {
    pub(super) fn new(ich: usize, out_rate: u32, prod: HeapProd<f32>, quality: ResampleQuality) -> Result<Self, String> {
        Ok(Self {
            ich,
            local: LocalFifo::new(),
            local_rs: StreamResampler::new(INTERNAL_RATE, out_rate, 2, quality)?,
            local_buf: vec![0.0; OUT_CHUNK * 2],
            stereo_buf: vec![0.0; 2048 * 2],
            tmp: vec![0.0; 8192],
            prod,
            safety: 0,
            seen_underrun: 0,
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

    /// 로컬 소스 링이 모자라 콜백이 0을 채웠으면(로컬 슬롯 언더런) 여유를 청크 하나씩 늘리고(최대 장치 버퍼 1개),
    /// 10초 동안 없으면 줄인다. 콜백 간격이 들쭉날쭉하거나 워커 스케줄링이 늦는 환경(느린 CI 러너 포함)에 적응한다.
    /// 전역 `xruns`가 아니라 로컬 슬롯 것만 본다: 전역 값에는 피어 소스 부족(네트워크·피어 마감)과 입력 오버런도 섞여,
    /// 로컬과 무관한 사건에 모니터 지연이 최대 장치 버퍼 1개만큼 늘었다.
    fn adapt_safety(&mut self, underrun_frames: u64, out_burst: u64, now: Duration, chunk_dev: u64) {
        if underrun_frames > self.seen_underrun {
            self.seen_underrun = underrun_frames;
            self.safety = (self.safety + chunk_dev).min(out_burst);
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
    /// `sh` = 로컬 소스 슬롯(`bank.slots[LOCAL_SOURCE]`) — 뱅크 전체가 아니라 그 하나만 받는다(피어 슬롯을 모른다).
    pub(super) fn service(&mut self, info: &StreamInfo, counters: &AudioCounters, controls: &MixControls, sh: &SourceShared, stats: &WorkerStats, epoch: Instant) {
        let mon = if controls.monitor_enabled.load(Ordering::Relaxed) { controls.monitor_gain.load() } else { 0.0 };
        sh.set_gains((mon, mon), (1.0, 1.0));
        let chunk_dev = chunk_dev(info);
        // 여유 판단과 목표 수위는 같은 출력 버스트 값을 쓴다(한 번 읽음). 아래 상한(`max_chunks`)은 전처럼 따로 읽는다.
        let out_burst = out_burst_dev(info, counters);
        self.adapt_safety(sh.underrun_frames.load(Ordering::Relaxed), out_burst, epoch.elapsed(), chunk_dev);
        let target = out_burst + chunk_dev + self.safety;
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
    pub(super) fn track_occupancy(&mut self, out_rate: u32, stats: &WorkerStats, pending48: usize) {
        let in48 = (pending48 + self.local.frames()) as f64;
        let local_dev = (self.prod.occupied_len() / 2 + self.local_rs.available()) as f64;
        let out_us = local_dev * 1e6 / out_rate.max(1) as f64;
        let mon_us = in48 * 1e6 / INTERNAL_RATE as f64 + out_us;
        self.monitor_ewma_us += (mon_us - self.monitor_ewma_us) * 0.01;
        stats.monitor_occupancy_us.store(self.monitor_ewma_us as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::device::BackendKind;
    use crate::engine::rt::{output_path, LOCAL_SOURCE};
    use std::sync::Arc;

    fn info() -> StreamInfo {
        StreamInfo {
            in_rate: 48_000,
            out_rate: 48_000,
            in_channels: 2,
            out_channels: 2,
            buffer_frames: 256,
            in_backend: BackendKind::Fake,
            out_backend: BackendKind::Fake,
            in_period_frames: 256,
            out_period_frames: 256,
            in_reported_latency_frames: 0,
            out_reported_latency_frames: 0,
            in_device_tag: 0,
            out_device_tag: 0,
        }
    }

    /// 로컬 여유는 로컬 슬롯 언더런에만 반응한다: 전역 `xruns`(피어 소스 부족·입력 오버런)가 올라도 그대로다.
    #[test]
    fn safety_grows_only_on_local_slot_underrun() {
        let counters = Arc::new(AudioCounters::default());
        let controls = Arc::new(MixControls::default());
        let (_rt, mut feed) = output_path(counters.clone(), controls.clone());
        let stats = WorkerStats::default();
        let prod = feed.prods.remove(LOCAL_SOURCE);
        let mut lp = LocalPath::new(2, 48_000, prod, ResampleQuality::Auto).unwrap();
        let (info, epoch) = (info(), Instant::now());
        let sh = &feed.bank.slots[LOCAL_SOURCE];
        lp.service(&info, &counters, &controls, sh, &stats, epoch);
        assert_eq!(lp.safety, 0);
        // 피어 소스 부족·입력 오버런: 전역 xruns만 오른다
        counters.xruns.fetch_add(3, Ordering::Relaxed);
        lp.service(&info, &counters, &controls, sh, &stats, epoch);
        assert_eq!(lp.safety, 0, "로컬과 무관한 xrun에 모니터 여유가 늘면 안 됨");
        // 로컬 슬롯 언더런: 청크 하나 늘린다(전역 xruns도 함께 오른다 — 콜백이 그렇게 센다)
        sh.underrun_frames.fetch_add(40, Ordering::Relaxed);
        counters.xruns.fetch_add(1, Ordering::Relaxed);
        lp.service(&info, &counters, &controls, sh, &stats, epoch);
        assert_eq!(lp.safety, chunk_dev(&info));
        // 같은 누적값이면 더 늘지 않는다
        lp.service(&info, &counters, &controls, sh, &stats, epoch);
        assert_eq!(lp.safety, chunk_dev(&info));
    }
}
