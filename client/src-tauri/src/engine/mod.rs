//! 엔진: 시작/정지, 스레드 소유, 공개 API (설계 문서 1절).
//!
//! 스레드 구성: 오디오 스레드(cpal 스트림 소유, 콜백은 cpal이 RT 스레드에서 호출),
//! 워커 1개, 네트워크 수신/제어 스레드, (구독 시) 믹스 탭 스레드.
//! `Engine::stop`(또는 drop)은 모든 스레드를 멈추고 join한다.

pub mod codec;
pub mod config;
pub mod device;
pub mod jitter;
pub mod mixer;
pub mod resample;
pub mod rt;
pub mod stats;
pub mod worker;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::HeapRb;

use crate::net::peer::LocalDescriptor;
use crate::net::{Net, NetOptions};
use config::{EngineConfig, INTERNAL_RATE};
use device::{AudioBackend, StreamInfo};
use jitter::JitterSettings;
use mixer::{clamp_gain, MixControls};
use rt::{InputRt, OutputRt};
use stats::{AudioCounters, EngineStats, PeerStats};
use worker::{MixTap, WorkerParams};

/// 믹스 청크 콜백: 20ms(960프레임) 48kHz 스테레오 f32 LE 인터리브 바이트.
pub type MixCallback = Box<dyn FnMut(Vec<u8>) + Send>;

/// 믹스 탭 청크 크기(프레임) = 20ms.
pub const MIX_CHUNK_FRAMES: usize = 960;

struct TapThread {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

pub struct Engine {
    cfg: EngineConfig,
    info: StreamInfo,
    counters: Arc<AudioCounters>,
    controls: Arc<MixControls>,
    out_fill: Arc<AtomicU64>,
    worker_stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    audio_stop: Arc<AtomicBool>,
    audio: Option<JoinHandle<()>>,
    net: Option<Net>,
    tap: Arc<MixTap>,
    tap_thread: Option<TapThread>,
}

impl Engine {
    /// 엔진 시작. `network`가 `Some`이면 (peer_id, 옵션)으로 네트워크도 연다.
    pub fn start(
        cfg: EngineConfig,
        backend: Arc<dyn AudioBackend>,
        controls: Arc<MixControls>,
        network: Option<(String, NetOptions)>,
    ) -> Result<Engine, String> {
        cfg.validate()?;
        let counters = Arc::new(AudioCounters::default());
        let (in_prod, in_cons) = rt::input_ring();
        let (out_prod, out_cons) = rt::output_ring();
        let input = InputRt::new(in_prod, counters.clone(), &cfg.input_channels, cfg.channels as usize);
        let output = OutputRt::new(out_cons, counters.clone());

        // 오디오 스레드: 스트림을 열고 정지 신호까지 소유한다(cpal::Stream은 !Send).
        let audio_stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Result<StreamInfo, String>>();
        let a_stop = audio_stop.clone();
        let a_cfg = cfg.clone();
        let a_counters = counters.clone();
        let audio = std::thread::Builder::new()
            .name("band-audio".into())
            .spawn(move || {
                let guard = match backend.open(&a_cfg, input, output, a_counters) {
                    Ok((info, guard)) => {
                        let _ = tx.send(Ok(info));
                        guard
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                while !a_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                drop(guard);
            })
            .map_err(|e| format!("오디오 스레드 생성 실패: {e}"))?;

        let info = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                let _ = audio.join();
                return Err(e);
            }
            Err(_) => {
                audio_stop.store(true, Ordering::SeqCst);
                // 장치가 응답하지 않으면 join이 막힐 수 있으므로 분리한다.
                return Err("오디오 장치 열기 시간 초과".into());
            }
        };

        let mut engine = Engine {
            cfg: cfg.clone(),
            info,
            counters: counters.clone(),
            controls: controls.clone(),
            out_fill: Arc::new(AtomicU64::new(0)),
            worker_stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            audio_stop,
            audio: Some(audio),
            net: None,
            tap: Arc::new(MixTap::default()),
            tap_thread: None,
        };

        if let Some((peer_id, opts)) = network {
            let mut opts = opts;
            if let Some(p) = cfg.udp_port {
                opts.port = p;
            }
            let js = JitterSettings {
                request_frames: cfg.frame_samples(),
                min_ms: cfg.jitter_min_ms(),
                max_ms: cfg.jitter_max_ms(),
            };
            engine.net = Some(Net::start(peer_id, &opts, js)?); // 실패 시 engine drop이 오디오 정리
        }

        let params = WorkerParams {
            cfg,
            info,
            in_cons,
            out_prod,
            counters,
            controls,
            net: engine.net.as_ref().map(|n| n.shared().clone()),
            tap: engine.tap.clone(),
            stop: engine.worker_stop.clone(),
            out_fill: engine.out_fill.clone(),
        };
        engine.worker = Some(
            std::thread::Builder::new()
                .name("band-worker".into())
                .spawn(move || worker::run(params))
                .map_err(|e| format!("워커 스레드 생성 실패: {e}"))?,
        );
        log::info!("엔진 시작: {:?}", engine.info);
        Ok(engine)
    }

    pub fn descriptor(&self) -> Option<&LocalDescriptor> {
        self.net.as_ref().map(|n| &n.descriptor)
    }

    pub fn local_port(&self) -> Option<u16> {
        self.net.as_ref().map(|n| n.local_port())
    }

    pub fn has_network(&self) -> bool {
        self.net.is_some()
    }

    pub fn add_peer(&self, peer_id: String, desc: &LocalDescriptor) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.add_peer(peer_id, desc)
    }

    pub fn remove_peer(&self, peer_id: &str) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        net.remove_peer(peer_id);
        Ok(())
    }

    pub fn set_peer_gain(&self, peer_id: &str, gain: f32, muted: bool) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        let peer = net.peers().into_iter().find(|p| p.id == peer_id).ok_or("알 수 없는 피어")?;
        peer.mix.gain.store(clamp_gain(gain));
        peer.mix.muted.store(muted, Ordering::Relaxed);
        Ok(())
    }

    pub fn controls(&self) -> &Arc<MixControls> {
        &self.controls
    }

    pub fn stats(&self) -> EngineStats {
        let c = &self.counters;
        let in_buf = match c.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_buf = match c.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_ring_frames = self.out_fill.load(Ordering::Relaxed) as f32 / 2.0;
        let est = in_buf as f32 / self.info.in_rate as f32 * 1000.0
            + self.cfg.frame_samples() as f32 / INTERNAL_RATE as f32 * 1000.0
            + out_ring_frames / self.info.out_rate as f32 * 1000.0
            + out_buf as f32 / self.info.out_rate as f32 * 1000.0;
        let peers = self
            .net
            .as_ref()
            .map(|n| {
                n.peers()
                    .iter()
                    .map(|p| {
                        let conn = p.conn.lock().clone();
                        let js = p.jitter.lock().stats();
                        PeerStats {
                            peer_id: p.id.clone(),
                            state: conn.state,
                            path: conn.path.map(|(_, k)| k),
                            rtt_ms: conn.rtt_ms,
                            jitter_ms: js.jitter_ms,
                            buffer_ms: js.buffer_ms,
                            loss_pct: js.loss_pct,
                            late: js.late,
                            concealed: js.concealed,
                            level: p.mix.level.load(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        EngineStats {
            running: true,
            sample_rate: self.info.out_rate,
            device_buffer: out_buf,
            input_level: c.input_level.load(),
            output_level: c.output_level.load(),
            xruns: c.xruns.load(Ordering::Relaxed),
            est_latency_ms: est,
            peers,
        }
    }

    /// 상세 지터 통계(테스트/진단용).
    pub fn peer_jitter_stats(&self) -> Vec<(String, jitter::JitterStats)> {
        self.net
            .as_ref()
            .map(|n| n.peers().iter().map(|p| (p.id.clone(), p.jitter.lock().stats())).collect())
            .unwrap_or_default()
    }

    /// 믹스 구독. 20ms마다 콜백 호출. 기존 구독은 교체된다.
    pub fn subscribe_mix(&mut self, mut cb: MixCallback) -> Result<(), String> {
        self.unsubscribe_mix();
        let (prod, mut cons) = HeapRb::<f32>::new(MIX_CHUNK_FRAMES * 2 * 25).split();
        *self.tap.prod.lock() = Some(prod);
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let handle = std::thread::Builder::new()
            .name("band-mix-tap".into())
            .spawn(move || {
                let mut chunk = vec![0f32; MIX_CHUNK_FRAMES * 2];
                while !st.load(Ordering::Relaxed) {
                    while cons.occupied_len() >= chunk.len() {
                        let n = cons.pop_slice(&mut chunk);
                        let mut bytes = Vec::with_capacity(n * 4);
                        for s in &chunk[..n] {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        cb(bytes);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
            .map_err(|e| format!("믹스 탭 스레드 생성 실패: {e}"))?;
        self.tap_thread = Some(TapThread { stop, handle });
        Ok(())
    }

    pub fn unsubscribe_mix(&mut self) {
        *self.tap.prod.lock() = None;
        if let Some(t) = self.tap_thread.take() {
            t.stop.store(true, Ordering::SeqCst);
            if t.handle.join().is_err() {
                log::error!("믹스 탭 스레드 패닉");
            }
        }
    }

    /// 모든 스레드를 멈추고 join한다. 여러 번 불러도 안전하다.
    pub fn stop(&mut self) {
        self.unsubscribe_mix();
        self.worker_stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.worker.take() {
            if w.join().is_err() {
                log::error!("워커 스레드 패닉");
            }
        }
        if let Some(mut n) = self.net.take() {
            n.stop();
        }
        self.audio_stop.store(true, Ordering::SeqCst);
        if let Some(a) = self.audio.take() {
            if a.join().is_err() {
                log::error!("오디오 스레드 패닉");
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}
