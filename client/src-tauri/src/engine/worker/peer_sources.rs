//! 피어 소스 단계(엔진 ②b): 피어 미디어 표(명령 큐로만 바뀐다)·소스 슬롯 배정·피어별 공급.
//!
//! 한 반복에 하는 일(순서가 조건이다 — `docs/overhaul/engine-stage2b-design.md` 5.2절 J1~J7, M1):
//! 1. 미디어 명령 큐(추가·제거)를 끝까지 비운다 → 프로필을 한 번 읽는다 → 표와 슬롯을 맞춘다.
//! 2. 피어마다(표 순서): `ingest`(그 피어의 수신 큐 비우기 → 재시작 → 프로필) → 슬롯 게인·레벨 →
//!    `PeerFeed::service` → 녹음 마커 → `publish`. 슬롯이 없는 피어도 `ingest`·`publish`는 한다.
//!
//! 피어 하나의 공급 규칙은 `peer_feed.rs`(`PeerFeed`)다.
//! 바뀌는 이유: 피어 수명(추가·제거·슬롯), 공급 순서, 녹음 마커. 모르는 것: net, 코덱 설정, 로컬 경로.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ringbuf::HeapProd;

use super::{FeedTiming, PeerFeed, WorkerStats, OUT_CHUNK};
use crate::engine::config::INTERNAL_RATE;
use crate::engine::measure::EchoProbe;
use crate::engine::peer_media::{MediaPortRt, MediaSettings, PeerMedia, MEDIA_CMD_CAPACITY};
use crate::engine::record::{MarkEvent, MarkKind, MarkSink};
use crate::engine::rt::{SourceBank, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN, SLOT_FREE};
use crate::engine::stats::AudioCounters;
use crate::net::txlat::LockWait;

/// `service` 한 번의 결과.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PeerRound {
    /// 은닉 마감이 가까운 피어가 있다(조정자가 짧게 자야 한다).
    pub(crate) urgent: bool,
}

/// 피어 슬롯(1..`MAX_SOURCES`)의 소스 링 생산자. 로컬 것(슬롯 0)은 `LocalPath`가 가지므로 슬롯 번호로만 접근한다.
struct PeerProds(Vec<HeapProd<f32>>);

impl PeerProds {
    fn get_mut(&mut self, slot: usize) -> Option<&mut HeapProd<f32>> {
        self.0.get_mut(slot.checked_sub(1)?)
    }
}

/// `PeerSources::new` 입력(소유권이 넘어오는 끝과 공유 핸들).
pub(crate) struct PeerSourcesParts {
    /// 슬롯 1.. 의 생산자(슬롯 번호 순).
    pub(crate) prods: Vec<HeapProd<f32>>,
    pub(crate) port: MediaPortRt,
    pub(crate) settings: Arc<MediaSettings>,
    pub(crate) rec_marks: Arc<MarkSink>,
    /// 에코 측정 상태(재생 시각 탭). ②b-5에서 조정자의 `EchoRt`가 빌려 주는 인자로 바뀐다.
    pub(crate) probe: Arc<parking_lot::Mutex<Option<EchoProbe>>>,
}

/// 워커와 가상 시간 하네스(`sim::tune`, ②b-3)가 **같은 것**을 쓴다 — 하네스의 5분 fixture가 이 단계의 호출 열을 지킨다.
pub(crate) struct PeerSources {
    /// 워커가 혼자 가지는 피어 미디어 표(지터 버퍼·수신 큐 소비자, 명령 큐로만 바뀐다, 엔진 ③-a2·a3). net 피어 표와 같은 순서.
    // Box 그대로 둔다: 명령 큐로 받은 할당을 제거 때 반환 큐로 그대로 돌려보내므로 워커가 다시 Box하지(할당하지) 않는다.
    #[allow(clippy::vec_box)]
    media: Vec<Box<PeerMedia>>,
    feeds: Vec<PeerFeed>,
    /// 피어 미디어 명령 큐의 워커 쪽 끝(추가·제거, 엔진 ③-a2).
    port: MediaPortRt,
    /// 지터 프로필(원자값, 반복마다 비교해 적용).
    settings: Arc<MediaSettings>,
    prods: PeerProds,
    /// 녹음 마커 큐(P2-4a): 녹음 중이면 피어별 은닉·버림을 출력 타임라인 위치로 넣는다.
    rec_marks: Arc<MarkSink>,
    probe: Arc<parking_lot::Mutex<Option<EchoProbe>>>,
    chunk_buf: Vec<f32>,
    tmp: Vec<f32>,
    slot_full_logged: bool,
}

impl PeerSources {
    pub(crate) fn new(parts: PeerSourcesParts) -> Self {
        Self {
            media: Vec::with_capacity(MEDIA_CMD_CAPACITY),
            feeds: Vec::with_capacity(MAX_SOURCES),
            port: parts.port,
            settings: parts.settings,
            prods: PeerProds(parts.prods),
            rec_marks: parts.rec_marks,
            probe: parts.probe,
            chunk_buf: vec![0.0; OUT_CHUNK * 2],
            tmp: vec![0.0; 8192],
            slot_full_logged: false,
        }
    }

    /// 피어 목록과 소스 슬롯을 맞춘다(새 피어에 빈 슬롯 배정, 사라진 피어 슬롯 해제).
    fn sync_slots(&mut self, bank: &SourceBank, out_rate: u32) {
        for f in &mut self.feeds {
            f.seen = false;
        }
        for m in &self.media {
            let key = m.shared().key;
            if let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) {
                f.seen = true;
                continue;
            }
            let used = |s: usize| self.feeds.iter().any(|f| f.slot == s);
            let free = (1..MAX_SOURCES).find(|&s| !used(s) && bank.slots[s].state.load(Ordering::Acquire) == SLOT_FREE);
            let Some(slot) = free else {
                if !self.slot_full_logged {
                    log::warn!("출력 소스 슬롯이 부족합니다(피어 {})", m.shared().peer_id);
                    self.slot_full_logged = true;
                }
                continue;
            };
            let sh = &bank.slots[slot];
            // 드문 할당(워커 스레드): 새 피어의 `PeerFeed`(리샘플러). engine-architecture.md 4.3절 1.
            match PeerFeed::new(slot, key, out_rate, sh) {
                Ok(f) => {
                    sh.playing.store(false, Ordering::Relaxed);
                    sh.occupant.store(m.shared().ssrc, Ordering::Relaxed); // 녹음 트랙 구분(P2-4), ACTIVE(Release) 전에
                    sh.state.store(SLOT_ACTIVE, Ordering::Release);
                    self.feeds.push(f);
                }
                Err(e) => log::error!("피어 출력 소스 생성 실패: {e}"),
            }
        }
        self.feeds.retain(|f| {
            if !f.seen {
                let sh = &bank.slots[f.slot];
                sh.playing.store(false, Ordering::Relaxed);
                sh.state.store(SLOT_DRAIN, Ordering::Release);
            }
            f.seen
        });
    }

    /// 반복마다, `FeedTiming`을 만든 뒤. `stats`에는 표 길이·마감 채움·건너뜀을 쓴다(+ 락 자리별 대기 기록).
    pub(crate) fn service(&mut self, bank: &SourceBank, counters: &AudioCounters, stats: &WorkerStats, out_rate: u32, t: &FeedTiming, lock_wait: &mut LockWait) -> PeerRound {
        // 반복 맨 앞: 추가·제거 명령을 끝까지 비운다(엔진 ③-a2). 프로필은 이번 반복에 한 번 읽는다.
        self.port.drain(&mut self.media);
        stats.media_peers.store(self.media.len() as u64, Ordering::Relaxed);
        let profile = self.settings.profile();
        self.sync_slots(bank, out_rate);
        let cb_seq = t.cb_seq;
        let consistent = || cb_seq.is_some_and(|s| counters.out_cb_begin.load(Ordering::SeqCst) == s);
        let (mut fills, mut skipped, mut urgent) = (0u64, 0u64, false);
        let mut guard = lock_wait.lock(&self.probe, &stats.lock_sites.probe_feed);
        let mut tap = guard.as_mut().filter(|p| p.tap);
        let marks = &self.rec_marks;
        let marking = marks.active();
        let played = &counters.out_played_frames;
        for pm in &mut self.media {
            // 그 피어의 `service` **바로 앞**에서 그 피어의 수신 큐를 끝까지 비워 삽입하고, 재시작 → 프로필 요청을 적용한다
            // (엔진 ③-a3, 설계 2.4절). 반복 맨 앞에서 전 피어를 한꺼번에 비우면 앞 피어의 service 동안 온 패킷이
            // 다음 반복으로 밀린다 — 그래서 피어마다 여기서 비운다.
            let (media, jb) = pm.ingest(profile);
            let key = media.key;
            // 통계 스냅샷은 슬롯이 없어도 발행한다(큐 비우기·삽입은 슬롯과 무관하게 계속된다, 엔진 ③-a1).
            let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) else {
                pm.publish();
                continue;
            };
            let sh = &bank.slots[f.slot];
            let g = media.mix.lr_gains();
            sh.set_gains(g, g);
            media.mix.level.store(sh.level.load());
            let Some(prod) = self.prods.get_mut(f.slot) else {
                pm.publish();
                continue;
            };
            let before = marking.then(|| jb.mark_counters());
            let ev = f.service(jb, prod, sh, t, &consistent, &mut self.chunk_buf, &mut self.tmp, tap.as_deref_mut().filter(|p| p.peer_id == media.peer_id && p.peer_ssrc == media.ssrc));
            if let Some((c0, d0)) = before {
                // 녹음 마커(P2-4a): 이번 반복에 은닉·버림이 늘었으면, 방금 링에 넣은 꼬리(= 다음 콜백 시작 + 선행량)의
                // 출력 타임라인 위치로 기록한다. 링 선행량 추정이라 대략 ±1 콜백으로 추정한다(위치 대조 측정은 아직 없음).
                let (c1, d1) = jb.mark_counters();
                let to_dev = |n48: u64| n48 * t.out_rate as u64 / INTERNAL_RATE as u64;
                let fs = jb.frame_samples() as u64;
                let tail = played.load(Ordering::Relaxed) + f.dev_ahead(prod) as u64;
                let mark = |kind, frames: u64, span: bool| {
                    let start = if span { tail.saturating_sub(frames) } else { tail };
                    marks.push(MarkEvent { ssrc: media.ssrc, kind, start, frames });
                };
                if c1 > c0 {
                    mark(MarkKind::Conceal, to_dev((c1 - c0) * fs), true);
                }
                // `skipped_frames`는 이미 장치 프레임이다
                let dropped = d1.saturating_sub(d0) * fs;
                if dropped > 0 || ev.skipped_frames > 0 {
                    mark(MarkKind::Drop, to_dev(dropped) + ev.skipped_frames, false);
                }
            }
            if ev.real_audio {
                pm.note_audio(t.wall_us);
            }
            pm.publish();
            fills += u64::from(ev.deadline_fill);
            skipped += ev.skipped_frames;
            urgent |= ev.urgent;
        }
        if fills > 0 {
            stats.deadline_fills.fetch_add(fills, Ordering::Relaxed);
        }
        if skipped > 0 {
            stats.skipped_frames.fetch_add(skipped, Ordering::Relaxed);
        }
        PeerRound { urgent }
    }

    /// 정지: 피어 슬롯을 닫는다(콜백이 아직 돌면 남은 소리를 버린다).
    pub(super) fn close(&mut self, bank: &SourceBank) {
        for f in self.feeds.drain(..) {
            let sh = &bank.slots[f.slot];
            sh.playing.store(false, Ordering::Relaxed);
            sh.state.store(SLOT_DRAIN, Ordering::Release);
        }
    }

    /// 피어 미디어 표(표 순서 = 추가 순). 하네스·테스트가 `service` 뒤에 피어별 지터 통계를 읽고, 끝에 남은 수신 큐를
    /// 비우는 데 쓴다. 제품 코드에는 이 입구가 없다.
    #[cfg(test)]
    #[allow(clippy::vec_box)]
    pub(crate) fn media_table(&mut self) -> &mut [Box<PeerMedia>] {
        &mut self.media
    }

    /// 표의 `i`번째 피어에 배정된 공급 상태의 적응 여유(장치 프레임). 슬롯이 없으면 None(하네스·테스트).
    #[cfg(test)]
    pub(crate) fn feed_margin(&self, i: usize) -> Option<usize> {
        let key = self.media.get(i)?.shared().key;
        self.feeds.iter().find(|f| f.key == key).map(|f| f.margin())
    }

    /// 표의 `i`번째 피어의 슬롯 번호(하네스·테스트).
    #[cfg(test)]
    pub(crate) fn feed_slot(&self, i: usize) -> Option<usize> {
        let key = self.media.get(i)?.shared().key;
        self.feeds.iter().find(|f| f.key == key).map(|f| f.slot)
    }
}
