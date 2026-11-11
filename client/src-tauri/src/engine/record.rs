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
use crate::diag::mask::pseudonymize;
use super::flac::FlacWriter;
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
    /// 슬롯별로 이 청크 끝에서 링이 모자라 0으로 채운 프레임(재생 중 언더런, P2-4a 마커).
    pub short: [u16; MAX_SOURCES],
}

/// RT와 제어 쪽이 함께 보는 녹음 통계.
#[derive(Debug, Default)]
pub struct RecordRtShared {
    /// 녹음 링이 모자라 버린 프레임(녹음 품질 지표). 그 자리는 무음으로 채워진다.
    pub dropped_frames: AtomicU64,
    /// 콜백이 이 녹음 끝점을 돌려줬다(그 뒤로는 링에 쓰지 않음). 쓰기 스레드는 이것을 보고 남은 것을 비운다.
    pub detached: AtomicBool,
}

/// 녹음 마커 종류(P2-4a, `markers.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkKind {
    /// 지터 버퍼 은닉(손실 PLC·FEC, 늦은 패킷 자리의 언더런 은닉). 워커가 은닉 카운터 증가분을 기록한다.
    Conceal,
    /// 버림: 지연을 줄이려 패킷 프레임을 버렸거나(`droppedFrames`), 출력 언더런 뒤 스트림을 건너뜀. 길이 없는 지점.
    Drop,
    /// 출력 콜백이 소스 링 부족으로 0을 채움(재생 중 언더런). 청크 머리에 있어 위치가 정확하다.
    Underrun,
}

/// 워커가 녹음 쓰기 스레드에 넘기는 마커 이벤트(위치는 출력 타임라인 `out_played_frames` 기준 장치 프레임).
#[derive(Debug, Clone, Copy)]
pub struct MarkEvent {
    pub ssrc: u32,
    pub kind: MarkKind,
    pub start: u64,
    pub frames: u64,
}

/// 워커 → 녹음 쓰기 스레드 마커 큐. 녹음 중에만 켜지고, 시작할 때 용량을 미리 잡는다. 워커는 `try_lock`으로
/// 넣기만 하고(가득 차거나 잠겨 있으면 버리고 `lost`만 올림, 할당 없음), 쓰기 스레드가 20ms마다 통째로 바꿔 간다.
/// 출력 콜백(RT)은 이것을 쓰지 않는다(언더런은 청크 머리 `short`로 온다).
#[derive(Debug, Default)]
pub struct MarkSink {
    on: AtomicBool,
    q: Mutex<Vec<MarkEvent>>,
    lost: AtomicU64,
}

/// 마커 큐 용량(쓰기 스레드가 20ms마다 비운다).
const MARK_CAP: usize = 1024;

impl MarkSink {
    /// 녹음 중(마커를 모으는 중)인가.
    #[inline]
    pub fn active(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    pub fn push(&self, e: MarkEvent) {
        if let Some(mut q) = self.q.try_lock() {
            if q.len() < q.capacity() {
                q.push(e);
                return;
            }
        }
        self.lost.fetch_add(1, Ordering::Relaxed);
    }

    fn arm(&self) {
        let mut q = self.q.lock();
        q.clear();
        q.reserve(MARK_CAP);
        self.lost.store(0, Ordering::Relaxed);
        self.on.store(true, Ordering::Release);
    }
}

/// 정지 중(락 밖에서 마무리 중)인 녹음 수. 엔진 정지·앱 종료가 그 마무리를 기다린다(PR #47 L4).
#[derive(Debug, Default)]
struct InFlight {
    n: Mutex<u32>,
    cv: parking_lot::Condvar,
}

/// [`StopJob`]이 끝나면(성공·실패·패닉 모두) 줄인다.
struct InFlightGuard(Arc<InFlight>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut n = self.0.n.lock();
        *n = n.saturating_sub(1);
        self.0.cv.notify_all();
    }
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
        self.cur = RecBlock { pos, frames: n as u32, mask: 0, mix: false, occ: [0; MAX_SOURCES], short: [0; MAX_SOURCES] };
    }

    /// RT: 슬롯 하나가 이 청크에 재생한 샘플(`n × 2`, 게인 전). `short`: 그중 끝에서 링 부족으로 0을 채운 프레임.
    #[inline]
    pub fn source(&mut self, slot: usize, occupant: u32, data: &[f32], short: usize) {
        if !self.open || slot >= MAX_SOURCES {
            return;
        }
        self.samples.push_slice(data);
        self.cur.mask |= 1 << slot;
        if let Some(o) = self.cur.occ.get_mut(slot) {
            *o = occupant;
        }
        if let Some(s) = self.cur.short.get_mut(slot) {
            *s = short.min(u16::MAX as usize) as u16;
        }
    }

    /// RT: 청크 끝. 믹스(클릭 없는 탭 버스)를 붙이고 머리를 넣는다. 콜백은 청크마다 반드시 부른다(PR #47 L1).
    /// 믹스 길이가 청크와 다르면 믹스만 빼고 머리를 넣는다(그 자리는 쓰기 스레드가 무음으로 채움 — 링이 어긋나지 않음).
    #[inline]
    pub fn finish(&mut self, mix: &[f32]) {
        if !self.open {
            return;
        }
        if self.include_mix && mix.len() == self.cur.frames as usize * 2 {
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

type Rings = (HeapProd<f32>, HeapCons<f32>, HeapProd<RecBlock>, HeapCons<RecBlock>);

/// 녹음 링(샘플·청크 머리)을 할당한다(비RT, 녹음 시작 때). `prefault`면 콜백에 넘기기 전에 모든 칸을 한 번 써서
/// 페이지를 미리 들여 둔다(P2-4b). `HeapRb`는 초기화하지 않은 큰 블록이라 OS가 페이지를 처음 쓰는 순간에야 붙인다 —
/// 안 하면 첫 몇 초 동안 출력 콜백이 링에 쓸 때마다 커널 페이지 폴트(0으로 채운 페이지 할당)를 RT 스레드에서 겪는다.
fn alloc_rings(rate: u32, prefault: bool) -> Rings {
    let frames_cap = rate as usize * RING_SECONDS;
    let (mut s_p, mut s_c) = HeapRb::<f32>::new(frames_cap * 2 * (MAX_SOURCES + 1)).split();
    let (mut b_p, mut b_c) = HeapRb::<RecBlock>::new(frames_cap / MIN_CHUNK_FRAMES + 16).split();
    if prefault {
        touch_all(&mut s_p, &mut s_c);
        touch_all(&mut b_p, &mut b_c);
    }
    (s_p, s_c, b_p, b_c)
}

/// 링 전체를 기본값으로 한 바퀴 채우고 바로 버린다(모든 페이지를 건드림). 끝나면 링은 다시 비어 있다.
fn touch_all<T: Copy + Default>(p: &mut HeapProd<T>, c: &mut HeapCons<T>) {
    let fill = [T::default(); 1024];
    let mut left = p.capacity().get();
    while left > 0 {
        let k = p.push_slice(&fill[..left.min(fill.len())]);
        c.skip(k);
        if k == 0 {
            break;
        }
        left -= k;
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
    /// 피어별 트랙 이름(peerId → 표시 이름). 없으면 peerId 가명(`id-xxxxxxxx`, PR #62 L5).
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
    /// 피어 id 가명(`id-` + 해시 8자, 진단 내보내기 `diag::mask::pseudonymize`와 같은 값 — 같은 id는 같은 값, PR #62 L5).
    /// `session.json`·`markers.json`·`engine_record_stop` 반환 모두 가명이다. 원문은 녹음 중 쓰기 스레드 안에만 있다.
    pub peer_id: Option<String>,
}

/// 쓰기 스레드 안의 트랙 메타 → 밖으로 내보내는 형태(피어 id 가명화). 표시 이름(닉네임)·파일 이름은 그대로 둔다.
fn public_track(t: &RecordTrack) -> RecordTrack {
    RecordTrack { peer_id: t.peer_id.as_deref().map(pseudonymize), ..t.clone() }
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
    /// 은닉·버림·언더런 구간 수(모든 트랙 합, 가까운 것은 합침). 자세한 위치는 `markersFile`(P2-4a).
    pub markers: u32,
    /// 마커 사이드카 파일 이름(세션 폴더 안, 보통 `markers.json`). 쓰지 못했으면 None.
    pub markers_file: Option<String>,
}

/// `markers.json`의 구간 하나. 위치는 녹음 시작 기준 프레임(트랙 파일의 샘플 위치와 같음).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordMarker {
    pub kind: MarkKind,
    pub start: u64,
    /// 끝(제외). `drop`은 길이 없는 지점이라 `start`와 같다.
    pub end: u64,
    pub start_sec: f64,
    pub end_sec: f64,
    /// 은닉·0 채움·버린 오디오의 양(프레임, 합친 이벤트의 합).
    pub frames: u64,
    /// 합친 이벤트 수.
    pub events: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerTrack {
    pub file: String,
    pub name: String,
    pub kind: TrackKind,
    pub peer_id: Option<String>,
    pub markers: Vec<RecordMarker>,
}

/// `markers.json` 전체.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkersFile {
    pub sample_rate: u32,
    /// 같은 종류 이벤트를 이 간격(프레임) 안이면 한 구간으로 합쳤다.
    pub merge_gap_frames: u64,
    /// 워커 이벤트 위치 허용 오차(프레임, ±). 언더런은 청크 머리라 정확하다. 은닉 마커는 실측으로 실제 위치보다 0~1패킷
    /// 이르다(PR #62 L1, `tests/record.rs` `concealment_marker_positions_match_recorded_concealment`).
    pub accuracy_frames: u64,
    /// 큐가 가득 차 잃은 워커 이벤트 수.
    pub lost_events: u64,
    /// 트랙·종류별 구간 상한(`MARK_TRACK_CAP`)을 넘어 구간으로 남기지 못하고 세기만 한 이벤트 수(PR #62 M2).
    pub overflow_events: u64,
    pub tracks: Vec<MarkerTrack>,
}

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

    /// 24비트 스테레오 WAV를 표준 파서(hound)로 읽는다(EXTENSIBLE 헤더 확인을 겸함, PR #47 L3). 반환: (프레임 수, 인터리브 샘플).
    fn data_frames(path: &Path) -> (usize, Vec<i32>) {
        let mut r = hound::WavReader::open(path).unwrap();
        assert_eq!((r.spec().channels, r.spec().bits_per_sample), (2, 24));
        let s: Vec<i32> = r.samples::<i32>().map(|v| v.unwrap()).collect();
        (s.len() / 2, s)
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
            r.source(LOCAL_SOURCE, 0, &local, 0);
            if k >= 2 {
                r.source(1, 7, &peer, 0);
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
        // 사이드카·반환의 피어 id는 진단 내보내기와 같은 가명(PR #62 L5), 닉네임은 그대로
        assert_eq!(gt.peer_id.as_deref(), Some(pseudonymize("peer-a").as_str()));
        let sj = std::fs::read_to_string(folder.join("session.json")).unwrap();
        assert!(!sj.contains("peer-a") && sj.contains(&pseudonymize("peer-a")) && sj.contains("기타"), "{sj}");
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
            r.source(LOCAL_SOURCE, 0, &data, 0);
            for &(slot, ssrc) in peers {
                r.source(slot, ssrc, &data, 0);
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
        assert!(!folder.join(format!("02-{}.wav", pseudonymize("peer-aaaaaaaaaa"))).exists());
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
            r.source(LOCAL_SOURCE, 0, &data, 0);
            r.source(1, 7, &data, 0);
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
            r.source(0, 0, &data, 0);
            r.finish(&data);
            pushed += 1;
        }
        let dropped = r.shared.dropped_frames.load(Ordering::Relaxed);
        assert!(dropped > 0 && dropped < pushed * 100, "{dropped}");
        let (sum, _) = stop_with_rt(&mut rec, port_rt);
        assert_eq!(sum.dropped_frames, dropped);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-4b: 미리 건드린 링은 비어 있고(가득 빈 자리) 그대로 RT 쓰기·쓰기 스레드 읽기에 쓸 수 있다.
    #[test]
    fn prefaulted_rings_start_empty() {
        let (mut s_p, mut s_c, mut b_p, mut b_c) = alloc_rings(100, true);
        assert_eq!(s_p.vacant_len(), s_p.capacity().get());
        assert_eq!(s_c.occupied_len(), 0);
        assert_eq!(b_p.vacant_len(), b_p.capacity().get());
        assert_eq!(b_c.occupied_len(), 0);
        let data: Vec<f32> = (0..64).map(|i| i as f32).collect();
        assert_eq!(s_p.push_slice(&data), 64);
        let mut out = [0f32; 64];
        assert_eq!(s_c.pop_slice(&mut out), 64);
        assert_eq!(&out[..], &data[..]);
        assert!(b_p.try_push(RecBlock { pos: 7, frames: 3, ..Default::default() }).is_ok());
        assert_eq!(b_c.try_pop().map(|b| (b.pos, b.frames)), Some((7, 3)));
    }

    /// P2-4b 측정: 출력 콜백이 녹음 링에 쓰는 비용 — 새로 할당한 링(첫 바퀴 = 페이지 폴트) vs 미리 건드린 링.
    /// 48k, 128프레임 청크, 슬롯 2개 + 믹스, 링 한 바퀴 넘게. `cargo test --release --lib prefault_rt_write_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "측정(벽시계)"]
    fn prefault_rt_write_cost() {
        for prefault in [false, true, false, true] {
            let t_alloc = Instant::now();
            let (s_p, mut s_c, b_p, mut b_c) = alloc_rings(48_000, prefault);
            let alloc_ms = t_alloc.elapsed().as_secs_f64() * 1e3;
            let shared = Arc::new(RecordRtShared::default());
            let mut r = RecordRt { samples: s_p, blocks: b_p, include_mix: true, shared: shared.clone(), cur: RecBlock::default(), open: false };
            let n = 128usize;
            let data = vec![0.1f32; n * 2];
            let chunks = s_c.capacity().get() / (n * 2 * 3) + 200;
            let (mut total, mut max) = (Duration::ZERO, Duration::ZERO);
            let mut over_20us = 0;
            for k in 0..chunks {
                let t = Instant::now();
                r.begin(k as u64 * n as u64, n);
                r.source(0, 0, &data, 0);
                r.source(1, 7, &data, 0);
                r.finish(&data);
                let d = t.elapsed();
                total += d;
                max = max.max(d);
                over_20us += usize::from(d > Duration::from_micros(20));
                s_c.skip(s_c.occupied_len());
                b_c.skip(b_c.occupied_len());
            }
            assert_eq!(shared.dropped_frames.load(Ordering::Relaxed), 0);
            println!(
                "prefault={prefault}: 준비 {alloc_ms:.2}ms, 청크 {chunks}개 RT 쓰기 합 {:.2}ms 평균 {:.2}us 최대 {:.1}us, 20us 넘은 청크 {over_20us}",
                total.as_secs_f64() * 1e3,
                total.as_secs_f64() * 1e6 / chunks as f64,
                max.as_secs_f64() * 1e6
            );
        }
    }

    /// P2-4a: FLAC 녹음 — 트랙 파일이 `.flac`이고 표준 디코더(claxon)로 열리며, 타임라인 정렬(늦은 피어 앞 무음)과
    /// 이름 바꾸기가 WAV와 같다.
    #[test]
    fn flac_tracks_decode_and_align() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("flac");
        let opts = RecordOptions { format: RecordFormat::Flac, include_mix: true, ..Default::default() };
        rec.start(opts, &dir, Box::new(|ssrc| (ssrc == 7).then(|| "peer-a".to_string())), 0).unwrap();
        let n = 240usize;
        let local = vec![0.25f32; n * 2];
        let peer = vec![-0.5f32; n * 2];
        for k in 0..40u64 {
            port_rt.poll();
            let r = port_rt.active().expect("녹음 끝점");
            r.begin(k * n as u64, n);
            r.source(LOCAL_SOURCE, 0, &local, 0);
            if k >= 20 {
                r.source(1, 7, &peer, 0);
            }
            r.finish(&local);
        }
        rec.set_track_names(HashMap::from([("peer-a".to_string(), "기타".to_string())])).unwrap();
        let (sum, _p) = stop_with_rt(&mut rec, port_rt);
        assert!(sum.error.is_none(), "{:?}", sum.error);
        let files: Vec<&str> = sum.tracks.iter().map(|t| t.file.as_str()).collect();
        assert_eq!(files, vec!["01-me.flac", "00-mix.flac", "03-기타.flac"]);
        let folder = PathBuf::from(&sum.folder);
        for t in &sum.tracks {
            let mut r = claxon::FlacReader::open(folder.join(&t.file)).expect("claxon으로 열린다");
            assert_eq!(r.streaminfo().samples, Some(40 * n as u64), "{}", t.file);
            assert_eq!(r.streaminfo().bits_per_sample, 24);
            let s: Vec<i32> = r.samples().map(|x| x.unwrap()).collect();
            assert_eq!(s.len(), 40 * n * 2);
            if t.kind == TrackKind::Peer {
                assert!(s[..20 * n * 2].iter().all(|&v| v == 0), "늦은 피어 앞은 무음");
                assert!(s[20 * n * 2..].iter().all(|&v| v == -4_194_304));
            } else {
                assert!(s.iter().all(|&v| v == 2_097_152));
            }
        }
        assert!(sum.bytes > 0 && sum.bytes < 40 * n as u64 * 6 * 3, "{}", sum.bytes);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-4a: 언더런(청크 머리 `short`)과 워커 마커(은닉·버림)가 트랙별 구간으로 `markers.json`에 남는다.
    /// 가까운 같은 종류는 합치고, 녹음 시작 기준 프레임 위치이며, 녹음 밖 이벤트는 버린다.
    #[test]
    fn markers_sidecar_maps_events_to_tracks() {
        let (mut port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let marks = rec.marks();
        assert!(!marks.active());
        let dir = tmp("marks");
        let resolve: PeerResolver = Box::new(|ssrc| match ssrc {
            7 => Some("peer-a".to_string()),
            9 => Some("peer-b".to_string()),
            _ => None,
        });
        let opts = RecordOptions { include_mix: true, track_names: HashMap::from([("peer-a".to_string(), "기타".to_string())]), ..Default::default() };
        rec.start(opts, &dir, resolve, 0).unwrap();
        assert!(marks.active());
        let base = 500_000u64;
        let n = 480usize;
        let data = vec![0.1f32; n * 2];
        for k in 0..200u64 {
            port_rt.poll();
            let r = port_rt.active().expect("녹음 끝점");
            r.begin(base + k * n as u64, n);
            r.source(LOCAL_SOURCE, 0, &data, if k == 10 { 100 } else { 0 });
            // 피어 a: 50번째 청크 끝 30프레임 언더런
            r.source(1, 7, &data, if k == 50 { 30 } else { 0 });
            r.source(2, 9, &data, 0);
            r.finish(&data);
        }
        let at = |k: u64| base + k * n as u64;
        // 피어 a 은닉 두 번(1.0초 근처, 20ms 간격 → 한 구간) + 멀리 하나, 피어 b 버림, 녹음 앞·뒤 이벤트
        marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(100), frames: 480 });
        marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(102), frames: 480 });
        marks.push(MarkEvent { ssrc: 7, kind: MarkKind::Conceal, start: at(150), frames: 240 });
        marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Drop, start: at(120), frames: 960 });
        marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Conceal, start: at(500), frames: 480 });
        marks.push(MarkEvent { ssrc: 9, kind: MarkKind::Conceal, start: base - 1000, frames: 1480 });
        let (sum, _p) = stop_with_rt(&mut rec, port_rt);
        assert!(!marks.active(), "정지하면 마커를 받지 않는다");
        assert!(sum.error.is_none(), "{:?}", sum.error);
        assert_eq!(sum.markers_file.as_deref(), Some("markers.json"));
        let folder = PathBuf::from(&sum.folder);
        let j: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("markers.json")).unwrap()).unwrap();
        assert_eq!(j["sampleRate"], 48_000);
        assert_eq!(j["lostEvents"], 0);
        assert_eq!(j["overflowEvents"], 0);
        let track = |file: &str| j["tracks"].as_array().unwrap().iter().find(|t| t["file"] == file).cloned().unwrap_or_else(|| panic!("{file}: {j}"));
        let ms = |t: &serde_json::Value| -> Vec<(String, u64, u64, u64, u64)> {
            t["markers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| (m["kind"].as_str().unwrap().to_string(), m["start"].as_u64().unwrap(), m["end"].as_u64().unwrap(), m["frames"].as_u64().unwrap(), m["events"].as_u64().unwrap()))
                .collect()
        };
        let nn = n as u64;
        assert_eq!(ms(&track("01-me.wav")), vec![("underrun".into(), 11 * nn - 100, 11 * nn, 100, 1)]);
        let a = track("02-기타.wav");
        assert_eq!(a["peerId"], pseudonymize("peer-a"));
        assert_eq!(
            ms(&a),
            vec![("underrun".into(), 51 * nn - 30, 51 * nn, 30, 1), ("conceal".into(), 100 * nn, 103 * nn, 960, 2), ("conceal".into(), 150 * nn, 150 * nn + 240, 240, 1),]
        );
        assert!((a["markers"][1]["startSec"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // 피어 b: 녹음 앞 은닉은 0부터 잘리고, 녹음 뒤(500청크)는 버린다. 버림은 길이 없는 지점.
        // 닉네임이 없는 피어 b는 트랙 이름·파일도 가명(원문 id 앞부분을 남기지 않음, PR #62 L5)
        let b = track(&format!("03-{}.wav", pseudonymize("peer-b")));
        assert_eq!(b["name"], pseudonymize("peer-b"));
        assert_eq!(b["peerId"], pseudonymize("peer-b"));
        let raw = std::fs::read_to_string(folder.join("markers.json")).unwrap() + &std::fs::read_to_string(folder.join("session.json")).unwrap();
        assert!(!raw.contains("peer-a") && !raw.contains("peer-b"), "사이드카에 피어 id 원문이 없다");
        assert_eq!(ms(&b), vec![("conceal".into(), 0, 480, 1480, 1), ("drop".into(), 120 * nn, 120 * nn, 960, 1)]);
        assert!(ms(&track("00-mix.wav")).is_empty(), "믹스 트랙에는 마커가 없다");
        assert_eq!(sum.markers, 6);
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// PR #47 L4: 락 밖에서 마무리 중인 정지가 있으면 `waiter`가 그것이 끝날 때까지 기다린다.
    #[test]
    fn waiter_waits_for_inflight_stop() {
        let (_port_rt, ctl) = record_port();
        let mut rec = Recorder::new(ctl, 48_000);
        let dir = tmp("inflight");
        let wait = rec.waiter();
        assert!(wait(Duration::from_millis(1)), "정지 중인 것이 없으면 바로 끝");
        rec.start(RecordOptions::default(), &dir, Box::new(|_| None), 0).unwrap();
        let job = rec.begin_stop().unwrap();
        assert!(!wait(Duration::from_millis(20)), "마무리 전에는 기다린다");
        let h = std::thread::spawn(move || job.finish()); // 콜백이 없어 1초 뒤 시간 초과로 마무리
        assert!(wait(Duration::from_secs(5)));
        assert!(h.join().unwrap().is_ok());
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
