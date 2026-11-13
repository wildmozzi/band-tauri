//! 녹음 RT 링: 출력 콜백이 쓰는 끝점과 핸드오프(설계 문서 1.5절). 할당·락·시스템콜·로그 없음.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::engine::config::MAX_SOURCES;

/// 녹음 링이 버티는 시간(초). 디스크가 이만큼 멈춰도 버리지 않는다.
const RING_SECONDS: usize = 3;
/// 콜백 청크 최소 크기 가정(블록 링 용량 산정).
const MIN_CHUNK_FRAMES: usize = 16;

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

/// 정지 중(락 밖에서 마무리 중)인 녹음 수. 엔진 정지·앱 종료가 그 마무리를 기다린다(PR #47 L4).
#[derive(Debug, Default)]
pub(super) struct InFlight {
    pub(super) n: Mutex<u32>,
    pub(super) cv: parking_lot::Condvar,
}

/// [`StopJob`]이 끝나면(성공·실패·패닉 모두) 줄인다.
pub(super) struct InFlightGuard(pub(super) Arc<InFlight>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut n = self.0.n.lock();
        *n = n.saturating_sub(1);
        self.0.cv.notify_all();
    }
}

/// 출력 콜백이 쓰는 녹음 끝점(녹음 하나당 하나).
pub struct RecordRt {
    pub(super) samples: HeapProd<f32>,
    pub(super) blocks: HeapProd<RecBlock>,
    pub(super) include_mix: bool,
    pub(super) shared: Arc<RecordRtShared>,
    pub(super) cur: RecBlock,
    pub(super) open: bool,
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
    pub(super) fn reclaim(&mut self) -> bool {
        while self.outbox.try_pop().is_some() {
            self.outstanding = false;
        }
        !self.outstanding
    }

    pub(super) fn attach(&mut self, r: RecordRt) -> Result<(), String> {
        if !self.reclaim() {
            return Err("이전 녹음이 아직 정리되지 않았습니다. 잠시 후 다시 시도하세요".into());
        }
        self.detach.store(false, Ordering::Release);
        self.inbox.try_push(r).map_err(|_| "녹음 시작 실패(핸드오프)".to_string())?;
        self.outstanding = true;
        Ok(())
    }

    /// 콜백에 정지를 요청한다(다음 주기 시작에 끝점을 돌려준다). 기다리지 않는다.
    pub(super) fn request_detach(&self) {
        self.detach.store(true, Ordering::Release);
    }
}

pub(super) type Rings = (HeapProd<f32>, HeapCons<f32>, HeapProd<RecBlock>, HeapCons<RecBlock>);

/// 녹음 링(샘플·청크 머리)을 할당한다(비RT, 녹음 시작 때). `prefault`면 콜백에 넘기기 전에 모든 칸을 한 번 써서
/// 페이지를 미리 들여 둔다(P2-4b). `HeapRb`는 초기화하지 않은 큰 블록이라 OS가 페이지를 처음 쓰는 순간에야 붙인다 —
/// 안 하면 첫 몇 초 동안 출력 콜백이 링에 쓸 때마다 커널 페이지 폴트(0으로 채운 페이지 할당)를 RT 스레드에서 겪는다.
pub(super) fn alloc_rings(rate: u32, prefault: bool) -> Rings {
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
