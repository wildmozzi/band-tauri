//! 엔진: 시작/정지, 스레드 소유, 공개 API (설계 문서 1절).
//!
//! 스레드 구성: 오디오 스레드(cpal 스트림 소유, 콜백은 cpal이 RT 스레드에서 호출),
//! 워커 1개, 네트워크 수신/제어 스레드, (구독 시) 믹스 탭 스레드.
//! `Engine::stop`(또는 drop)은 모든 스레드를 멈추고 join한다.

pub mod codec;
pub mod config;
pub mod cpuprof;
pub mod device;
pub mod flac;
pub mod jitter;
#[cfg(test)]
mod loss_codec_sim;
pub mod measure;
pub mod metronome;
pub mod mixer;
pub mod prio;
pub mod record;
pub mod relay_codec;
pub mod resample;
pub mod rt;
pub mod stats;
#[cfg(windows)]
pub mod wasapi;
pub mod wav;
pub mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

use crate::net::peer::LocalDescriptor;
use crate::net::{Net, NetOptions};
use config::{EngineConfig, INTERNAL_RATE};
use device::{AudioBackend, StreamInfo};
use jitter::JitterSettings;
use measure::{EchoJob, EchoProbe, LoopbackJob};
use mixer::{clamp_gain, MixControls};
use resample::StreamResampler;
use rt::InputRt;
use stats::{AudioCounters, EngineStats, PeerStats};
use worker::{Worker, WorkerParams, WorkerStats};

/// 믹스 청크 콜백: 20ms(960프레임) 48kHz 스테레오 f32 LE 인터리브 바이트.
pub type MixCallback = Box<dyn FnMut(Vec<u8>) + Send>;

/// 믹스 탭 청크 크기(프레임) = 20ms.
pub const MIX_CHUNK_FRAMES: usize = 960;

struct TapThread {
    stop: Arc<AtomicBool>,
    /// 끝나면 탭 링 소비자를 돌려준다(다음 구독에서 다시 씀).
    handle: JoinHandle<HeapCons<f32>>,
}

pub struct Engine {
    cfg: EngineConfig,
    info: StreamInfo,
    counters: Arc<AudioCounters>,
    controls: Arc<MixControls>,
    worker_stats: Arc<WorkerStats>,
    worker_stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    audio_stop: Arc<AtomicBool>,
    audio: Option<JoinHandle<()>>,
    net: Option<Net>,
    /// 출력 콜백이 장치 레이트로 채우는 믹스 탭 링의 소비자. 구독 중에는 탭 스레드가 가진다.
    tap_cons: Mutex<Option<HeapCons<f32>>>,
    tap_thread: Option<TapThread>,
    /// 지연 측정(P1-1): RT 끝점 제어와 측정 스레드 쪽 링 끝점(측정 중에는 비어 있음).
    measure_sh: Arc<rt::MeasureShared>,
    measure_ends: Arc<Mutex<Option<rt::MeasureEnds>>>,
    /// 에코 측정 상태(워커와 공유).
    probe: Arc<Mutex<Option<EchoProbe>>>,
    /// 엔진 수명 동안 쥐는 1ms 타이머 해상도(P1-2). `stop`에서 놓는다.
    timer: Option<prio::TimerResolution>,
    /// 공유 메트로놈(P2-3): 계획·상태(워커와 공유).
    metronome: Arc<metronome::MetronomeCtl>,
    /// 엔진 시계 기준(Net epoch, 없으면 시작 시각). 메트로놈 `startUs`의 기준이다.
    epoch: Instant,
    /// CPU 통계 창 회전(P1-10). `stats`가 2초마다 RT 콜백 히스토그램을 비우고 워커 CPU 증분을 잰다.
    cpu: Mutex<cpuprof::CpuSampler>,
    /// 합주 녹음(P2-4). 정지(파일 마무리)는 엔진 락 밖에서 할 수 있게 따로 잠근다.
    recorder: Arc<Mutex<record::Recorder>>,
}

impl Engine {
    /// 엔진 시작. `network`가 `Some`이면 (peer_id, 옵션)으로 네트워크도 연다.
    pub fn start(
        cfg: EngineConfig,
        backend: Arc<dyn AudioBackend>,
        controls: Arc<MixControls>,
        network: Option<(String, NetOptions)>,
    ) -> Result<Engine, String> {
        Self::start_cancellable(cfg, backend, controls, network, None)
    }

    /// `cancel`이 켜지면 단계 사이(장치 열기 후, STUN 중)에서 일찍 포기한다. 이미 연 장치는 닫고 돌아온다.
    pub fn start_cancellable(
        cfg: EngineConfig,
        backend: Arc<dyn AudioBackend>,
        controls: Arc<MixControls>,
        network: Option<(String, NetOptions)>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<Engine, String> {
        let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
        cfg.validate()?;
        // 시작이 중간에 실패하면 drop으로 놓인다(Engine으로 옮긴 뒤에는 stop이 놓는다).
        let timer = prio::TimerResolution::acquire();
        let counters = Arc::new(AudioCounters::default());
        let (in_prod, in_cons) = rt::input_ring();
        let mut input = InputRt::new(in_prod, counters.clone(), &cfg.input_channels, cfg.channels as usize);
        // 콜백 구동 믹스: 출력 콜백이 소스 링들에서 직접 꺼내 섞는다(P1-0).
        let (mut output, feed) = rt::output_path(counters.clone(), controls.clone());
        // 지연 측정 끝점(링은 여기서 미리 할당, 콜백은 링 push/pop과 원자값만 쓴다)
        let (measure_sh, m_in, m_out, measure_ends) = rt::measure_path();
        input.attach_measure(m_in);
        output.attach_measure(m_out);
        let metronome = Arc::new(metronome::MetronomeCtl::default());
        output.attach_click(metronome::ClickRt::new(metronome.click.clone()));
        // 녹음 핸드오프(링은 녹음 시작 때 할당해 넘긴다)
        let (rec_rt, rec_ctl) = record::record_port();
        output.attach_record(rec_rt);
        let probe = Arc::new(Mutex::new(None));

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
            worker_stats: Arc::new(WorkerStats::default()),
            worker_stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            audio_stop,
            audio: Some(audio),
            net: None,
            tap_cons: Mutex::new(Some(feed.tap)),
            tap_thread: None,
            measure_sh,
            measure_ends: Arc::new(Mutex::new(Some(measure_ends))),
            probe: probe.clone(),
            timer: Some(timer),
            metronome: metronome.clone(),
            epoch: Instant::now(),
            cpu: Mutex::new(cpuprof::CpuSampler::new(Instant::now(), cpuprof::tsc_now())),
            recorder: Arc::new(Mutex::new(record::Recorder::new(rec_ctl, info.out_rate))),
        };
        // 48kHz 클릭 파형을 장치 레이트로 읽는 보폭
        metronome.click.step.store(INTERNAL_RATE as f32 / info.out_rate.max(1) as f32);

        if cancelled() {
            return Err("엔진 시작이 취소되었습니다".into()); // engine drop이 오디오 정리
        }
        if let Some((peer_id, opts)) = network {
            let mut opts = opts;
            if let Some(p) = cfg.udp_port {
                opts.port = p;
            }
            if opts.cancel.is_none() {
                opts.cancel = cancel.clone();
            }
            let js = JitterSettings { min_ms: cfg.jitter_min_ms(), max_ms: cfg.jitter_max_ms(), profile: cfg.jitter_profile() };
            engine.net = Some(Net::start(peer_id, &opts, js)?); // 실패 시 engine drop이 오디오 정리
        }
        if cancelled() {
            return Err("엔진 시작이 취소되었습니다".into());
        }

        let epoch = engine.net.as_ref().map(|n| n.shared().epoch()).unwrap_or(engine.epoch);
        engine.epoch = epoch;
        let params = WorkerParams {
            cfg,
            info,
            in_cons,
            prods: feed.prods,
            bank: feed.bank,
            counters,
            controls,
            net: engine.net.as_ref().map(|n| n.shared().clone()),
            stop: engine.worker_stop.clone(),
            stats: engine.worker_stats.clone(),
            epoch,
            probe,
            metronome,
            rec_marks: engine.recorder.lock().marks(),
        };
        // 초기화(리샘플러/인코더)는 여기서 해서 실패를 호출자에게 돌려준다(리뷰 M10).
        let worker = Worker::new(params)?;
        engine.worker = Some(
            std::thread::Builder::new()
                .name("band-worker".into())
                .spawn(move || worker.run())
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

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub fn has_network(&self) -> bool {
        self.net.is_some()
    }

    pub fn add_peer(&self, peer_id: String, desc: &LocalDescriptor) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.add_peer(peer_id, desc)
    }

    /// 릴레이 설정/토큰 갱신/해제(설계 4절 `engine_set_relay`, relay-design.md).
    pub fn set_relay(&self, relay: Option<crate::net::RelaySetting<'_>>) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.set_relay(relay)
    }

    /// 지금 릴레이가 `host`(host:port 문자열)로 설정돼 있으면 그때 해석한 주소(DNS를 다시 풀지 않는다, M3).
    pub fn relay_addr_for_host(&self, host: &str) -> Option<std::net::SocketAddr> {
        self.net.as_ref().and_then(|n| n.relay_addr_for_host(host))
    }

    pub fn remove_peer(&self, peer_id: &str) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        net.remove_peer(peer_id);
        Ok(())
    }

    pub fn set_peer_gain(&self, peer_id: &str, gain: f32, muted: bool, pan: Option<f32>) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        let peer = net.peers().into_iter().find(|p| p.id == peer_id).ok_or("알 수 없는 피어")?;
        peer.mix.gain.store(clamp_gain(gain));
        peer.mix.muted.store(muted, Ordering::Relaxed);
        peer.mix.pan.store(mixer::clamp_pan(pan.unwrap_or(0.0)));
        Ok(())
    }

    /// 지터 버퍼 '저지연(0) ↔ 안정(1)' 프로필을 실행 중에 바꾼다(모든 피어 + 이후 추가될 피어, P1-5).
    pub fn set_jitter_profile(&self, profile: f32) -> Result<(), String> {
        if !profile.is_finite() || !(0.0..=1.0).contains(&profile) {
            return Err(format!("jitterProfile은 0..1 범위여야 합니다: {profile}"));
        }
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        net.set_jitter_profile(profile);
        Ok(())
    }

    pub fn controls(&self) -> &Arc<MixControls> {
        &self.controls
    }

    /// 엔진 시계(µs). 메트로놈 `startUs`와 Pong 시계 표본의 기준.
    pub fn now_us(&self) -> f64 {
        self.epoch.elapsed().as_micros() as f64
    }

    /// 공유 메트로놈 계획 적용(설계 4절 `engine_set_metronome`). 적용된 계획을 돌려준다.
    pub fn set_metronome(&self, c: metronome::MetronomeConfig) -> Result<metronome::MetronomePlan, String> {
        if c.ref_peer_id.as_deref().is_some_and(|s| !s.is_empty()) && self.net.is_none() {
            return Err("네트워크 없이 시작된 엔진은 다른 피어를 따를 수 없습니다".into());
        }
        let plan = metronome::MetronomePlan::from_config(c, self.now_us())?;
        self.metronome.set_plan(plan.clone());
        Ok(plan)
    }

    /// 루프백 보정값(`offsetMs`)으로 메트로놈 클릭의 숨은 출력 지연을 보정한다(PR #34 M2(a)). None이면 해제.
    /// 드라이버가 출력 지연을 보고하는 장치에서는 드라이버 값이 우선이다(`MetronomeStats.outputLatencySource`).
    pub fn set_loopback_offset(&self, offset_ms: Option<f64>) -> Result<(), String> {
        self.metronome.set_loopback_offset(offset_ms)
    }

    /// 숨은 지연 보정값이 유효한 장치 구성 키(설계 1.4절). 숨은 지연(실측 − 콜백 크기)에 영향을 주는 값을 모두 넣는다:
    /// 드라이버·실제 백엔드(자체 WASAPI/cpal 폴백)·실제로 연 장치(기본 장치면 그때 고른 장치)·요청 버퍼 크기·in/out 레이트.
    /// 버퍼를 바꾸면 엔드포인트 버퍼·드라이버 여유가 달라질 수 있어 다시 재야 한다(PR #57 M1).
    pub fn device_key(&self) -> String {
        let (c, i) = (&self.cfg, &self.info);
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:016x}|{:016x}|{}|{}|{}",
            c.input_driver,
            c.output_driver,
            i.in_backend,
            i.out_backend,
            i.in_device_tag,
            i.out_device_tag,
            c.buffer_size,
            i.in_rate,
            i.out_rate
        )
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
        let in_ms = in_buf as f32 / self.info.in_rate as f32 * 1000.0;
        let out_ms = out_buf as f32 / self.info.out_rate as f32 * 1000.0;
        let frame_ms = self.cfg.frame_samples() as f32 / config::INTERNAL_RATE as f32 * 1000.0;
        let ws = &self.worker_stats;
        // GOAL 1절 정의: 입력 장치 버퍼 + 프레임 + 출력 링 + 장치 출력 버퍼(네트워크 경로의 로컬 몫, 첫 샘플 기준).
        // 콜백 구동 믹스(P1-0)에는 믹스된 출력 링이 없다. 출력 콜백이 피어 소스 링에서 바로 꺼내고,
        // 소스 링에 머무는 시간은 지터 버퍼 깊이(`PeerStats.bufferMs`)에 포함되어 목표로 제어된다.
        let est = in_ms + frame_ms + out_ms;
        // 모니터 경로(내 소리 → 내 귀): 입력 버퍼 + 로컬 FIFO·로컬 소스 링 점유량 시간평균 + 출력 버퍼(임펄스 실측으로 검증).
        let monitor = in_ms + ws.monitor_occupancy_us.load(Ordering::Relaxed) as f32 / 1000.0 + out_ms;
        let stream_errors = c.stream_errors.load(Ordering::Relaxed);
        let device_lost = ws.device_stalled.load(Ordering::Relaxed) || c.device_lost.load(Ordering::Relaxed);
        let stream_failed = c.stream_failed.load(Ordering::Relaxed);
        let period_frames = match self.info.out_period_frames {
            0 => out_buf,
            n => n,
        };
        let device_period_ms = period_frames as f32 / self.info.out_rate.max(1) as f32 * 1000.0;
        let worker_cycles = self.worker.as_ref().and_then(cpuprof::thread_cycles);
        let cpu = self.cpu.lock().sample(Instant::now(), &c.rt_timing, cpuprof::tsc_now(), worker_cycles, device_period_ms * 1000.0);
        let peers = self
            .net
            .as_ref()
            .map(|n| {
                n.peers()
                    .iter()
                    .map(|p| {
                        let conn = p.conn.lock().clone();
                        let js = p.jitter.lock().stats();
                        let (echo_requested, echo_active, peer_echoing) = {
                            let e = p.echo.lock();
                            let now = Instant::now();
                            (e.remote_requesting(now) && !e.enabled, e.reflecting(now), e.remote_echoing(now))
                        };
                        let (tx_codec, tx_codec_reason) = {
                            use crate::engine::relay_codec::CodecReason;
                            use crate::net::peer::{TX_CODEC_AUTO, TX_CODEC_AUTO_LOSS, TX_CODEC_OPUS, TX_CODEC_SENT};
                            let c = p.tx_codec.load(Ordering::Relaxed);
                            let kind = if c & TX_CODEC_OPUS != 0 { config::CodecKind::Opus } else { config::CodecKind::Pcm16 };
                            let reason = if c & TX_CODEC_AUTO != 0 {
                                Some(CodecReason::RelayLoad)
                            } else if c & TX_CODEC_AUTO_LOSS != 0 {
                                Some(CodecReason::LossFeedback)
                            } else {
                                None
                            };
                            ((c & TX_CODEC_SENT != 0).then_some(kind), reason)
                        };
                        let remote = (*p.remote_loss.lock())
                            .filter(|(_, at)| Instant::now().saturating_duration_since(*at) <= relay_codec::REPORT_STALE)
                            .map(|(r, _)| r);
                        PeerStats {
                            peer_id: p.id.clone(),
                            state: conn.state,
                            path: conn.path.map(|(_, k)| k),
                            rtt_ms: conn.rtt_ms,
                            jitter_ms: js.jitter_ms,
                            buffer_ms: js.buffer_ms,
                            target_ms: js.target_ms,
                            conceal_pct: js.conceal_pct,
                            corr_time_pct: js.corr_time_pct,
                            max_corr_cents: js.max_corr_cents,
                            loss_pct: js.loss_pct,
                            late: js.late,
                            concealed: js.concealed,
                            level: p.mix.level.load(),
                            // 벽시계로 잰 버퍼 지연(소스 링 포함) + 장치 출력 버퍼: 클럭 추정과 독립적인 실제 재생 지연
                            playout_ms: if js.wall_buffer_ms > 0.0 { js.wall_buffer_ms + out_ms } else { 0.0 },
                            echo_requested,
                            echo_active,
                            peer_echoing,
                            tx_codec,
                            tx_codec_auto: tx_codec_reason.is_some(),
                            tx_codec_reason,
                            remote_loss_pct: remote.map(|r| r.loss_pct),
                            remote_conceal_pct: remote.map(|r| r.conceal_pct),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        EngineStats {
            // 장치를 잃으면 false: 프론트의 기존 "엔진 정지" 감지(running=false 연속)가 그대로 동작한다(PR #5 M-2).
            running: !device_lost && !stream_failed,
            sample_rate: self.info.out_rate,
            device_buffer: out_buf,
            input_level: c.input_level.load(),
            output_level: c.output_level.load(),
            xruns: c.xruns.load(Ordering::Relaxed),
            est_latency_ms: est,
            monitor_latency_ms: monitor,
            stream_errors,
            device_lost,
            stream_failed,
            stream_restarts: c.stream_restarts.load(Ordering::Relaxed),
            input_discontinuities: c.input_discontinuities.load(Ordering::Relaxed),
            input_silent_packets: c.input_silent_packets.load(Ordering::Relaxed),
            device_period_ms,
            backend: self.info.backend_label(),
            thread_priority: prio::ThreadPriorityStats {
                worker: ws.thread_prio.load(),
                net_recv: self.net.as_ref().map(|n| n.shared().recv_prio.load()).unwrap_or(prio::ThreadPriority::Unmanaged),
                audio: c.audio_thread.load(),
                timer_1ms: self.timer.as_ref().is_some_and(|t| t.active()),
            },
            peers,
            relay: self.net.as_ref().and_then(|n| n.relay_stats()),
            qos: self.net.as_ref().map(|n| n.qos_stats()),
            send_queue: self.net.as_ref().map(|n| n.send_queue_stats()),
            metronome: self.metronome.stats(),
            cpu,
            // 녹음 락은 짧게만 잡힌다(정지의 기다리는 단계는 락 밖). 정지를 시작하면 null.
            recording: self.recorder.lock().stats(),
        }
    }

    /// 녹음 시작(P2-4): `dir` 아래 세션 폴더에 트랙별 WAV를 쓴다.
    pub fn record_start(&self, opts: record::RecordOptions, dir: &std::path::Path) -> Result<record::RecordStats, String> {
        let net = self.net.as_ref().map(|n| n.shared().clone());
        let resolve: record::PeerResolver = Box::new(move |ssrc| net.as_ref().and_then(|n| n.peer_id_for_ssrc(ssrc)));
        self.recorder.lock().start(opts, dir, resolve, crate::diag::now_ms())
    }

    /// 녹음 중 피어 트랙 이름 추가(늦게 들어온 연주자, 1.5절).
    pub fn record_set_track_names(&self, names: std::collections::HashMap<String, String>) -> Result<(), String> {
        self.recorder.lock().set_track_names(names)
    }

    /// 녹음 제어 핸들. 정지(콜백 반환 대기 + 파일 마무리, 최대 수 초)는 엔진 락 밖에서 이것으로 한다.
    pub fn recorder(&self) -> Arc<Mutex<record::Recorder>> {
        self.recorder.clone()
    }

    /// 하드웨어 루프백 측정 작업(설계 4절 `engine_measure_loopback`). 엔진 락 밖에서 `run`한다.
    pub fn loopback_job(&self) -> LoopbackJob {
        let s = self.stats();
        LoopbackJob {
            sh: self.measure_sh.clone(),
            ends: self.measure_ends.clone(),
            counters: self.counters.clone(),
            info: self.info,
            est_latency_ms: s.est_latency_ms,
            backend: s.backend,
        }
    }

    /// 에코 피어 측정 작업(측정하는 쪽, 설계 4절 `engine_measure_echo_peer`). 엔진 락 밖에서 `run`한다.
    pub fn echo_job(&self, peer_id: &str) -> Result<EchoJob, String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.shared().clone();
        let peer_ssrc = net.peer_ssrc(peer_id)?;
        let (in_ms, out_ms) = self.buffer_ms();
        Ok(EchoJob {
            probe: self.probe.clone(),
            net,
            peer_id: peer_id.to_string(),
            peer_ssrc,
            frame_ms: self.cfg.frame_samples() as f32 * 1000.0 / INTERNAL_RATE as f32,
            local_latency_ms: self.stats().est_latency_ms,
            input_buffer_ms: in_ms,
            output_buffer_ms: out_ms,
        })
    }

    /// 에코하는 쪽: 피어의 에코 요청에 동의/해제(설계 4절 `engine_set_echo_mode`).
    pub fn set_echo_mode(&self, peer_id: &str, enabled: bool) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.shared().set_echo_mode(peer_id, enabled)
    }

    /// (입력 버퍼, 출력 버퍼) ms — 실측 콜백 크기(아직 없으면 요청값).
    fn buffer_ms(&self) -> (f32, f32) {
        let c = &self.counters;
        let in_buf = match c.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_buf = match c.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        (in_buf as f32 / self.info.in_rate.max(1) as f32 * 1000.0, out_buf as f32 / self.info.out_rate.max(1) as f32 * 1000.0)
    }

    /// 열린 스트림 정보(백엔드, 레이트, 장치 주기).
    pub fn stream_info(&self) -> StreamInfo {
        self.info
    }

    /// 콜백 구동 믹스 진단: (마감 직전 은닉 횟수, 콜백 언더런으로 건너뛴 장치 프레임).
    pub fn feed_stats(&self) -> (u64, u64) {
        (
            self.worker_stats.deadline_fills.load(Ordering::Relaxed),
            self.worker_stats.skipped_frames.load(Ordering::Relaxed),
        )
    }

    /// 피어와의 키 확인용 안전 번호(SAS). 양쪽이 같은 값을 보면 시그널링 서버가 키를 바꿔치기하지 않은 것이다.
    pub fn safety_code(&self, peer_id: &str) -> Result<String, String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        net.safety_code(peer_id)
    }

    /// 로컬 입력 FIFO 보정/언더런 횟수(테스트/진단용).
    pub fn local_path_stats(&self) -> (u64, u64) {
        (
            self.worker_stats.local_corrections.load(Ordering::Relaxed),
            self.worker_stats.local_underruns.load(Ordering::Relaxed),
        )
    }

    /// 상세 지터 통계(테스트/진단용).
    pub fn peer_jitter_stats(&self) -> Vec<(String, jitter::JitterStats)> {
        self.net
            .as_ref()
            .map(|n| n.peers().iter().map(|p| (p.id.clone(), p.jitter.lock().stats())).collect())
            .unwrap_or_default()
    }

    /// 믹스 구독. 20ms마다 콜백 호출. 기존 구독은 교체된다.
    /// 출력 콜백은 장치 레이트로 탭 링을 채운다. 탭 스레드가 48kHz로 리샘플(장치가 48k면 그대로)해
    /// 20ms 청크로 보낸다.
    pub fn subscribe_mix(&mut self, mut cb: MixCallback) -> Result<(), String> {
        self.unsubscribe_mix();
        let mut cons = self.tap_cons.lock().take().ok_or("믹스 탭을 쓸 수 없습니다")?;
        let mut rs = match StreamResampler::new(self.info.out_rate, INTERNAL_RATE, 2) {
            Ok(r) => r,
            Err(e) => {
                *self.tap_cons.lock() = Some(cons);
                return Err(e);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let spawned = std::thread::Builder::new().name("band-mix-tap".into()).spawn(move || {
            // 구독 전 쌓인(가득 찬) 오래된 믹스는 버린다.
            cons.clear();
            let mut tmp = vec![0f32; 8192];
            let mut chunk = vec![0f32; MIX_CHUNK_FRAMES * 2];
            while !st.load(Ordering::Relaxed) {
                loop {
                    let n = cons.pop_slice(&mut tmp) & !1;
                    if n == 0 {
                        break;
                    }
                    rs.push(&tmp[..n]);
                }
                while rs.available() >= MIX_CHUNK_FRAMES {
                    let n = rs.pop(&mut chunk) * 2;
                    let mut bytes = Vec::with_capacity(n * 4);
                    for s in &chunk[..n] {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    cb(bytes);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            cons
        });
        let handle = spawned.map_err(|e| format!("믹스 탭 스레드 생성 실패: {e}"))?;
        self.tap_thread = Some(TapThread { stop, handle });
        Ok(())
    }

    pub fn unsubscribe_mix(&mut self) {
        if let Some(t) = self.tap_thread.take() {
            t.stop.store(true, Ordering::SeqCst);
            match t.handle.join() {
                Ok(cons) => *self.tap_cons.lock() = Some(cons),
                Err(_) => log::error!("믹스 탭 스레드 패닉"),
            }
        }
    }

    /// 모든 스레드를 멈추고 join한다. 여러 번 불러도 안전하다.
    pub fn stop(&mut self) {
        // 녹음은 출력 콜백이 도는 동안 닫는다(콜백이 녹음 끝점을 돌려줘야 남은 것을 다 쓴다)
        // 마무리(콜백 반환 대기 최대 1초 + 파일 sync)는 recorder 락 밖에서 한다 — 그동안 `stats()`가 막히지 않게(PR #47 L2).
        let (job, wait) = {
            let mut r = self.recorder.lock();
            (r.is_recording().then(|| r.begin_stop()), r.waiter())
        };
        if let Some(job) = job {
            if let Err(e) = job.and_then(|j| j.finish()) {
                log::warn!("엔진 정지 중 녹음 정지 실패: {e}");
            }
            self.recorder.lock().reclaim();
        }
        // `engine_record_stop`이 다른 스레드에서 마무리 중이면 끝날 때까지 기다린다(앱 종료 경합, PR #47 L4).
        if !wait(Duration::from_secs(10)) {
            log::warn!("엔진 정지: 녹음 마무리를 기다리다 시간 초과");
        }
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
        // 모든 스레드가 끝난 뒤 타이머 해상도를 놓는다. take로 여러 번 불러도 timeEndPeriod는 한 번이다.
        drop(self.timer.take());
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}
