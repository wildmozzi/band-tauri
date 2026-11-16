//! 녹음 마커 큐(P2-4a): 워커 → 쓰기 스레드. RT가 아니다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::Serialize;

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
    pub(super) on: AtomicBool,
    pub(super) q: Mutex<Vec<MarkEvent>>,
    pub(super) lost: AtomicU64,
}

/// 마커 큐 용량(쓰기 스레드가 20ms마다 비운다).
pub(super) const MARK_CAP: usize = 1024;

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

    pub(super) fn arm(&self) {
        let mut q = self.q.lock();
        q.clear();
        q.reserve(MARK_CAP);
        self.lost.store(0, Ordering::Relaxed);
        self.on.store(true, Ordering::Release);
    }
}
