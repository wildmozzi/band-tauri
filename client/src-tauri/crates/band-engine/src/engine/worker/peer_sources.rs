//! 피어 소스 단계(엔진 ②b): 피어 미디어 표(명령 큐로만 바뀐다)·소스 슬롯 배정·피어별 공급.
//!
//! 한 반복에 하는 일(순서가 조건이다 — `docs/overhaul/engine-stage2b-design.md` 5.2절 J1~J7, M1):
//! 1. 미디어 명령 큐(추가·제거)를 끝까지 비워 표에 반영한다 → 프로필을 한 번 읽는다 → 슬롯을 맞춘다(슬롯 없는 피어에 가장
//!    낮은 빈 슬롯 배정 → 이번 반복에 제거된 피어의 슬롯을 `DRAIN`으로).
//! 2. 피어마다(표 순서): `ingest`(그 피어의 수신 큐 비우기 → 재시작 → 프로필) → 슬롯 게인·레벨 →
//!    `PeerFeed::service` → 녹음 마커 → `publish`. 슬롯이 없는 피어도 `ingest`·`publish`는 한다.
//!
//! 표는 하나다(엔진 ②b-4): 피어 하나 = 미디어(지터 버퍼·수신 큐 소비자) + 공급 상태(슬롯·리샘플러, 슬롯을 못 받았으면 없음).
//! 리샘플러는 미디어가 싣고 온 것을 슬롯 배정 때 공급 상태로 옮기고, 제거·정지 때 미디어에 되실어 반환 큐로 보낸다 — 이 단계는
//! 피어 추가·제거에서 할당도 해제도 하지 않는다(엔진 ⑧ 후속, 하네스 `tests/sim/tune.rs`의 churn 시험이 할당 계수로 지킨다).
//! 예외 하나: 반환 큐가 가득 차 있으면 `give_back`이 그 항목을 여기(워커)서 해제하고 경고를 남긴다 — 제어 평면이 규약대로
//! 비우는 한 닿지 않는다(`peer_media::MEDIA_RET_CAPACITY`).
//! 피어 하나의 공급 규칙은 `peer_feed.rs`(`PeerFeed`)다.
//! 바뀌는 이유: 피어 수명(추가·제거·슬롯), 공급 순서, 녹음 마커. 모르는 것: net, 코덱 설정, 로컬 경로.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ringbuf::HeapProd;

use super::{FeedTiming, PeerFeed, WorkerStats, OUT_CHUNK};
use band_dsp::config::INTERNAL_RATE;
use crate::engine::measure::echo::EchoProbe;
use band_dsp::peer_media::{MediaCmd, MediaPortRt, MediaSettings, PeerMedia, MEDIA_CMD_CAPACITY};
use crate::engine::record::{MarkEvent, MarkKind, MarkSink};
use crate::engine::rt::{SourceBank, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN, SLOT_FREE};
use band_dsp::stats::AudioCounters;

/// `service` 한 번의 결과.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerRound {
    /// 링이 모자란 피어 중 가장 이른 은닉 마감(워커 epoch µs). 조정자는 이것으로 다음 기다림을 고른다(`Nap::pick`, P2-17g).
    pub deadline_us: Option<u64>,
}

/// 피어 슬롯(1..`MAX_SOURCES`)의 소스 링 생산자. 로컬 것(슬롯 0)은 `LocalPath`가 가지므로 슬롯 번호로만 접근한다.
struct PeerProds(Vec<HeapProd<f32>>);

impl PeerProds {
    fn get_mut(&mut self, slot: usize) -> Option<&mut HeapProd<f32>> {
        self.0.get_mut(slot.checked_sub(1)?)
    }
}

/// `PeerSources::new` 입력(소유권이 넘어오는 끝과 공유 핸들).
pub struct PeerSourcesParts {
    /// 슬롯 1.. 의 생산자(슬롯 번호 순).
    pub prods: Vec<HeapProd<f32>>,
    pub port: MediaPortRt,
    pub settings: Arc<MediaSettings>,
    pub rec_marks: Arc<MarkSink>,
}

/// 워커와 가상 시간 하네스(`tests/sim/tune.rs`, ②b-3)가 **같은 것**을 쓴다 — 하네스의 5분 fixture가 이 단계의 호출 열을 지킨다.
/// 피어 하나의 소스: 미디어 + 공급 상태.
struct PeerSource {
    /// 지터 버퍼·수신 큐 소비자. Box 그대로 둔다: 명령 큐로 받은 할당을 제거 때 반환 큐로 그대로 돌려보내므로 워커가 다시
    /// Box하지(할당하지) 않는다.
    media: Box<PeerMedia>,
    /// 소스 슬롯과 리샘플러. 빈 슬롯이 없으면 None — 그래도 `ingest`·`publish`는 한다(J7). 다음 반복에 다시 배정을 시도한다.
    feed: Option<PeerFeed>,
}

pub struct PeerSources {
    /// 워커가 혼자 가지는 피어 표(명령 큐로만 바뀐다, 엔진 ③-a2·a3). net 피어 표와 같은 순서(추가 순, 제거는 그 자리를 뺀다 —
    /// 순서를 바꾸지 않는다, J4).
    table: Vec<PeerSource>,
    /// 이번 반복에 제거된 피어가 쥐고 있던 슬롯 번호. 슬롯 배정이 끝난 뒤 `DRAIN`으로 바꾼다(그 반복에는 슬롯을 쥐고 있다).
    /// 리샘플러는 여기 남지 않는다 — 제거할 때 미디어에 되실어 반환 큐로 보냈다.
    released: Vec<usize>,
    /// 피어 미디어 명령 큐의 워커 쪽 끝(추가·제거, 엔진 ③-a2).
    port: MediaPortRt,
    /// 지터 프로필(원자값, 반복마다 비교해 적용).
    settings: Arc<MediaSettings>,
    prods: PeerProds,
    /// 녹음 마커 큐(P2-4a): 녹음 중이면 피어별 은닉·버림을 출력 타임라인 위치로 넣는다.
    rec_marks: Arc<MarkSink>,
    chunk_buf: Vec<f32>,
    tmp: Vec<f32>,
    slot_full_logged: bool,
    rate_mismatch_logged: bool,
}

impl PeerSources {
    pub fn new(parts: PeerSourcesParts) -> Self {
        Self {
            table: Vec::with_capacity(MEDIA_CMD_CAPACITY),
            released: Vec::with_capacity(MAX_SOURCES),
            port: parts.port,
            settings: parts.settings,
            prods: PeerProds(parts.prods),
            rec_marks: parts.rec_marks,
            chunk_buf: vec![0.0; OUT_CHUNK * 2],
            tmp: vec![0.0; 8192],
            slot_full_logged: false,
            rate_mismatch_logged: false,
        }
    }

    /// 미디어 명령(추가·제거)을 끝까지 비워 표에 반영한다(엔진 ③-a2). 추가는 표 끝에, 제거는 그 자리를 뺀다(순서 유지).
    /// 제거한 미디어는 공급 리샘플러를 되실어 반환 큐로 보내고(해제는 제어 평면), 슬롯 번호는 `released`로 옮긴다 — 슬롯은
    /// `sync_slots`가 배정을 끝낸 뒤에 놓는다. 모르는 키의 제거는 무시한다.
    fn apply_commands(&mut self) {
        while let Some(cmd) = self.port.next_cmd() {
            match cmd {
                MediaCmd::Add(media) => self.table.push(PeerSource { media, feed: None }),
                MediaCmd::Remove(key) => {
                    if let Some(i) = self.table.iter().position(|p| p.media.shared().key == key) {
                        let PeerSource { mut media, feed } = self.table.remove(i);
                        if let Some(f) = feed {
                            self.released.push(f.slot);
                            media.put_feed_resampler(f.into_resampler());
                        }
                        let _ = self.port.give_back(media);
                    }
                }
            }
        }
    }

    /// 표와 소스 슬롯을 맞춘다: 슬롯이 없는 피어에(표 순서대로) 가장 낮은 빈 슬롯을 배정하고, 그 뒤에 이번 반복에 제거된
    /// 피어의 슬롯을 `DRAIN`으로 바꾼다. 제거된 피어의 슬롯은 이 반복에는 쓰이지 않는다(콜백이 비워 `FREE`가 된 뒤에 재사용).
    /// 리샘플러는 미디어가 싣고 온 것을 옮겨 쓴다(할당 없음). 그 리샘플러의 출력 레이트가 `out_rate`와 다르면(엔진이 자기
    /// `StreamInfo.out_rate`로 만들므로 생기지 않는다 — 방어) 슬롯을 주지 않는다: 워커에서 새로 만들지 않는다.
    fn sync_slots(&mut self, bank: &SourceBank, out_rate: u32) {
        for i in 0..self.table.len() {
            if self.table[i].feed.is_some() {
                continue;
            }
            let used = |s: usize| self.table.iter().filter_map(|p| p.feed.as_ref().map(|f| f.slot)).chain(self.released.iter().copied()).any(|x| x == s);
            let free = (1..MAX_SOURCES).find(|&s| !used(s) && bank.slots[s].state.load(Ordering::Acquire) == SLOT_FREE);
            let Some(slot) = free else {
                if !self.slot_full_logged {
                    log::warn!("출력 소스 슬롯이 부족합니다(피어 {})", self.table[i].media.shared().peer_id);
                    self.slot_full_logged = true;
                }
                continue;
            };
            let p = &mut self.table[i];
            // 미디어가 싣고 온 리샘플러를 옮긴다(할당 없음, engine-architecture.md 4.3절 1).
            let Some(rs) = p.media.take_feed_resampler(out_rate) else {
                if !self.rate_mismatch_logged {
                    log::error!("피어 {}의 공급 리샘플러가 출력 레이트 {out_rate}Hz와 맞지 않습니다(소리 없음 — 이후 같은 경고는 생략)", p.media.shared().peer_id);
                    self.rate_mismatch_logged = true;
                }
                continue;
            };
            let sh = &bank.slots[slot];
            let f = PeerFeed::new(slot, rs, sh);
            sh.playing.store(false, Ordering::Relaxed);
            sh.occupant.store(p.media.shared().ssrc, Ordering::Relaxed); // 녹음 트랙 구분(P2-4), ACTIVE(Release) 전에
            sh.state.store(SLOT_ACTIVE, Ordering::Release);
            p.feed = Some(f);
        }
        for slot in self.released.drain(..) {
            let sh = &bank.slots[slot];
            sh.playing.store(false, Ordering::Relaxed);
            sh.state.store(SLOT_DRAIN, Ordering::Release);
        }
    }

    /// 반복마다, `FeedTiming`을 만든 뒤. `stats`에는 표 길이·마감 채움·건너뜀을 쓴다. `tap` = 에코 측정 중 탭이 켜진 프로브
    /// (조정자가 빌려 준다): 그 피어의 소스 링에 넣는 샘플과 재생 시각을 모은다.
    pub fn service(&mut self, bank: &SourceBank, counters: &AudioCounters, stats: &WorkerStats, out_rate: u32, t: &FeedTiming, mut tap: Option<&mut EchoProbe>) -> PeerRound {
        // 반복 맨 앞: 추가·제거 명령을 끝까지 비운다(엔진 ③-a2). 프로필은 이번 반복에 한 번 읽는다.
        self.apply_commands();
        stats.media_peers.store(self.table.len() as u64, Ordering::Relaxed);
        let profile = self.settings.profile();
        self.sync_slots(bank, out_rate);
        let cb_seq = t.cb_seq;
        let consistent = || cb_seq.is_some_and(|s| counters.out_cb_begin.load(Ordering::SeqCst) == s);
        let (mut fills, mut skipped, mut deadline_us) = (0u64, 0u64, None::<u64>);
        let marks = &self.rec_marks;
        let marking = marks.active();
        let played = &counters.out_played_frames;
        for PeerSource { media: pm, feed } in &mut self.table {
            // 그 피어의 `service` **바로 앞**에서 그 피어의 수신 큐를 끝까지 비워 삽입하고, 재시작 → 프로필 요청을 적용한다
            // (엔진 ③-a3, 설계 2.4절). 반복 맨 앞에서 전 피어를 한꺼번에 비우면 앞 피어의 service 동안 온 패킷이
            // 다음 반복으로 밀린다 — 그래서 피어마다 여기서 비운다.
            // 리버퍼 무음 시간(진단): 직전 반복 끝의 지터 상태로 그 사이 시간을 센다 — 삽입 전에.
            pm.note_stall(t.wall_us);
            let (media, jb) = pm.ingest(profile);
            // 통계 스냅샷은 슬롯이 없어도 발행한다(큐 비우기·삽입은 슬롯과 무관하게 계속된다, 엔진 ③-a1).
            let Some(f) = feed.as_mut() else {
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
            if let Some(d) = ev.deadline_us {
                deadline_us = Some(deadline_us.map_or(d, |e: u64| e.min(d)));
            }
        }
        if fills > 0 {
            stats.deadline_fills.fetch_add(fills, Ordering::Relaxed);
        }
        if skipped > 0 {
            stats.skipped_frames.fetch_add(skipped, Ordering::Relaxed);
        }
        PeerRound { deadline_us }
    }

    /// 정지: 피어 슬롯을 닫고(콜백이 아직 돌면 남은 소리를 버린다), 표에 남은 피어를 리샘플러를 되실어 모두 반환 큐로
    /// 보낸다 — 해제는 워커가 끝난 뒤 제어 평면(`Engine::stop`의 `reclaim`)이 한다. 워커 스레드는 끝나면서도 피어 상태를
    /// 해제하지 않는다(엔진 ⑧ 후속. 전에는 표에 남은 지터 버퍼·리샘플러가 워커 스레드에서 해제됐다).
    pub fn close(&mut self, bank: &SourceBank) {
        let drain = |slot: usize| {
            let sh = &bank.slots[slot];
            sh.playing.store(false, Ordering::Relaxed);
            sh.state.store(SLOT_DRAIN, Ordering::Release);
        };
        self.released.drain(..).for_each(drain);
        for PeerSource { mut media, feed } in self.table.drain(..) {
            if let Some(f) = feed {
                drain(f.slot);
                media.put_feed_resampler(f.into_resampler());
            }
            let _ = self.port.give_back(media);
        }
    }

    /// 표 길이(하네스·테스트). 제품 코드에는 아래 입구들이 없다.
    #[cfg(any(test, feature = "test-support"))]
    pub fn table_len(&self) -> usize {
        self.table.len()
    }

    /// 표의 `i`번째 피어의 미디어(표 순서 = 추가 순). 하네스·테스트가 `service` 뒤에 피어별 지터 통계를 읽고, 끝에 남은
    /// 수신 큐를 비우는 데 쓴다.
    #[cfg(any(test, feature = "test-support"))]
    pub fn media_at(&mut self, i: usize) -> &mut PeerMedia {
        &mut self.table[i].media
    }

    /// 표의 `i`번째 피어에 배정된 공급 상태의 적응 여유(장치 프레임). 슬롯이 없으면 None(하네스·테스트).
    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_margin(&self, i: usize) -> Option<usize> {
        self.table.get(i)?.feed.as_ref().map(|f| f.margin())
    }

    /// 표의 `i`번째 피어의 슬롯 번호(하네스·테스트).
    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_slot(&self, i: usize) -> Option<usize> {
        self.table.get(i)?.feed.as_ref().map(|f| f.slot)
    }
}
