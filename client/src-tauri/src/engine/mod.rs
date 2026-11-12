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
mod sim;
pub mod measure;
pub mod metronome;
pub mod mixer;
pub mod peer_media;
pub mod record;
pub mod relay_codec;
pub mod report;
pub mod resample;
pub mod rt;
pub mod rt_timing;
pub mod stats;
#[cfg(windows)]
pub mod wasapi;
pub mod wav;
pub mod worker;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

use crate::net::media_link::PeerKey;
use crate::net::peer::LocalDescriptor;
use crate::net::{Net, NetOptions};
use crate::sys::prio;
use config::{EngineConfig, INTERNAL_RATE};
use device::{AudioBackend, StreamInfo};
use jitter::JitterSettings;
use measure::{EchoJob, EchoProbe, LoopbackJob};
use mixer::{clamp_gain, MixControls};
use peer_media::{media_port, MediaCmd, MediaPort, MediaSettings, PeerMedia, PeerMediaShared};
use ringbuf::traits::Producer;
use resample::StreamResampler;
use rt::InputRt;
use stats::AudioCounters;
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
    /// 피어 미디어 등록부(엔진 ③). net 피어 표와 같은 peerId 집합·순서를 유지한다. 제어 평면 스레드만 잡는다.
    media: Mutex<Vec<Arc<PeerMediaShared>>>,
    /// `PeerKey` 발급 카운터(1부터, 단조 증가).
    next_key: AtomicU64,
    /// 새 피어의 지터 설정과 실행 중 프로필.
    media_settings: Arc<MediaSettings>,
    /// 워커 미디어 명령 큐(추가·제거)와 반환 큐의 제어 평면 쪽 끝(③-a2).
    media_port: MediaPort,
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
        let (media_port, media_port_rt) = media_port();

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
            cpu: Mutex::new(cpuprof::CpuSampler::new(Instant::now(), crate::sys::cpu::tsc_now())),
            recorder: Arc::new(Mutex::new(record::Recorder::new(rec_ctl, info.out_rate))),
            media: Mutex::new(Vec::new()),
            next_key: AtomicU64::new(1),
            media_settings: Arc::new(MediaSettings::new(JitterSettings {
                min_ms: cfg.jitter_min_ms(),
                max_ms: cfg.jitter_max_ms(),
                profile: cfg.jitter_profile(),
            })),
            media_port,
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
            engine.net = Some(Net::start(peer_id, &opts)?); // 실패 시 engine drop이 오디오 정리
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
            media_port: media_port_rt,
            media_settings: engine.media_settings.clone(),
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

    /// 피어 추가(같은 peerId면 교체). 미디어 상태(지터 버퍼 할당)는 여기, 커맨드 스레드에서 만든다(설계 1.4절).
    pub fn add_peer(&self, peer_id: String, desc: &LocalDescriptor) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        self.media_port.reclaim();
        let key = PeerKey::from_raw(self.next_key.fetch_add(1, Ordering::Relaxed));
        let settings = self.media_settings.current();
        let media = Arc::new(PeerMediaShared::new(key, peer_id.clone(), desc.ssrc, settings));
        // 등록부 락 안에서 믹스 설정 복사 → net 교체 → 등록부 교체: 겹친 set_peer_gain은 옛 것에 쓰여 복사로 넘어가거나
        // 새 것에 쓰인다(설계 7.3절). 복사는 net에 넣기 전에 해서 워커가 새 항목을 기본 게인으로 보는 순간이 없다.
        let mut reg = self.media.lock();
        if let Some(old) = reg.iter().find(|m| m.peer_id == peer_id) {
            media.inherit_mix(old);
        }
        // 명령 두 칸(추가 + 교체면 옛 것 제거)을 net에 넣기 전에 확보한다: 넣은 뒤 실패해 되돌릴 일이 없게(설계 4절).
        // 확보 못 하면 워커가 멈춘 것이다(용량 16, 워커가 반복마다 비운다).
        let mut cmd = self.media_port.reserve_cmd(2)?;
        let replaced = net.add_peer(peer_id, desc, media.clone())?;
        // 순서: 새 것 추가 → 옛 것 제거(설계 1.4절). 생산자가 이 락 하나로 직렬화되고 큐가 FIFO라 뒤바뀌지 않는다.
        let _ = cmd.try_push(MediaCmd::Add(PeerMedia::new(media.clone(), settings.profile)));
        if let Some(old_key) = replaced {
            let _ = cmd.try_push(MediaCmd::Remove(old_key));
            reg.retain(|m| m.key != old_key);
        }
        reg.push(media);
        Ok(())
    }

    /// 릴레이 설정/토큰 갱신/해제(설계 4절 `engine_set_relay`, relay-design.md).
    pub fn set_relay(&self, relay: Option<crate::net::RelaySetting<'_>>) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.set_relay(relay)
    }

    /// 지금 릴레이가 `host`(host:port 문자열)로 설정돼 있으면 그때 해석한 주소(DNS를 다시 풀지 않는다, M3).
    pub fn relay_addr_for_host(&self, host: &str) -> Option<std::net::SocketAddr> {
        self.net.as_ref().and_then(|n| n.relay_addr_for_host(host))
    }

    /// 측정 도구 전용(P1-4f M1, `tests/cpu_profile.rs`): PCM16 설정에서 모든 피어를 손실 피드백 전환 상태로 강제해
    /// 피어마다 전용 Opus 인코더가 돌게 한다(최악 조건 CPU). 앱은 쓰지 않는다.
    #[doc(hidden)]
    pub fn force_auto_opus(&self, on: bool) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?.shared().set_force_auto_opus(on);
        Ok(())
    }

    pub fn remove_peer(&self, peer_id: &str) -> Result<(), String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        self.media_port.reclaim();
        let mut reg = self.media.lock();
        let mut cmd = self.media_port.reserve_cmd(1)?;
        if let Some(key) = net.remove_peer(peer_id) {
            let _ = cmd.try_push(MediaCmd::Remove(key));
            reg.retain(|m| m.key != key);
        }
        Ok(())
    }

    pub fn set_peer_gain(&self, peer_id: &str, gain: f32, muted: bool, pan: Option<f32>) -> Result<(), String> {
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        let reg = self.media.lock();
        let m = reg.iter().find(|m| m.peer_id == peer_id).ok_or("알 수 없는 피어")?;
        m.mix.gain.store(clamp_gain(gain));
        m.mix.muted.store(muted, Ordering::Relaxed);
        m.mix.pan.store(mixer::clamp_pan(pan.unwrap_or(0.0)));
        Ok(())
    }

    /// 지터 버퍼 '저지연(0) ↔ 안정(1)' 프로필을 실행 중에 바꾼다(모든 피어 + 이후 추가될 피어, P1-5).
    pub fn set_jitter_profile(&self, profile: f32) -> Result<(), String> {
        if !profile.is_finite() || !(0.0..=1.0).contains(&profile) {
            return Err(format!("jitterProfile은 0..1 범위여야 합니다: {profile}"));
        }
        self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        // 지금 있는 피어 모두와 이후 추가될 피어(P1-5): 워커가 반복마다 이 값을 피어별 마지막 적용값과 비교해 적용한다
        // (③-a2). 추가와 겹쳐도 다음 반복에 맞춰진다.
        self.media_settings.set_profile(profile);
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

    /// 녹음 시작(P2-4): `dir` 아래 세션 폴더에 트랙별 WAV를 쓴다.
    pub fn record_start(&self, opts: record::RecordOptions, dir: &std::path::Path) -> Result<record::RecordStats, String> {
        let net = self.net.as_ref().map(|n| n.shared().clone());
        let resolve: record::PeerResolver = Box::new(move |ssrc| net.as_ref().and_then(|n| n.peer_id_for_ssrc(ssrc)));
        self.recorder.lock().start(opts, dir, resolve, crate::sys::time::unix_ms())
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
        let media = self.media.lock().iter().find(|m| m.peer_id == peer_id).cloned().ok_or("알 수 없는 피어")?;
        let (in_ms, out_ms) = self.buffer_ms();
        Ok(EchoJob {
            probe: self.probe.clone(),
            net,
            peer_id: peer_id.to_string(),
            peer_ssrc: media.ssrc,
            media,
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

    /// 열린 스트림 정보(백엔드, 레이트, 장치 주기).
    pub fn stream_info(&self) -> StreamInfo {
        self.info
    }

    /// 피어와의 키 확인용 안전 번호(SAS). 양쪽이 같은 값을 보면 시그널링 서버가 키를 바꿔치기하지 않은 것이다.
    pub fn safety_code(&self, peer_id: &str) -> Result<String, String> {
        let net = self.net.as_ref().ok_or("네트워크 없이 시작된 엔진입니다")?;
        net.safety_code(peer_id)
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
        // 워커가 끝났으므로 반환 큐에 남은 것과 등록부를 여기(제어 평면)에서 해제한다.
        self.media_port.reclaim();
        self.media.lock().clear();
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
