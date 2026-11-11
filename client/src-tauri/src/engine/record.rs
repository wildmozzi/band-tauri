//! 합주 녹음(P2-4): 로컬 멀티트랙(내 입력 + 피어별 수신 오디오 + 선택: 믹스)을 WAV로 저장한다(설계 문서 1.5절).
//!
//! **출력 콜백(RT)**: 청크마다 재생한 소스 샘플(게인·팬·뮤트 전, 장치 레이트 스테레오)을 사전할당 SPSC 샘플 링에
//! 넣고, 청크 머리([`RecBlock`]: 타임라인 위치, 프레임 수, 활성 슬롯, 슬롯 점유자 ssrc)를 블록 링에 넣는다.
//! 링에 최악의 경우(모든 슬롯 + 믹스)만큼 자리가 없으면 그 청크를 통째로 버리고 `dropped_frames`만 올린다.
//! 할당·락·시스템콜·로그 없음. 메트로놈 클릭은 소스가 아니므로 들어가지 않는다(믹스 트랙도 클릭 없는 탭 버스).
//!
//! **핸드오프**: 녹음 링은 녹음을 시작할 때(비RT) 할당해 용량 1 링으로 콜백에 넘긴다. 정지하면 `detach`를 켜고,
//! 콜백이 다음 주기 시작에 그 녹음 끝점을 반환 링으로 돌려준다. 그래서 링 해제도 비RT에서 일어나고, 반환을 받은
//! 뒤에는 콜백이 더 쓰지 않으므로 쓰기 스레드가 남은 것을 모두 비우고 닫는다.
//!
//! **쓰기 스레드**(`band-record`): 블록 순서대로 샘플을 꺼내 트랙 파일에 쓴다. 트랙 길이를 블록의 타임라인 위치
//! (녹음 시작 기준 장치 프레임)에 맞춰 무음으로 채우므로, 늦게 들어온 피어는 앞이 무음이고 버린 청크 자리도
//! 무음이다. 정지하면 모든 트랙을 같은 길이로 맞춘다. 같은 피어(peerId)가 다시 들어오면 같은 트랙에 이어 쓴다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use serde::{Deserialize, Serialize};

use super::rt::{LOCAL_SOURCE, MAX_SOURCES};
use super::wav::{RecordFormat, WavWriter};

/// 녹음 링이 버티는 시간(초). 디스크가 이만큼 멈춰도 버리지 않는다.
const RING_SECONDS: usize = 3;
/// 콜백 청크 최소 크기 가정(블록 링 용량 산정).
const MIN_CHUNK_FRAMES: usize = 16;
/// 정지 시 콜백이 녹음 끝점을 돌려주길 기다리는 최대 시간.
const DETACH_WAIT: Duration = Duration::from_millis(1000);

/// 콜백 청크 하나의 머리.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecBlock {
    /// 청크 첫 프레임의 출력 타임라인 위치(`out_played_frames` 기준 장치 프레임).
    pub pos: u64,
    pub frames: u32,
    /// 샘플 링에 이 청크 샘플이 들어간 슬롯(비트). 슬롯 순서대로 `frames × 2`씩 들어 있다.
    pub mask: u16,
    /// 슬롯 샘플 뒤에 믹스(탭 버스) `frames × 2`가 있다.
    pub mix: bool,
    /// 슬롯 점유자(피어 ssrc, 로컬 슬롯은 0).
    pub occ: [u32; MAX_SOURCES],
}

/// RT와 제어 쪽이 함께 보는 녹음 통계.
#[derive(Debug, Default)]
pub struct RecordRtShared {
    /// 녹음 링이 모자라 버린 프레임(녹음 품질 지표). 그 자리는 무음으로 채워진다.
    pub dropped_frames: AtomicU64,
    /// 콜백이 이 녹음 끝점을 돌려줬다(그 뒤로는 링에 쓰지 않음). 쓰기 스레드는 이것을 보고 남은 것을 비운다.
    pub detached: AtomicBool,
}

/// 출력 콜백이 쓰는 녹음 끝점(녹음 하나당 하나).
pub struct RecordRt {
    samples: HeapProd<f32>,
    blocks: HeapProd<RecBlock>,
    include_mix: bool,
    shared: Arc<RecordRtShared>,
    cur: RecBlock,
    open: bool,
}

impl RecordRt {
    /// RT: 청크 시작. 최악의 경우만큼 자리가 없으면 이 청크를 버린다.
    #[inline]
    pub fn begin(&mut self, pos: u64, n: usize) {
        let need = n * 2 * (MAX_SOURCES + 1);
        self.open = self.samples.vacant_len() >= need && self.blocks.vacant_len() >= 1;
        if !self.open {
            self.shared.dropped_frames.fetch_add(n as u64, Ordering::Relaxed);
        }
        self.cur = RecBlock { pos, frames: n as u32, mask: 0, mix: false, occ: [0; MAX_SOURCES] };
    }

    /// RT: 슬롯 하나가 이 청크에 재생한 샘플(`n × 2`, 게인 전).
    #[inline]
    pub fn source(&mut self, slot: usize, occupant: u32, data: &[f32]) {
        if !self.open || slot >= MAX_SOURCES {
            return;
        }
        self.samples.push_slice(data);
        self.cur.mask |= 1 << slot;
        if let Some(o) = self.cur.occ.get_mut(slot) {
            *o = occupant;
        }
    }

    /// RT: 청크 끝. 믹스(클릭 없는 탭 버스)를 붙이고 머리를 넣는다.
    #[inline]
    pub fn finish(&mut self, mix: &[f32]) {
        if !self.open {
            return;
        }
        if self.include_mix {
            self.samples.push_slice(mix);
            self.cur.mix = true;
        }
        let _ = self.blocks.try_push(self.cur);
        self.open = false;
    }
}

/// 출력 콜백이 가진 핸드오프 끝점.
pub struct RecordPortRt {
    inbox: HeapCons<RecordRt>,
    outbox: HeapProd<RecordRt>,
    detach: Arc<AtomicBool>,
    active: Option<RecordRt>,
}

impl RecordPortRt {
    /// RT: 콜백 시작마다 부른다. 새 녹음을 받고, 정지 요청이면 돌려준다(해제는 비RT에서).
    #[inline]
    pub fn poll(&mut self) {
        if self.active.is_none() {
            self.active = self.inbox.try_pop();
        }
        if self.detach.load(Ordering::Acquire) {
            if let Some(r) = self.active.take() {
                // 지난 콜백까지 넣은 것이 전부다(Release: 링 기록이 먼저 보인다)
                r.shared.detached.store(true, Ordering::Release);
                if let Err(r) = self.outbox.try_push(r) {
                    r.shared.detached.store(false, Ordering::Release);
                    self.active = Some(r);
                }
            }
        }
    }

    #[inline]
    pub fn active(&mut self) -> Option<&mut RecordRt> {
        self.active.as_mut()
    }
}

/// 제어 쪽 핸드오프 끝점.
pub struct RecordPortCtl {
    inbox: HeapProd<RecordRt>,
    outbox: HeapCons<RecordRt>,
    detach: Arc<AtomicBool>,
    /// 콜백에 넘긴 녹음 끝점이 아직 돌아오지 않았다.
    outstanding: bool,
}

/// 핸드오프 링 쌍(엔진 시작 시, 장치를 열기 전에 만든다).
pub fn record_port() -> (RecordPortRt, RecordPortCtl) {
    let (in_p, in_c) = HeapRb::<RecordRt>::new(1).split();
    let (out_p, out_c) = HeapRb::<RecordRt>::new(2).split();
    let detach = Arc::new(AtomicBool::new(false));
    (
        RecordPortRt { inbox: in_c, outbox: out_p, detach: detach.clone(), active: None },
        RecordPortCtl { inbox: in_p, outbox: out_c, detach, outstanding: false },
    )
}

impl RecordPortCtl {
    /// 돌아온 끝점을 거둔다. 반환: 콜백에 남은 것이 없다.
    fn reclaim(&mut self) -> bool {
        while self.outbox.try_pop().is_some() {
            self.outstanding = false;
        }
        !self.outstanding
    }

    fn attach(&mut self, r: RecordRt) -> Result<(), String> {
        if !self.reclaim() {
            return Err("이전 녹음이 아직 정리되지 않았습니다. 잠시 후 다시 시도하세요".into());
        }
        self.detach.store(false, Ordering::Release);
        self.inbox.try_push(r).map_err(|_| "녹음 시작 실패(핸드오프)".to_string())?;
        self.outstanding = true;
        Ok(())
    }

    /// 콜백에 정지를 요청한다(다음 주기 시작에 끝점을 돌려준다). 기다리지 않는다.
    fn request_detach(&self) {
        self.detach.store(true, Ordering::Release);
    }
}

/// `engine_record_start` 옵션.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordOptions {
    pub format: RecordFormat,
    /// 믹스 트랙(내가 들은 피어 게인·팬 + 내 입력, 클릭 없음)도 저장.
    pub include_mix: bool,
    /// 세션 폴더 이름(파일 이름에 못 쓰는 문자는 `_`). 없으면 `band-rec-<UTC 시각>`.
    pub name: Option<String>,
    /// 내 입력 트랙 이름(기본 "me").
    pub local_name: Option<String>,
    /// 피어별 트랙 이름(peerId → 표시 이름). 없으면 peerId 앞 8자.
    pub track_names: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Local,
    Peer,
    Mix,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordTrack {
    pub kind: TrackKind,
    pub name: String,
    pub file: String,
    pub peer_id: Option<String>,
}

/// `engine_record_stop` 반환(세션 폴더의 `session.json`과 같은 내용).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSummary {
    pub folder: String,
    pub format: RecordFormat,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_sec: f64,
    pub bytes: u64,
    pub dropped_frames: u64,
    pub started_at_ms: u64,
    pub tracks: Vec<RecordTrack>,
    pub error: Option<String>,
}

/// `EngineStats.recording`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordStats {
    pub folder: String,
    pub format: RecordFormat,
    pub sample_rate: u32,
    pub duration_sec: f64,
    pub bytes: u64,
    pub tracks: u32,
    pub dropped_frames: u64,
    pub error: Option<String>,
}

/// 피어 ssrc → peerId.
pub type PeerResolver = Box<dyn Fn(u32) -> Option<String> + Send>;

#[derive(Default)]
struct WriterShared {
    frames: AtomicU64,
    bytes: AtomicU64,
    tracks: AtomicU32,
    error: Mutex<Option<String>>,
    /// 피어 트랙 이름(peerId → 표시 이름). 시작 옵션으로 채우고 녹음 중에 `set_track_names`로 더한다.
    names: Mutex<HashMap<String, String>>,
}

struct Session {
    folder: PathBuf,
    format: RecordFormat,
    rate: u32,
    rt_shared: Arc<RecordRtShared>,
    shared: Arc<WriterShared>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<RecordSummary>,
}

/// 정지 중인 녹음. [`StopJob::finish`]가 콜백 반환(최대 1초)과 쓰기 스레드 마무리를 기다린다.
pub struct StopJob {
    s: Session,
}

impl StopJob {
    pub fn finish(self) -> Result<RecordSummary, String> {
        let s = self.s;
        let t0 = Instant::now();
        while !s.rt_shared.detached.load(Ordering::Acquire) {
            if t0.elapsed() >= DETACH_WAIT {
                log::warn!("녹음 정지: 출력 콜백이 응답하지 않아 남은 링을 비우고 닫습니다");
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        s.stop.store(true, Ordering::Release);
        let summary = s.handle.join().map_err(|_| "녹음 스레드 패닉".to_string())?;
        log::info!("녹음 정지: {:.1}초, {}트랙, 버림 {}프레임", summary.duration_sec, summary.tracks.len(), summary.dropped_frames);
        Ok(summary)
    }
}

/// 엔진이 가진 녹음 제어(핸드오프 + 쓰기 스레드).
pub struct Recorder {
    port: RecordPortCtl,
    rate: u32,
    session: Option<Session>,
}

impl Recorder {
    pub fn new(port: RecordPortCtl, rate: u32) -> Self {
        Self { port, rate, session: None }
    }

    pub fn is_recording(&self) -> bool {
        self.session.is_some()
    }

    /// 녹음 시작: `dir` 아래에 세션 폴더를 만들고 링을 할당해 콜백에 넘긴 뒤 쓰기 스레드를 띄운다.
    pub fn start(&mut self, opts: RecordOptions, dir: &Path, resolve: PeerResolver, now_ms: u64) -> Result<RecordStats, String> {
        if self.session.is_some() {
            return Err("이미 녹음 중입니다".into());
        }
        let rate = self.rate.max(1);
        let base = opts.name.as_deref().map(sanitize_name).filter(|s| !s.is_empty()).unwrap_or_else(|| default_session_name(now_ms));
        std::fs::create_dir_all(dir).map_err(|e| format!("녹음 폴더를 만들 수 없습니다: {e}"))?;
        let folder = unique_dir(dir, &base).map_err(|e| format!("녹음 폴더를 만들 수 없습니다: {e}"))?;

        let frames_cap = rate as usize * RING_SECONDS;
        let (s_p, s_c) = HeapRb::<f32>::new(frames_cap * 2 * (MAX_SOURCES + 1)).split();
        let (b_p, b_c) = HeapRb::<RecBlock>::new(frames_cap / MIN_CHUNK_FRAMES + 16).split();
        let rt_shared = Arc::new(RecordRtShared::default());
        let rt = RecordRt { samples: s_p, blocks: b_p, include_mix: opts.include_mix, shared: rt_shared.clone(), cur: RecBlock::default(), open: false };
        if let Err(e) = self.port.attach(rt) {
            std::fs::remove_dir(&folder).ok();
            return Err(e);
        }

        let shared = Arc::new(WriterShared { names: Mutex::new(opts.track_names.clone()), ..Default::default() });
        let stop = Arc::new(AtomicBool::new(false));
        let w = Writer::new(&opts, folder.clone(), rate, s_c, b_c, resolve, shared.clone(), rt_shared.clone(), now_ms);
        let st = stop.clone();
        let handle = std::thread::Builder::new().name("band-record".into()).spawn(move || w.run(&st));
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                self.port.request_detach();
                std::fs::remove_dir(&folder).ok();
                return Err(format!("녹음 스레드 생성 실패: {e}"));
            }
        };
        log::info!("녹음 시작: {}Hz {:?} mix={}", rate, opts.format, opts.include_mix);
        self.session = Some(Session { folder, format: opts.format, rate, rt_shared, shared, stop, handle });
        self.stats().ok_or_else(|| "녹음 상태 없음".to_string())
    }

    /// 정지 1단계(빠름, 락 안에서): 세션을 떼고 콜백에 반환을 요청한다. 이때부터 `stats()`는 None.
    /// 2단계 [`StopJob::finish`]는 락 밖에서, 3단계 [`Recorder::reclaim`]은 다시 짧게 락을 잡고 한다.
    pub fn begin_stop(&mut self) -> Result<StopJob, String> {
        let s = self.session.take().ok_or("녹음 중이 아닙니다")?;
        self.port.request_detach();
        Ok(StopJob { s })
    }

    /// 콜백이 돌려준 녹음 끝점(링)을 거둬 해제한다.
    /// 녹음 중 피어 트랙 이름을 더한다(늦게 들어온 연주자, PR #49 M1). 앞으로 만들 트랙은 이 이름을 쓰고,
    /// 이름 없이(peerId 앞 8자) 만든 트랙은 정지할 때 이 이름으로 파일을 바꾼다. 녹음 중이 아니면 에러.
    pub fn set_track_names(&self, names: HashMap<String, String>) -> Result<(), String> {
        let s = self.session.as_ref().ok_or("녹음 중이 아닙니다")?;
        s.shared.names.lock().extend(names);
        Ok(())
    }

    pub fn reclaim(&mut self) {
        self.port.reclaim();
    }

    /// 세 단계를 한 번에(락을 오래 잡아도 되는 곳·테스트용).
    pub fn stop(&mut self) -> Result<RecordSummary, String> {
        let r = self.begin_stop()?.finish();
        self.reclaim();
        r
    }

    pub fn stats(&self) -> Option<RecordStats> {
        let s = self.session.as_ref()?;
        Some(RecordStats {
            folder: s.folder.to_string_lossy().into_owned(),
            format: s.format,
            sample_rate: s.rate,
            duration_sec: s.shared.frames.load(Ordering::Relaxed) as f64 / s.rate as f64,
            bytes: s.shared.bytes.load(Ordering::Relaxed),
            tracks: s.shared.tracks.load(Ordering::Relaxed),
            dropped_frames: s.rt_shared.dropped_frames.load(Ordering::Relaxed),
            error: s.shared.error.lock().clone(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum TrackKey {
    Local,
    Peer(String),
    Mix,
}

struct Track {
    key: TrackKey,
    meta: RecordTrack,
    /// 파일 번호(`NN-이름.wav`의 NN).
    idx: usize,
    /// 피어 트랙을 이름 없이(peerId 앞 8자) 만들었다 → 정지 때 이름이 생겼으면 파일을 바꾼다.
    unnamed: bool,
    w: Option<WavWriter>,
    /// 쓴 프레임(타임라인 기준, 무음 포함).
    frames: u64,
}

struct Writer {
    folder: PathBuf,
    format: RecordFormat,
    rate: u32,
    samples: HeapCons<f32>,
    blocks: HeapCons<RecBlock>,
    resolve: PeerResolver,
    local_name: String,
    tracks: Vec<Track>,
    by_ssrc: HashMap<u32, usize>,
    origin: Option<u64>,
    end: u64,
    buf: Vec<f32>,
    failed: Option<String>,
    /// 파일을 만들지 못한 트랙 키(그 트랙만 빼고 나머지는 계속 쓴다, PR #47 M2). 다시 시도하지 않는다.
    bad_keys: Vec<TrackKey>,
    /// 트랙 생성 실패 메시지(첫 것). 녹음은 계속되므로 `failed`와 따로 둔다.
    track_error: Option<String>,
    shared: Arc<WriterShared>,
    rt_shared: Arc<RecordRtShared>,
    started_at_ms: u64,
}

impl Writer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        opts: &RecordOptions,
        folder: PathBuf,
        rate: u32,
        samples: HeapCons<f32>,
        blocks: HeapCons<RecBlock>,
        resolve: PeerResolver,
        shared: Arc<WriterShared>,
        rt_shared: Arc<RecordRtShared>,
        started_at_ms: u64,
    ) -> Self {
        Self {
            folder,
            format: opts.format,
            rate,
            samples,
            blocks,
            resolve,
            local_name: opts.local_name.clone().unwrap_or_else(|| "me".into()),
            tracks: Vec::new(),
            by_ssrc: HashMap::new(),
            origin: None,
            end: 0,
            buf: Vec::new(),
            failed: None,
            bad_keys: Vec::new(),
            track_error: None,
            shared,
            rt_shared,
            started_at_ms,
        }
    }

    fn run(mut self, stop: &AtomicBool) -> RecordSummary {
        loop {
            // 정지 플래그를 먼저 읽고 비운다: 플래그 전에 콜백이 넣은 것은 모두 이번에 나간다.
            let stopping = stop.load(Ordering::Acquire);
            self.drain();
            if stopping {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.finish()
    }

    fn fail(&mut self, e: String) {
        log::error!("녹음 쓰기 실패: {e}");
        *self.shared.error.lock() = Some(e.clone());
        self.failed = Some(e);
        // 파일은 지금까지 쓴 만큼 닫는다. 이후 블록은 링에서 비우기만 한다.
        for t in &mut self.tracks {
            if let Some(w) = t.w.take() {
                let _ = w.finish();
            }
        }
    }

    fn drain(&mut self) {
        while let Some(b) = self.blocks.try_pop() {
            let n = b.frames as usize * 2;
            let origin = *self.origin.get_or_insert(b.pos);
            let rel = b.pos.saturating_sub(origin);
            for slot in 0..MAX_SOURCES {
                if b.mask & (1 << slot) == 0 {
                    continue;
                }
                self.pop(n);
                let key = if slot == LOCAL_SOURCE { self.local_track() } else { self.peer_track(b.occ[slot]) };
                if let Some(i) = key {
                    self.write_track(i, rel, b.frames as u64);
                }
            }
            if b.mix {
                self.pop(n);
                let i = self.track(TrackKey::Mix, TrackKind::Mix, "mix".into(), None);
                if let Some(i) = i {
                    self.write_track(i, rel, b.frames as u64);
                }
            }
            self.end = self.end.max(rel + b.frames as u64);
        }
        self.shared.frames.store(self.end, Ordering::Relaxed);
        let bytes = self.tracks.iter().filter_map(|t| t.w.as_ref()).map(|w| w.data_bytes()).sum();
        self.shared.bytes.store(bytes, Ordering::Relaxed);
    }

    fn pop(&mut self, n: usize) {
        self.buf.resize(n, 0.0);
        let got = self.samples.pop_slice(&mut self.buf);
        // 콜백은 머리보다 샘플을 먼저 넣으므로 모자랄 수 없다. 혹시 모자라면 무음.
        if let Some(rest) = self.buf.get_mut(got..) {
            rest.fill(0.0);
        }
    }

    fn local_track(&mut self) -> Option<usize> {
        let name = self.local_name.clone();
        self.track(TrackKey::Local, TrackKind::Local, name, None)
    }

    fn peer_track(&mut self, ssrc: u32) -> Option<usize> {
        if let Some(&i) = self.by_ssrc.get(&ssrc) {
            return Some(i);
        }
        let id = (self.resolve)(ssrc).unwrap_or_else(|| format!("ssrc-{ssrc:08x}"));
        let named = self.shared.names.lock().get(&id).cloned();
        let unnamed = named.is_none();
        let name = named.unwrap_or_else(|| id.chars().take(8).collect());
        let i = self.track(TrackKey::Peer(id.clone()), TrackKind::Peer, name, Some(id))?;
        if let Some(t) = self.tracks.get_mut(i) {
            t.unnamed = unnamed;
        }
        self.by_ssrc.insert(ssrc, i);
        Some(i)
    }

    /// 키에 맞는 트랙(없으면 만든다). 쓰기에 실패한 뒤에는 None.
    fn track(&mut self, key: TrackKey, kind: TrackKind, name: String, peer_id: Option<String>) -> Option<usize> {
        if let Some(i) = self.tracks.iter().position(|t| t.key == key) {
            return Some(i);
        }
        if self.failed.is_some() || self.bad_keys.contains(&key) {
            return None;
        }
        let idx = self.tracks.len() + 1;
        let clean = sanitize_name(&name);
        let label = if clean.is_empty() { format!("track{idx}") } else { clean };
        let file = match kind {
            TrackKind::Mix => "00-mix.wav".to_string(),
            _ => format!("{idx:02}-{label}.wav"),
        };
        match WavWriter::create(&self.folder.join(&file), self.format, 2, self.rate) {
            Ok(w) => {
                self.tracks.push(Track { key, meta: RecordTrack { kind, name, file, peer_id }, idx, unnamed: false, w: Some(w), frames: 0 });
                self.shared.tracks.store(self.tracks.len() as u32, Ordering::Relaxed);
                Some(self.tracks.len() - 1)
            }
            Err(e) => {
                // 백신·인덱서의 순간 잠금, 이름 문제 등: 이 트랙만 빼고 나머지 트랙은 계속 쓴다(PR #47 M2).
                // 디스크 가득이면 다른 트랙도 곧 쓰기에 실패해 `fail`로 전체가 멈춘다.
                let msg = format!("{file}: 트랙을 만들 수 없습니다: {e}");
                log::error!("녹음 {msg}");
                self.shared.error.lock().get_or_insert_with(|| msg.clone());
                self.track_error.get_or_insert(msg);
                self.bad_keys.push(key);
                None
            }
        }
    }

    /// `buf`(프레임 `frames`)를 타임라인 `rel`에 쓴다. 앞이 비었으면 무음으로 채운다.
    fn write_track(&mut self, i: usize, rel: u64, frames: u64) {
        let Some(t) = self.tracks.get_mut(i) else { return };
        let Some(w) = t.w.as_mut() else { return };
        let mut res = Ok(());
        if rel > t.frames {
            res = w.write_silence(rel - t.frames);
            t.frames = rel;
        }
        // 겹치는 앞부분(있을 수 없지만)은 건너뛴다.
        let skip = (t.frames - rel).min(frames) as usize;
        if res.is_ok() && skip < frames as usize {
            res = w.write(&self.buf[skip * 2..frames as usize * 2]);
            t.frames = rel + frames;
        }
        if let Err(e) = res {
            let f = t.meta.file.clone();
            self.fail(format!("{f}: {e}"));
        }
    }

    /// 이름 없이 만든 피어 트랙에 녹음 중 이름이 생겼으면 파일 이름을 바꾼다(파일은 이미 닫힘). 실패하면 옛 이름 그대로.
    fn rename_late_named(&mut self) {
        let names = self.shared.names.lock().clone();
        for t in self.tracks.iter_mut().filter(|t| t.unnamed) {
            let Some(name) = t.meta.peer_id.as_ref().and_then(|id| names.get(id)) else { continue };
            let clean = sanitize_name(name);
            if clean.is_empty() {
                continue;
            }
            let file = format!("{:02}-{clean}.wav", t.idx);
            let to = self.folder.join(&file);
            if to.exists() {
                continue;
            }
            match std::fs::rename(self.folder.join(&t.meta.file), &to) {
                Ok(()) => {
                    t.meta.file = file;
                    t.meta.name = name.clone();
                    t.unnamed = false;
                }
                Err(e) => log::warn!("녹음 트랙 이름 바꾸기 실패({}): {e}", t.meta.file),
            }
        }
    }

    fn finish(mut self) -> RecordSummary {
        let end = self.end;
        let mut bytes = 0;
        let mut err = self.failed.clone().or_else(|| self.track_error.clone());
        for t in &mut self.tracks {
            if let Some(mut w) = t.w.take() {
                let r = if end > t.frames { w.write_silence(end - t.frames) } else { Ok(()) };
                let r = match r {
                    Ok(()) => w.finish(),
                    // 무음 채우기 실패: 디스크에 있는 만큼으로 헤더를 맞춰 닫는다(PR #47 M1)
                    Err(e) => {
                        bytes += w.finish_best_effort();
                        Err(e)
                    }
                };
                match r {
                    Ok(b) => bytes += b,
                    Err(e) => {
                        err.get_or_insert_with(|| format!("{}: {e}", t.meta.file));
                    }
                }
            }
        }
        self.rename_late_named();
        let summary = RecordSummary {
            folder: self.folder.to_string_lossy().into_owned(),
            format: self.format,
            sample_rate: self.rate,
            channels: 2,
            duration_sec: end as f64 / self.rate.max(1) as f64,
            bytes,
            dropped_frames: self.rt_shared.dropped_frames.load(Ordering::Relaxed),
            started_at_ms: self.started_at_ms,
            tracks: self.tracks.iter().map(|t| t.meta.clone()).collect(),
            error: err,
        };
        match serde_json::to_vec_pretty(&summary) {
            Ok(j) => {
                if let Err(e) = std::fs::write(self.folder.join("session.json"), j) {
                    log::warn!("session.json 저장 실패: {e}");
                }
            }
            Err(e) => log::warn!("session.json 직렬화 실패: {e}"),
        }
        summary
    }
}

/// 파일·폴더 이름에 쓸 수 있게 고친다(Windows 금지 문자·제어 문자 → `_`, 끝 점·공백 제거, 60자).
pub fn sanitize_name(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .take(60)
        .collect();
    let t = mapped.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_string();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
        "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = t.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        format!("_{t}")
    } else {
        t
    }
}

/// `band-rec-YYYYMMDD-HHMMSSZ`(UTC). 프론트가 `name`으로 지역 시각을 넘기는 것이 보통이다.
pub fn default_session_name(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("band-rec-{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// Howard Hinnant의 days → civil 알고리즘
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `dir/base`가 있으면 `base (2)`, `base (3)`… 중 없는 것을 만든다.
fn unique_dir(dir: &Path, base: &str) -> std::io::Result<PathBuf> {
    for k in 1..1000 {
        let name = if k == 1 { base.to_string() } else { format!("{base} ({k})") };
        let p = dir.join(name);
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("같은 이름의 녹음 폴더가 너무 많습니다"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("band-rec-test-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        d
    }

    /// 정지하는 동안 콜백처럼 `poll`을 돌려 끝점을 돌려받게 한다.
    fn stop_with_rt(rec: &mut Recorder, port_rt: RecordPortRt) -> (RecordSummary, RecordPortRt) {
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let h = std::thread::spawn(move || {
            let mut p = port_rt;
            while !d.load(Ordering::Relaxed) {
                p.poll();
                std::thread::sleep(Duration::from_millis(1));
            }
            p
        });
        let sum = rec.stop().unwrap();
        done.store(true, Ordering::Relaxed);
        (sum, h.join().unwrap())
    }

    fn data_frames(path: &Path) -> (usize, Vec<i32>) {
        let b = std::fs::read(path).unwrap();
        let data = &b[80..];
        let s: Vec<i32> = data.chunks(3).map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8).collect();
        (data.len() / 6, s)
    }

    /// 콜백을 흉내 낸다: 로컬은 처음부터, 피어(ssrc 7)는 480프레임 뒤부터, 한 청크는 링 부족으로 버린 것처럼 건너뛴다.
    #[test]
    fn tracks_share_timeline_and_pad_late_peer() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("align");
        let opts = RecordOptions {
            include_mix: true,
            name: Some("세션: 1".into()),
            track_names: HashMap::from([("peer-a".to_string(), "기타".to_string())]),
            ..Default::default()
        };
        let resolve: PeerResolver = Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string()));
        let st = rec.start(opts, &dir, resolve, 0).unwrap();
        assert!(st.folder.ends_with("세션_ 1"), "{}", st.folder);
        assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_err());

        let n = 240usize;
        let local = vec![0.25f32; n * 2];
        let peer = vec![-0.5f32; n * 2];
        let mix = vec![0.125f32; n * 2];
        let base = 1_000_000u64; // 녹음 시작 전부터 돌던 출력 타임라인
        for k in 0..6u64 {
            port_rt.poll();
            let r = port_rt.active().expect("녹음 끝점");
            if k == 3 {
                continue; // 버린 청크(블록 없음)
            }
            r.begin(base + k * n as u64, n);
            r.source(LOCAL_SOURCE, 0, &local);
            if k >= 2 {
                r.source(1, 7, &peer);
            }
            r.finish(&mix);
        }
        let (sum, mut port_rt) = stop_with_rt(&mut rec, port_rt);
        assert!(port_rt.active().is_none(), "끝점을 돌려줬다");
        assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_ok(), "돌려받았으니 다시 시작된다");
        rec.stop().unwrap();
        assert!(sum.error.is_none(), "{:?}", sum.error);
        assert_eq!(sum.tracks.len(), 3);
        assert!((sum.duration_sec - 6.0 * n as f64 / 48_000.0).abs() < 1e-9);

        let folder = PathBuf::from(&sum.folder);
        let me = folder.join(&sum.tracks.iter().find(|t| t.kind == TrackKind::Local).unwrap().file);
        let gt = sum.tracks.iter().find(|t| t.kind == TrackKind::Peer).unwrap();
        assert_eq!(gt.name, "기타");
        assert_eq!(gt.peer_id.as_deref(), Some("peer-a"));
        let (f_me, s_me) = data_frames(&me);
        let (f_peer, s_peer) = data_frames(&folder.join(&gt.file));
        assert_eq!(f_me, 6 * n);
        assert_eq!(f_peer, 6 * n, "모든 트랙 길이가 같다");
        // 피어 앞 2청크는 무음, 3번째 청크부터 소리
        assert!(s_peer[..2 * n * 2].iter().all(|&v| v == 0));
        assert!(s_peer[2 * n * 2] < -4_000_000);
        // 버린 청크(3) 자리는 모든 트랙에서 무음
        assert!(s_me[3 * n * 2..4 * n * 2].iter().all(|&v| v == 0));
        assert!(s_me[4 * n * 2] > 2_000_000);
        assert!(folder.join("session.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// PR #49 M1: 녹음 중에 이름을 더하면 이후 트랙은 그 이름으로 만들고, 이름 없이 만든 트랙은 정지 때 파일 이름을 바꾼다.
    #[test]
    fn late_track_names_apply() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("latenames");
        assert!(rec.set_track_names(HashMap::new()).is_err(), "녹음 중이 아니면 에러");
        let resolve: PeerResolver = Box::new(|ssrc| match ssrc {
            7 => Some("peer-aaaaaaaaaa".to_string()),
            9 => Some("peer-b".to_string()),
            _ => None,
        });
        rec.start(RecordOptions::default(), &dir, resolve, 0).unwrap();
        let n = 240usize;
        let data = vec![0.25f32; n * 2];
        let feed = |port_rt: &mut RecordPortRt, k: u64, peers: &[(usize, u32)]| {
            port_rt.poll();
            let r = port_rt.active().expect("녹음 끝점");
            r.begin(k * n as u64, n);
            r.source(LOCAL_SOURCE, 0, &data);
            for &(slot, ssrc) in peers {
                r.source(slot, ssrc, &data);
            }
            r.finish(&data);
        };
        for k in 0..2 {
            feed(&mut port_rt, k, &[(1, 7)]);
        }
        // 쓰기 스레드가 이름 없는 트랙을 만들 때까지 기다린다
        let t0 = Instant::now();
        while rec.stats().map(|s| s.tracks).unwrap_or(0) < 2 && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        rec.set_track_names(HashMap::from([("peer-aaaaaaaaaa".to_string(), "베이스".to_string()), ("peer-b".to_string(), "드럼".to_string())])).unwrap();
        for k in 2..4 {
            feed(&mut port_rt, k, &[(1, 7), (2, 9)]);
        }
        let (sum, _port_rt) = stop_with_rt(&mut rec, port_rt);
        assert!(sum.error.is_none(), "{:?}", sum.error);
        let peers: Vec<(&str, &str)> = sum.tracks.iter().filter(|t| t.kind == TrackKind::Peer).map(|t| (t.name.as_str(), t.file.as_str())).collect();
        assert_eq!(peers, vec![("베이스", "02-베이스.wav"), ("드럼", "03-드럼.wav")]);
        let folder = PathBuf::from(&sum.folder);
        assert!(!folder.join("02-peer-aaa.wav").exists());
        for t in &sum.tracks {
            assert_eq!(data_frames(&folder.join(&t.file)).0, 4 * n, "{}", t.file);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// PR #47 M2: 트랙 하나를 만들지 못해도(같은 이름 폴더가 막음) 나머지 트랙은 끝까지 쓴다.
    #[test]
    fn one_track_create_failure_keeps_others() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("trackfail");
        let opts = RecordOptions {
            include_mix: true,
            track_names: HashMap::from([("peer-a".to_string(), "blocker".to_string())]),
            ..Default::default()
        };
        let st = rec.start(opts, &dir, Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string())), 0).unwrap();
        std::fs::create_dir_all(PathBuf::from(&st.folder).join("02-blocker.wav")).unwrap();
        let n = 240usize;
        let data = vec![0.25f32; n * 2];
        for k in 0..4u64 {
            port_rt.poll();
            let r = port_rt.active().expect("녹음 끝점");
            r.begin(k * n as u64, n);
            r.source(LOCAL_SOURCE, 0, &data);
            r.source(1, 7, &data);
            r.finish(&data);
        }
        let (sum, _port_rt) = stop_with_rt(&mut rec, port_rt);
        let err = sum.error.clone().expect("트랙 실패를 알린다");
        assert!(err.contains("02-blocker.wav"), "{err}");
        let kinds: Vec<TrackKind> = sum.tracks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![TrackKind::Local, TrackKind::Mix]);
        let folder = PathBuf::from(&sum.folder);
        for t in &sum.tracks {
            assert_eq!(data_frames(&folder.join(&t.file)).0, 4 * n, "{}", t.file);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_without_callback_times_out_but_finishes() {
        let (_port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("dead");
        rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
        let sum = rec.stop().unwrap();
        assert_eq!(sum.duration_sec, 0.0);
        assert!(rec.stop().is_err());
        // 콜백이 끝점을 아직 안 받았으므로 다음 시작은 거부된다(링을 두 번 넘기지 않음)
        assert!(rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn full_ring_drops_whole_chunk() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 100); // 링 300프레임
        let dir = tmp("drop");
        rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
        port_rt.poll();
        let r = port_rt.active().unwrap();
        let data = vec![0.1f32; 200];
        let mut pushed = 0;
        for k in 0..100u64 {
            r.begin(k * 100, 100);
            r.source(0, 0, &data);
            r.finish(&data);
            pushed += 1;
        }
        let dropped = r.shared.dropped_frames.load(Ordering::Relaxed);
        assert!(dropped > 0 && dropped < pushed * 100, "{dropped}");
        let (sum, _) = stop_with_rt(&mut rec, port_rt);
        assert_eq!(sum.dropped_frames, dropped);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("a/b:c*?"), "a_b_c__");
        assert_eq!(sanitize_name("  con.txt "), "_con.txt");
        assert_eq!(sanitize_name("trail... "), "trail");
        assert_eq!(sanitize_name("..\\x"), "_x");
        assert_eq!(default_session_name(1_762_819_200_000), "band-rec-20251111-000000Z");
    }
}
