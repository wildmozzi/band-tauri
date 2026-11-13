//! 녹음 쓰기 스레드(`band-record`)와 제어(`Recorder`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

use super::marks::{MarkEvent, MarkKind, MarkSink, MARK_CAP};
use super::rt_ring::{alloc_rings, InFlight, InFlightGuard, RecBlock, RecordPortCtl, RecordRt, RecordRtShared};
use super::sidecar::{default_session_name, public_track, sanitize_name, unique_dir, MarkerTrack, MarkersFile, RecordMarker, RecordOptions, RecordStats, RecordSummary, RecordTrack, TrackKind};
use crate::diag::mask::pseudonymize;
use crate::engine::config::{LOCAL_SOURCE, MAX_SOURCES};
use crate::engine::flac::FlacWriter;
use crate::engine::wav::{RecordFormat, WavWriter};

/// 정지 시 콜백이 녹음 끝점을 돌려주길 기다리는 최대 시간.
const DETACH_WAIT: Duration = Duration::from_millis(1000);

/// 트랙·종류별로 남기는 구간 상한. 넘으면 이벤트 수만 `overflowEvents`에 센다(긴 녹음 내내 끊기는 경우 메모리 상한).
/// 100ms 안의 이벤트는 바로 합치므로, 이만큼 차려면 100ms 넘게 떨어진 끊김이 1만 번 있어야 한다.
const MARK_TRACK_CAP: usize = 10_000;

/// 합치는 중인 구간 하나(절대 타임라인 위치).
#[derive(Debug, Clone, Copy)]
struct MarkIv {
    kind: MarkKind,
    start: u64,
    end: u64,
    frames: u64,
    events: u32,
}

/// 트랙 하나의 마커 누적(PR #62 M2). 종류마다 열린 구간 하나를 두고 들어오는 대로 합치며, `gap` 넘게 떨어지면 닫는다.
/// 메모리는 이벤트 수가 아니라 구간 수에 비례하고 `MARK_TRACK_CAP`으로 막힌다.
#[derive(Debug, Default)]
struct MarkAcc {
    open: [Option<MarkIv>; 3],
    closed: Vec<MarkIv>,
    /// 종류별 닫힌 구간 수(상한 판정용).
    count: [usize; 3],
    overflow: u64,
}

impl MarkAcc {
    fn add(&mut self, kind: MarkKind, start: u64, frames: u64, gap: u64) {
        let span = if kind == MarkKind::Drop { 0 } else { frames };
        let end = start.saturating_add(span);
        let k = kind as usize;
        if let Some(o) = &mut self.open[k] {
            // 열린 구간과 `gap` 안에서 겹치면(순서가 조금 뒤바뀐 것 포함) 늘린다
            if start <= o.end.saturating_add(gap) && end.saturating_add(gap) >= o.start {
                o.start = o.start.min(start);
                o.end = o.end.max(end);
                o.frames = o.frames.saturating_add(frames);
                o.events = o.events.saturating_add(1);
                return;
            }
        }
        if let Some(done) = self.open[k].take() {
            self.close(done);
        }
        self.open[k] = Some(MarkIv { kind, start, end, frames, events: 1 });
    }

    fn close(&mut self, iv: MarkIv) {
        let k = iv.kind as usize;
        if self.count[k] < MARK_TRACK_CAP {
            self.count[k] += 1;
            self.closed.push(iv);
        } else {
            self.overflow += u64::from(iv.events);
        }
    }

    fn intervals(&self) -> impl Iterator<Item = &MarkIv> {
        self.closed.iter().chain(self.open.iter().flatten())
    }
}

/// 같은 종류 이벤트를 이만큼(초) 안이면 한 구간으로 합친다.
const MARK_MERGE_SEC: f64 = 0.1;

fn mark_gap(rate: u32) -> u64 {
    (rate as f64 * MARK_MERGE_SEC) as u64
}
/// 워커 이벤트 위치 허용 오차(초): 소스 링·리샘플러 선행량 추정과 콜백 경계. 실측(PR #62 L1, 5ms 패킷·버퍼 128/256)은
/// 마커가 녹음 파일의 실제 은닉보다 0~236프레임(≤ 4.9ms) 이르고 늦은 쪽은 +3프레임 이하라 이 값 안이다.
const MARK_ACCURACY_SEC: f64 = 0.01;

/// 트랙 파일 쓰기(형식별).
enum TrackWriter {
    Wav(WavWriter),
    Flac(Box<FlacWriter>),
}

impl TrackWriter {
    fn create(path: &Path, fmt: RecordFormat, channels: u16, rate: u32) -> std::io::Result<Self> {
        Ok(match fmt {
            RecordFormat::Flac => TrackWriter::Flac(Box::new(FlacWriter::create(path, channels, rate)?)),
            RecordFormat::Wav24 | RecordFormat::WavF32 => TrackWriter::Wav(WavWriter::create(path, fmt, channels, rate)?),
        })
    }

    fn write(&mut self, s: &[f32]) -> std::io::Result<()> {
        match self {
            TrackWriter::Wav(w) => w.write(s),
            TrackWriter::Flac(w) => w.write(s),
        }
    }

    fn write_silence(&mut self, frames: u64) -> std::io::Result<()> {
        match self {
            TrackWriter::Wav(w) => w.write_silence(frames),
            TrackWriter::Flac(w) => w.write_silence(frames),
        }
    }

    fn data_bytes(&self) -> u64 {
        match self {
            TrackWriter::Wav(w) => w.data_bytes(),
            TrackWriter::Flac(w) => w.data_bytes(),
        }
    }

    fn finish(self) -> std::io::Result<u64> {
        match self {
            TrackWriter::Wav(w) => w.finish(),
            TrackWriter::Flac(w) => (*w).finish(),
        }
    }

    fn finish_best_effort(self) -> u64 {
        match self {
            TrackWriter::Wav(w) => w.finish_best_effort(),
            TrackWriter::Flac(w) => (*w).finish_best_effort(),
        }
    }
}

/// 트랙 하나의 누적 구간(절대 타임라인 위치)을 녹음 기준 구간으로 자르고 다시 합친다(순서가 뒤바뀌어 따로 닫힌 것 정리).
/// `origin`은 녹음 첫 블록 위치, `end`는 녹음 길이.
fn merge_marks(acc: &MarkAcc, origin: u64, end: u64, rate: u32) -> Vec<RecordMarker> {
    let gap = mark_gap(rate);
    let mut v: Vec<MarkIv> = acc
        .intervals()
        .filter_map(|iv| {
            // 녹음 앞(시작 전 이벤트)은 녹음 안으로 자르고, 녹음 뒤는 버린다.
            let s = iv.start.saturating_sub(origin);
            let e = iv.end.saturating_sub(origin).min(end);
            (s < end).then_some(MarkIv { start: s, end: e.max(s), ..*iv })
        })
        .collect();
    v.sort_by_key(|x| (x.start, x.kind as u8));
    let mut out: Vec<RecordMarker> = Vec::new();
    for iv in v {
        if let Some(m) = out.iter_mut().rev().find(|m| m.kind == iv.kind) {
            if iv.start <= m.end + gap {
                m.end = m.end.max(iv.end);
                m.frames = m.frames.saturating_add(iv.frames);
                m.events = m.events.saturating_add(iv.events);
                continue;
            }
        }
        out.push(RecordMarker { kind: iv.kind, start: iv.start, end: iv.end, start_sec: 0.0, end_sec: 0.0, frames: iv.frames, events: iv.events });
    }
    out.sort_by_key(|m| (m.start, m.kind as u8));
    let r = rate.max(1) as f64;
    for m in &mut out {
        m.start_sec = (m.start as f64 / r * 1000.0).round() / 1000.0;
        m.end_sec = (m.end as f64 / r * 1000.0).round() / 1000.0;
    }
    out
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
    _inflight: InFlightGuard,
}

impl StopJob {
    pub fn finish(self) -> Result<RecordSummary, String> {
        let StopJob { s, _inflight } = self;
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
    marks: Arc<MarkSink>,
    inflight: Arc<InFlight>,
}

impl Recorder {
    pub fn new(port: RecordPortCtl, rate: u32) -> Self {
        Self { port, rate, session: None, marks: Arc::new(MarkSink::default()), inflight: Arc::new(InFlight::default()) }
    }

    pub fn is_recording(&self) -> bool {
        self.session.is_some()
    }

    /// 워커가 은닉·버림 마커를 넣는 큐(P2-4a). 엔진 시작 때 워커에 넘긴다.
    pub fn marks(&self) -> Arc<MarkSink> {
        self.marks.clone()
    }

    /// 락 밖에서 마무리 중인 정지([`StopJob`])가 모두 끝날 때까지 기다린다(최대 `timeout`). 반환: 다 끝났다.
    /// 이 함수는 `Recorder` 락을 잡은 채 부르지 말 것 — [`Recorder::waiter`]로 받은 것을 락 밖에서 부른다.
    pub fn waiter(&self) -> impl Fn(Duration) -> bool + Send + 'static {
        let f = self.inflight.clone();
        move |timeout| {
            let deadline = Instant::now() + timeout;
            let mut n = f.n.lock();
            while *n > 0 {
                if f.cv.wait_until(&mut n, deadline).timed_out() {
                    return *n == 0;
                }
            }
            true
        }
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

        let (s_p, s_c, b_p, b_c) = alloc_rings(rate, true);
        let rt_shared = Arc::new(RecordRtShared::default());
        let rt = RecordRt { samples: s_p, blocks: b_p, include_mix: opts.include_mix, shared: rt_shared.clone(), cur: RecBlock::default(), open: false };
        if let Err(e) = self.port.attach(rt) {
            std::fs::remove_dir(&folder).ok();
            return Err(e);
        }

        let shared = Arc::new(WriterShared { names: Mutex::new(opts.track_names.clone()), ..Default::default() });
        let stop = Arc::new(AtomicBool::new(false));
        self.marks.arm();
        let w = Writer::new(&opts, folder.clone(), rate, s_c, b_c, resolve, shared.clone(), rt_shared.clone(), self.marks.clone(), now_ms);
        let st = stop.clone();
        let handle = std::thread::Builder::new().name("band-record".into()).spawn(move || w.run(&st));
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                self.marks.on.store(false, Ordering::Release);
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
        // 이후 마커는 녹음 끝 뒤라 받지 않는다(이미 큐에 있는 것은 쓰기 스레드가 마지막에 비운다).
        self.marks.on.store(false, Ordering::Release);
        *self.inflight.n.lock() += 1;
        Ok(StopJob { s, _inflight: InFlightGuard(self.inflight.clone()) })
    }

    /// 콜백이 돌려준 녹음 끝점(링)을 거둬 해제한다.
    /// 녹음 중 피어 트랙 이름을 더한다(늦게 들어온 연주자, PR #49 M1). 앞으로 만들 트랙은 이 이름을 쓰고,
    /// 이름 없이(peerId 가명) 만든 트랙은 정지할 때 이 이름으로 파일을 바꾼다. 녹음 중이 아니면 에러.
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
    /// 피어 트랙을 이름 없이(peerId 가명) 만들었다 → 정지 때 이름이 생겼으면 파일을 바꾼다.
    unnamed: bool,
    w: Option<TrackWriter>,
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
    /// 워커 마커 큐와 바꿔 쓰는 버퍼(용량 `MARK_CAP`).
    sink: Arc<MarkSink>,
    mark_swap: Vec<MarkEvent>,
    /// 트랙 키별 마커 누적(들어오는 대로 합침, PR #62 M2).
    marks: Vec<(TrackKey, MarkAcc)>,
    /// ssrc → `marks` 인덱스(이벤트마다 트랙 키를 만들지 않게).
    mark_ssrc: HashMap<u32, usize>,
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
        sink: Arc<MarkSink>,
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
            sink,
            mark_swap: Vec::with_capacity(MARK_CAP),
            marks: Vec::new(),
            mark_ssrc: HashMap::new(),
        }
    }

    /// 피어 ssrc의 트랙 키(트랙을 만들지는 않는다).
    fn peer_key(&self, ssrc: u32) -> TrackKey {
        if let Some(t) = self.by_ssrc.get(&ssrc).and_then(|&i| self.tracks.get(i)) {
            return t.key.clone();
        }
        TrackKey::Peer((self.resolve)(ssrc).unwrap_or_else(|| format!("ssrc-{ssrc:08x}")))
    }

    /// 워커 마커 큐를 비워 모은다(큐와 버퍼를 바꿔 워커 쪽은 할당 없이 계속 넣는다).
    fn drain_marks(&mut self) {
        {
            let mut q = self.sink.q.lock();
            std::mem::swap(&mut *q, &mut self.mark_swap);
        }
        let evs = std::mem::take(&mut self.mark_swap);
        for e in &evs {
            let i = self.mark_peer(e.ssrc);
            self.add_mark(i, e.kind, e.start, e.frames);
        }
        self.mark_swap = evs;
        self.mark_swap.clear();
    }

    /// 트랙 키의 마커 누적 인덱스(없으면 만든다). 트랙은 몇 개뿐이라 선형 탐색.
    fn mark_slot(&mut self, key: TrackKey) -> usize {
        if let Some(i) = self.marks.iter().position(|(k, _)| *k == key) {
            return i;
        }
        self.marks.push((key, MarkAcc::default()));
        self.marks.len() - 1
    }

    /// 피어 ssrc의 마커 누적 인덱스. 트랙 키(peerId)는 ssrc마다 처음 한 번만 정한다.
    fn mark_peer(&mut self, ssrc: u32) -> usize {
        if let Some(&i) = self.mark_ssrc.get(&ssrc) {
            return i;
        }
        let i = self.mark_slot(self.peer_key(ssrc));
        self.mark_ssrc.insert(ssrc, i);
        i
    }

    fn add_mark(&mut self, i: usize, kind: MarkKind, start: u64, frames: u64) {
        let gap = mark_gap(self.rate);
        self.marks[i].1.add(kind, start, frames, gap);
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
        self.drain_marks();
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
                // 언더런 마커(P2-4a): 콜백이 이 청크 끝에서 0으로 채운 만큼
                let short = u64::from(b.short[slot]).min(b.frames as u64);
                if short > 0 {
                    let i = if slot == LOCAL_SOURCE { self.mark_slot(TrackKey::Local) } else { self.mark_peer(b.occ[slot]) };
                    self.add_mark(i, MarkKind::Underrun, b.pos + b.frames as u64 - short, short);
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
        // 닉네임이 없으면 사이드카와 같은 가명(원문 id 앞부분을 파일 이름에 남기지 않음, PR #62 L5)
        let name = named.unwrap_or_else(|| pseudonymize(&id));
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
        let ext = self.format.extension();
        let file = match kind {
            TrackKind::Mix => format!("00-mix.{ext}"),
            _ => format!("{idx:02}-{label}.{ext}"),
        };
        match TrackWriter::create(&self.folder.join(&file), self.format, 2, self.rate) {
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
            let file = format!("{:02}-{clean}.{}", t.idx, self.format.extension());
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

    /// 트랙별 마커를 합쳐 `markers.json`에 쓴다(트랙 파일 이름이 확정된 뒤). 반환: (구간 수, 파일 이름).
    fn write_markers(&mut self, end: u64) -> (u32, Option<String>) {
        self.drain_marks();
        let origin = self.origin.unwrap_or(0);
        let tracks: Vec<MarkerTrack> = self
            .tracks
            .iter()
            .map(|t| MarkerTrack {
                file: t.meta.file.clone(),
                name: t.meta.name.clone(),
                kind: t.meta.kind,
                peer_id: t.meta.peer_id.as_deref().map(pseudonymize),
                markers: match self.marks.iter().find(|(k, _)| *k == t.key) {
                    Some((_, acc)) if t.key != TrackKey::Mix => merge_marks(acc, origin, end, self.rate),
                    _ => Vec::new(),
                },
            })
            .collect();
        let count = tracks.iter().map(|t| t.markers.len() as u32).sum();
        let file = MarkersFile {
            sample_rate: self.rate,
            merge_gap_frames: mark_gap(self.rate),
            accuracy_frames: (self.rate as f64 * MARK_ACCURACY_SEC) as u64,
            lost_events: self.sink.lost.load(Ordering::Relaxed),
            overflow_events: self.marks.iter().map(|(_, a)| a.overflow).sum(),
            tracks,
        };
        let name = "markers.json";
        let res = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string()).and_then(|j| std::fs::write(self.folder.join(name), j).map_err(|e| e.to_string()));
        match res {
            Ok(()) => (count, Some(name.to_string())),
            Err(e) => {
                log::warn!("markers.json 저장 실패: {e}");
                (count, None)
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
        let (markers, markers_file) = self.write_markers(end);
        let summary = RecordSummary {
            folder: self.folder.to_string_lossy().into_owned(),
            format: self.format,
            sample_rate: self.rate,
            channels: 2,
            duration_sec: end as f64 / self.rate.max(1) as f64,
            bytes,
            dropped_frames: self.rt_shared.dropped_frames.load(Ordering::Relaxed),
            started_at_ms: self.started_at_ms,
            tracks: self.tracks.iter().map(|t| public_track(&t.meta)).collect(),
            error: err,
            markers,
            markers_file,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #62 M2: 마커는 들어오는 대로 합쳐 메모리가 이벤트 수가 아니라 구간 수에 비례하고, 구간은 상한에서 멈춘다.
    #[test]
    fn mark_acc_merges_online_and_caps() {
        let gap = mark_gap(48_000);
        let mut a = MarkAcc::default();
        // 1시간 내내 128프레임 청크마다 언더런(135만 이벤트) → 열린 구간 하나
        let n = 48_000u64 * 3600 / 128;
        for k in 0..n {
            a.add(MarkKind::Underrun, k * 128 + 100, 28, gap);
        }
        assert!(a.closed.is_empty() && a.closed.capacity() == 0, "닫힌 구간이 쌓이지 않는다");
        let m = merge_marks(&a, 0, n * 128, 48_000);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].start, m[0].end, m[0].frames, u64::from(m[0].events)), (100, (n - 1) * 128 + 128, n * 28, n));
        // 순서가 조금 뒤바뀐 이벤트도 열린 구간에 합친다
        let mut b = MarkAcc::default();
        b.add(MarkKind::Conceal, 10_000, 480, gap);
        b.add(MarkKind::Conceal, 9_000, 480, gap);
        b.add(MarkKind::Drop, 9_500, 960, gap);
        assert_eq!(b.closed.len(), 0);
        let m = merge_marks(&b, 0, 1 << 30, 48_000);
        assert_eq!(m.iter().map(|m| (m.kind, m.start, m.end, m.events)).collect::<Vec<_>>(), vec![(MarkKind::Conceal, 9_000, 10_480, 2), (MarkKind::Drop, 9_500, 9_500, 1)]);
        // 100ms 넘게 떨어진 끊김이 상한보다 많으면 상한까지만 구간으로 남기고 나머지는 센다
        let mut c = MarkAcc::default();
        let extra = 25u64;
        for k in 0..(MARK_TRACK_CAP as u64 + 1 + extra) {
            c.add(MarkKind::Conceal, k * gap * 3, 480, gap);
        }
        assert_eq!(c.closed.len(), MARK_TRACK_CAP);
        assert_eq!(c.overflow, extra);
        assert_eq!(merge_marks(&c, 0, u64::MAX, 48_000).len(), MARK_TRACK_CAP + 1, "닫힌 상한 + 열린 하나");
    }
}
