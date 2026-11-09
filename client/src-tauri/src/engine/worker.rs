//! 엔진 워커(비RT 스레드 1개, 설계 문서 1.1절).
//!
//! - 입력 링 → (리샘플) → 프레임 단위 인코드 → 송신(한 번 암호화해 모든 피어에게)
//! - 지터 버퍼에서 피어 프레임을 꺼내 믹스 → (리샘플) → 출력 링을 목표 수위로 유지
//!
//! 출력 링 수위는 "워커가 넣은 누적 샘플 - 출력 콜백이 소비한 누적 샘플(원자 카운터)"로 계산한다.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Producer};
use ringbuf::{HeapCons, HeapProd};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, EngineConfig, INTERNAL_RATE, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use super::device::StreamInfo;
use super::mixer::{self, MixControls};
use super::resample::StreamResampler;
use super::stats::AudioCounters;
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::NetShared;

/// 워커 폴링 주기. Windows에서도 std::thread::sleep은 고해상도 타이머를 쓴다.
const POLL: Duration = Duration::from_micros(500);

/// 믹스 탭(관람자 브릿지용) 공유 지점. 워커가 48k 스테레오 믹스를 밀어 넣는다.
#[derive(Default)]
pub struct MixTap {
    pub prod: Mutex<Option<HeapProd<f32>>>,
}

pub struct WorkerParams {
    pub cfg: EngineConfig,
    pub info: StreamInfo,
    pub in_cons: HeapCons<f32>,
    pub out_prod: HeapProd<f32>,
    pub counters: Arc<AudioCounters>,
    pub controls: Arc<MixControls>,
    pub net: Option<Arc<NetShared>>,
    pub tap: Arc<MixTap>,
    pub stop: Arc<AtomicBool>,
    /// 출력 링 수위(샘플) 공유값 — 통계용.
    pub out_fill: Arc<std::sync::atomic::AtomicU64>,
}

struct Worker {
    p: WorkerParams,
    frame: usize,
    ich: usize,
    in_rs: StreamResampler,
    out_rs: StreamResampler,
    encoder: Option<Encoder>,
    flags: u8,
    frame_code: u8,
    ts: u32,
    produced_out: u64,
    monitor_fifo: VecDeque<f32>,
    in_tmp: Vec<f32>,
    frame_buf: Vec<f32>,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    local: Vec<f32>,
    bus: Vec<f32>,
    tap_bus: Vec<f32>,
    peer_buf: Vec<f32>,
    out_tmp: Vec<f32>,
    reported_errors: u32,
}

pub fn run(p: WorkerParams) {
    let mut w = match Worker::new(p) {
        Ok(w) => w,
        Err(e) => {
            log::error!("워커 초기화 실패: {e}");
            return;
        }
    };
    w.run();
}

impl Worker {
    fn new(p: WorkerParams) -> Result<Self, String> {
        let frame = p.cfg.frame_samples();
        let ich = p.cfg.channels as usize;
        let in_rs = StreamResampler::new(p.info.in_rate, INTERNAL_RATE, ich)?;
        let out_rs = StreamResampler::new(INTERNAL_RATE, p.info.out_rate, 2)?;
        let encoder = if p.net.is_some() {
            Some(Encoder::new(p.cfg.codec, ich, p.cfg.opus_bitrate())?)
        } else {
            None
        };
        let mut flags = 0;
        if encoder.as_ref().is_some_and(|e| e.is_opus()) {
            flags |= FLAG_OPUS;
        }
        if ich == 2 {
            flags |= FLAG_STEREO;
        }
        let frame_code = frame_code_from_samples(frame).ok_or("잘못된 프레임 크기")?;
        Ok(Self {
            frame,
            ich,
            in_rs,
            out_rs,
            encoder,
            flags,
            frame_code,
            ts: rand::random(),
            produced_out: 0,
            monitor_fifo: VecDeque::with_capacity(frame * 2 * 8),
            in_tmp: vec![0.0; 4096],
            frame_buf: vec![0.0; frame * ich],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            local: vec![0.0; frame * 2],
            bus: vec![0.0; frame * 2],
            tap_bus: vec![0.0; frame * 2],
            peer_buf: vec![0.0; frame * 2],
            out_tmp: vec![0.0; 8192],
            reported_errors: 0,
            p,
        })
    }

    fn run(&mut self) {
        log::info!("워커 시작: frame {} ch {}", self.frame, self.ich);
        while !self.p.stop.load(Ordering::Relaxed) {
            self.pump_input();
            self.pump_output();
            let errs = self.p.counters.stream_errors.load(Ordering::Relaxed);
            if errs != self.reported_errors {
                log::warn!("오디오 스트림 에러 {}회 발생(장치 분리 등)", errs);
                self.reported_errors = errs;
            }
            std::thread::sleep(POLL);
        }
        log::info!("워커 종료");
    }

    fn pump_input(&mut self) {
        loop {
            let n = self.p.in_cons.pop_slice(&mut self.in_tmp);
            if n == 0 {
                break;
            }
            let n = n - n % self.ich;
            self.in_rs.push(&self.in_tmp[..n]);
        }
        while self.in_rs.available() >= self.frame {
            let mut fb = std::mem::take(&mut self.frame_buf);
            self.in_rs.pop(&mut fb);
            self.on_capture_frame(&fb);
            self.frame_buf = fb;
        }
    }

    fn on_capture_frame(&mut self, frame: &[f32]) {
        if let (Some(enc), Some(net)) = (self.encoder.as_mut(), self.p.net.as_ref()) {
            match enc.encode(frame, &mut self.payload) {
                Ok(n) => net.send_audio(&self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt),
                Err(e) => log::warn!("인코드 실패: {e}"),
            }
        }
        self.ts = self.ts.wrapping_add(self.frame as u32);
        // 모니터/탭용 로컬 FIFO(스테레오). 입력·출력 클럭 차이로 쌓이면 오래된 것부터 버림.
        if self.ich == 2 {
            self.monitor_fifo.extend(frame.iter().copied());
        } else {
            for &s in frame {
                self.monitor_fifo.push_back(s);
                self.monitor_fifo.push_back(s);
            }
        }
        let max = self.frame * 2 * 3;
        if self.monitor_fifo.len() > max {
            let excess = self.monitor_fifo.len() - max;
            self.monitor_fifo.drain(..excess);
        }
    }

    fn device_out_buffer(&self) -> u64 {
        let measured = self.p.counters.out_cb_frames.load(Ordering::Relaxed);
        if measured > 0 {
            measured as u64
        } else {
            self.p.info.buffer_frames as u64
        }
    }

    fn pump_output(&mut self) {
        // 목표 수위 = 장치 버퍼 1개 + 프레임 1개 (장치 레이트, 스테레오 샘플)
        let frame_dev = (self.frame as u64 * self.p.info.out_rate as u64).div_ceil(INTERNAL_RATE as u64);
        let target = (self.device_out_buffer() + frame_dev) * 2;
        for _ in 0..16 {
            let consumed = self.p.counters.out_consumed.load(Ordering::Relaxed);
            let fill = self.produced_out.saturating_sub(consumed);
            self.p.out_fill.store(fill, Ordering::Relaxed);
            if fill >= target {
                break;
            }
            self.produce_output_frame();
        }
    }

    fn produce_output_frame(&mut self) {
        let n2 = self.frame * 2;
        self.bus.fill(0.0);
        self.tap_bus.fill(0.0);

        // 로컬 입력
        if self.monitor_fifo.len() >= n2 {
            for (o, s) in self.local.iter_mut().zip(self.monitor_fifo.drain(..n2)) {
                *o = s;
            }
        } else {
            self.local.fill(0.0);
        }
        let ctl = &self.p.controls;
        if ctl.monitor_enabled.load(Ordering::Relaxed) {
            mixer::add_stereo(&mut self.bus, &self.local, ctl.monitor_gain.load());
        }
        mixer::add_stereo(&mut self.tap_bus, &self.local, 1.0);

        // 피어
        if let Some(net) = self.p.net.as_ref() {
            let now_us = net.now_us();
            let bus = &mut self.bus;
            let tap = &mut self.tap_bus;
            let pb = &mut self.peer_buf;
            net.for_each_peer(|peer| {
                peer.jitter.lock().pop(pb, now_us);
                let g = peer.mix.effective_gain();
                peer.mix.level.store(mixer::peak(pb));
                mixer::add_stereo(bus, pb, g);
                mixer::add_stereo(tap, pb, g);
            });
        }

        mixer::scale(&mut self.bus, ctl.master_gain.load());
        mixer::soft_clip_buf(&mut self.bus);

        // 믹스 탭
        if let Some(mut guard) = self.p.tap.prod.try_lock() {
            if let Some(prod) = guard.as_mut() {
                mixer::soft_clip_buf(&mut self.tap_bus);
                let _ = prod.push_slice(&self.tap_bus);
            }
        }

        self.out_rs.push(&self.bus);
        loop {
            let frames = self.out_rs.pop(&mut self.out_tmp);
            if frames == 0 {
                break;
            }
            let pushed = self.p.out_prod.push_slice(&self.out_tmp[..frames * 2]);
            self.produced_out += pushed as u64;
        }
    }
}
