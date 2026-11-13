//! 피어 미디어 상태(엔진 ③, `docs/overhaul/engine-stage3-design.md` 1.2절).
//!
//! - `PeerMedia`(지터 버퍼 + 수신 큐 소비자): 워커가 `Box`로 넘겨받아 혼자 가진다. 락이 없다(③-a3).
//!   net 수신 스레드는 피어별 수신 큐(`net::media_link`)에 넣기만 하고, 워커가 피어마다 `service` 직전에 비워 삽입한다.
//! - `PeerMediaShared`(믹스 제어·통계 스냅샷·재시작 요청, 전부 원자값): 제어 평면(엔진 등록부)과 워커가 `Arc`로 나눈다.
//! - 통계와 손실 보고는 워커가 발행한 스냅샷(seqlock·원자값)으로만 읽는다(③-a1).
//! - 명령(③-a2): 프로필은 `MediaSettings.profile` 원자값, 재시작은 세대 카운터, 추가·제거는 SPSC 명령 큐
//!   (`MediaPort` → 워커의 `MediaPortRt`, 용량 16). 응답은 없고, 제거된 항목은 반환 큐로 제어 평면에 돌아와 해제된다.

use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::jitter::{JitterBuffer, JitterSettings, JitterStats};
use super::mixer::PeerMixCtl;
use super::stats::AtomicF32;
use crate::net::media_link::{MediaRx, PeerKey};

/// 피어 하나의 미디어 통계 한 벌(워커가 발행한 스냅샷).
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct MediaStats {
    /// 지터 버퍼가 재생 중인가.
    pub playing: bool,
    pub js: JitterStats,
    /// 수신 큐가 가득 차 버린 패킷 수(③-a3부터. 그 전에는 0).
    pub rx_dropped: u64,
    /// 이 피어의 실제(은닉이 아닌) 오디오가 처음 재생 링에 들어간 시각(엔진 epoch µs, P1-20). 0 = 아직.
    pub first_audio_us: u64,
}

impl MediaStats {
    /// add_peer(`added_us`) → 첫 실제 오디오 재생(ms, P1-20). 아직이면 None.
    pub fn first_audio_ms(&self, added_us: u64) -> Option<f32> {
        (self.first_audio_us != 0).then(|| self.first_audio_us.saturating_sub(added_us) as f32 / 1000.0)
    }
}

/// `MediaStats`의 원자값 칸 수: `JitterStats` 24필드 + playing + rx_dropped + first_audio_us. 필드가 늘면 `unpack`의 구조체 식이
/// 컴파일되지 않으므로 `pack`·`unpack`을 함께 고친다.
const STAT_WORDS: usize = 27;

fn pack(s: &MediaStats) -> [u64; STAT_WORDS] {
    let j = &s.js;
    let f = |v: f32| v.to_bits() as u64;
    [
        s.playing as u64,
        s.rx_dropped,
        f(j.buffer_ms),
        f(j.target_ms),
        f(j.jitter_ms),
        f(j.loss_pct),
        j.late,
        j.concealed,
        j.concealed_loss,
        j.produced,
        j.dropped_frames,
        j.samples_dropped,
        j.samples_inserted,
        j.rebuffers,
        f(j.conceal_pct),
        f(j.lead_ms),
        f(j.jitter_est_ms),
        f(j.corr_time_pct),
        f(j.max_corr_cents),
        j.late_dropped,
        j.burst_events,
        f(j.wall_buffer_ms),
        f(j.tail_ms),
        j.tail_adjusts,
        j.tail_resets,
        j.min_transit as u64,
        s.first_audio_us,
    ]
}

fn unpack(w: &[u64; STAT_WORDS]) -> MediaStats {
    let f = |v: u64| f32::from_bits(v as u32);
    MediaStats {
        playing: w[0] != 0,
        rx_dropped: w[1],
        js: JitterStats {
            buffer_ms: f(w[2]),
            target_ms: f(w[3]),
            jitter_ms: f(w[4]),
            loss_pct: f(w[5]),
            late: w[6],
            concealed: w[7],
            concealed_loss: w[8],
            produced: w[9],
            dropped_frames: w[10],
            samples_dropped: w[11],
            samples_inserted: w[12],
            rebuffers: w[13],
            conceal_pct: f(w[14]),
            lead_ms: f(w[15]),
            jitter_est_ms: f(w[16]),
            corr_time_pct: f(w[17]),
            max_corr_cents: f(w[18]),
            late_dropped: w[19],
            burst_events: w[20],
            wall_buffer_ms: f(w[21]),
            tail_ms: f(w[22]),
            tail_adjusts: w[23],
            tail_resets: w[24],
            min_transit: w[25] as i64,
        },
        first_audio_us: w[26],
    }
}

/// `MediaStats` seqlock(설계 3.2절). 쓰는 쪽은 워커 하나뿐이고 기다리지 않는다. 읽는 쪽은 발행 중이면 다시 읽는다.
/// 필드가 전부 원자값이라 `unsafe`가 없다(찢긴 읽기도 정의된 동작이고, 버전 비교가 한 벌임을 보장한다).
/// 누적 카운터는 짝으로 쓰이므로(`produced − concealed`) 한 벌 일관성이 필요하다.
pub struct MediaStatsCell {
    ver: AtomicU32,
    words: [AtomicU64; STAT_WORDS],
}

impl Default for MediaStatsCell {
    fn default() -> Self {
        let cell = Self { ver: AtomicU32::new(0), words: std::array::from_fn(|_| AtomicU64::new(0)) };
        cell.write(&MediaStats::default());
        cell
    }
}

/// 읽기에서 바쁜 재시도를 이만큼 한 뒤에는 `yield_now`로 쓰는 쪽(워커)에 CPU를 넘긴다. 발행은 수십 ns이고 간격은
/// 워커 반복(≥150µs)이라 실제로는 1~2회에 끝난다.
const SPINS_BEFORE_YIELD: usize = 64;

impl MediaStatsCell {
    /// 쓰기. 쓰는 스레드는 하나여야 한다(둘이면 버전이 깨진다). 지금(③-a1·a2)은 워커만 `PeerMediaShared::publish`로
    /// 부르고, 그 호출이 지터 락을 쥔 채라 락이 직렬화한다. ③-a3에서 락이 없어지면 워커 단독 소유 `PeerMedia`의
    /// `&mut self` 메서드에서만 부르게 해 타입으로 보장한다.
    pub(crate) fn write(&self, s: &MediaStats) {
        let w = pack(s);
        let v = self.ver.load(Ordering::Relaxed);
        self.ver.store(v.wrapping_add(1), Ordering::Relaxed); // 홀수: 쓰는 중
        fence(Ordering::Release); // 아래 필드 store가 홀수 버전보다 먼저 보이지 않게
        for (a, x) in self.words.iter().zip(w) {
            a.store(x, Ordering::Relaxed);
        }
        self.ver.store(v.wrapping_add(2), Ordering::Release); // 짝수: 한 벌 완성
    }

    /// 한 번 읽어 본다. 발행 중이었거나 그 사이 발행이 끼었으면 None.
    pub fn try_read(&self) -> Option<MediaStats> {
        let v1 = self.ver.load(Ordering::Acquire);
        if v1 & 1 != 0 {
            return None;
        }
        let w: [u64; STAT_WORDS] = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
        fence(Ordering::Acquire); // 필드 load가 아래 버전 load보다 뒤로 가지 않게
        (self.ver.load(Ordering::Relaxed) == v1).then(|| unpack(&w))
    }

    /// 일관된 한 벌. 성공할 때까지 다시 읽는다(찢긴 값은 절대 돌려주지 않는다). `SPINS_BEFORE_YIELD`회마다
    /// `yield_now` — 쓰는 쪽이 발행 도중 선점됐으면 그 스레드가 마치게 한다. 비RT 독자(커맨드·`band-diag`·
    /// 측정)만 부른다. RT 경로가 읽어야 한다면 `try_read`만 쓴다(이 셀은 지금 RT 경로에서 쓰지 않는다).
    pub fn read(&self) -> MediaStats {
        let mut spins = 0;
        loop {
            if let Some(s) = self.try_read() {
                return s;
            }
            spins += 1;
            if spins % SPINS_BEFORE_YIELD == 0 {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

/// 피어 하나의 미디어 상태 중 여러 스레드가 보는 쪽. 전부 원자값이라 누구도 락을 잡지 않는다(설계 1.2절).
pub struct PeerMediaShared {
    pub key: PeerKey,
    pub peer_id: String,
    /// 녹음 트랙·에코 프로브 대조용(불변).
    pub ssrc: u32,
    /// gain·muted·pan: 커맨드가 쓴다. level: 워커가 쓴다.
    pub mix: PeerMixCtl,
    /// 워커가 반복마다 발행하는 통계 스냅샷(쓰는 쪽은 `PeerMedia::publish` 하나).
    stats: MediaStatsCell,
    /// 재시작 요청 세대: 제어가 +1, 워커가 마지막으로 본 값과 비교한다(여러 요청은 한 번으로 합쳐져도 결과가 같다).
    restart_req: AtomicU32,
    /// `add_peer` 시각(엔진 epoch µs, 불변). `firstAudioMs`의 기준(P1-20).
    pub added_us: u64,
}

impl PeerMediaShared {
    /// `PeerMedia::new`만 부른다(짝 하나로만 생긴다 — seqlock 단일 작성자의 타입 보장, #118 리뷰 L-3).
    fn new(key: PeerKey, peer_id: String, ssrc: u32, added_us: u64) -> Self {
        Self { key, peer_id, ssrc, mix: PeerMixCtl::default(), stats: MediaStatsCell::default(), restart_req: AtomicU32::new(0), added_us }
    }

    /// 교체(같은 peerId 재추가) 때 옛 상태의 믹스 설정(gain·muted·pan)을 이어받는다.
    pub fn inherit_mix(&self, old: &PeerMediaShared) {
        self.mix.gain.store(old.mix.gain.load());
        self.mix.muted.store(old.mix.muted.load(Ordering::Relaxed), Ordering::Relaxed);
        self.mix.pan.store(old.mix.pan.load());
    }

    /// 워커가 마지막으로 발행한 통계(락 없음, 최대 워커 반복 1회 전의 값).
    pub fn stats(&self) -> MediaStats {
        self.stats.read()
    }

    /// 지터 버퍼를 처음부터 다시 채우라고 요청한다(에코 측정 시작·끝에서 스트림이 바뀔 때). 워커가 다음 반복에 적용한다.
    pub fn request_restart(&self) {
        self.restart_req.fetch_add(1, Ordering::Release);
    }
}

/// 워커가 혼자 가지는 피어 미디어 상태: 지터 버퍼(락 없음)와 수신 큐 소비자(설계 1.2절).
/// 제어 평면 스레드에서 만들어(지터 버퍼 할당) 명령 큐로 넘기고, 제거되면 반환 큐로 돌아와 제어 평면에서 해제된다.
pub struct PeerMedia {
    pub shared: Arc<PeerMediaShared>,
    rx: MediaRx,
    jitter: JitterBuffer,
    restart_seen: u32,
    /// 이 지터 버퍼에 마지막으로 적용한 프로필(비트).
    profile_applied: u32,
    /// 첫 실제 오디오를 재생 링에 넣은 시각(엔진 epoch µs, 0 = 아직). 이 항목의 수명 동안 한 번만 정한다(P1-20).
    first_audio_us: u64,
}

impl PeerMedia {
    /// 지터 버퍼(슬롯 1024개 ≈ 1.19MB + 디코더)를 여기서 할당한다. 제어 평면 스레드에서 부른다.
    /// 공유 쪽(`PeerMediaShared`)도 여기서 함께 만들어 짝으로 돌려준다: `PeerMediaShared`는 이 함수로만 생기므로 한 공유 상태에
    /// 작성자(`PeerMedia`, `Clone` 아님, `publish(&mut self)`)가 정확히 하나다 — seqlock 단일 작성자가 모듈 규약이 아니라
    /// 타입으로 보장된다(#116 L-1, #118 리뷰 L-3).
    /// `added_us`: `add_peer` 시각(엔진 epoch µs, `firstAudioMs` 기준, P1-20).
    pub fn new(key: PeerKey, peer_id: String, ssrc: u32, added_us: u64, settings: JitterSettings, rx: MediaRx) -> (Arc<PeerMediaShared>, Box<Self>) {
        let shared = Arc::new(PeerMediaShared::new(key, peer_id, ssrc, added_us));
        let restart_seen = shared.restart_req.load(Ordering::Acquire);
        let me = Box::new(Self { shared: shared.clone(), rx, jitter: JitterBuffer::new(settings), restart_seen, profile_applied: settings.profile.to_bits(), first_audio_us: 0 });
        (shared, me)
    }

    /// 워커 반복의 앞부분(그 피어의 `service` 바로 앞, 설계 2.4절·4절):
    /// ① 수신 큐를 **끝까지** 비워 도착 순서·도착 시각 그대로 `insert` → ② 재시작 요청 → ③ 프로필.
    /// 큐를 비운 뒤 재시작하는 이유: 요청 전에 도착한 옛 스트림 패킷이 새 버퍼의 지연 통계에 섞이지 않게.
    /// `profile`은 이번 반복에 읽은 `MediaSettings.profile`. 마지막 적용값과 같으면 아무것도 하지 않는다.
    pub(crate) fn ingest(&mut self, profile: f32) -> (&PeerMediaShared, &mut JitterBuffer) {
        let jb = &mut self.jitter;
        self.rx.drain(|p| jb.insert(&p.hdr, p.payload(), p.arrival_us));
        let req = self.shared.restart_req.load(Ordering::Acquire);
        if req != self.restart_seen {
            self.restart_seen = req;
            self.jitter.restart();
        }
        if profile.to_bits() != self.profile_applied {
            self.profile_applied = profile.to_bits();
            self.jitter.set_profile(profile);
        }
        (&self.shared, &mut self.jitter)
    }

    /// 이번 반복에 이 피어의 실제(은닉이 아닌) 오디오를 재생 링에 넣었다(워커, `service` 직후). 처음 한 번만 기록한다.
    /// 지터 재시작(에코 측정)·경로 전환은 다시 재지 않는다 — 같은 항목의 "입장 → 첫 소리"는 한 번뿐이다.
    pub(crate) fn note_audio(&mut self, now_us: u64) {
        if self.first_audio_us == 0 {
            self.first_audio_us = now_us.max(1);
        }
    }

    /// 워커 반복의 뒷부분(`service` 직후): 통계 스냅샷과 손실 보고를 발행한다. `&mut self`라 쓰는 쪽이 이 항목의
    /// 단독 소유자(워커) 하나임이 타입으로 보장된다 — seqlock의 단일 작성자 조건(#116 L-1).
    pub(crate) fn publish(&mut self) {
        let js = self.jitter.stats();
        let playing = self.jitter.is_playing();
        self.shared.stats.write(&MediaStats { playing, js, rx_dropped: self.rx.dropped(), first_audio_us: self.first_audio_us });
        self.rx.set_loss_report(playing.then_some((js.loss_pct, js.conceal_pct)));
    }

    /// 지터 버퍼 읽기(하네스·테스트).
    #[cfg(test)]
    pub(crate) fn jitter(&self) -> &JitterBuffer {
        &self.jitter
    }

    /// 수신 큐 넘침 누적(하네스·테스트).
    #[cfg(test)]
    pub(crate) fn rx_dropped(&self) -> u64 {
        self.rx.dropped()
    }
}

/// 워커 명령(소유권이 넘어가는 것만. 값은 원자값으로, 설계 4절).
pub enum MediaCmd {
    Add(Box<PeerMedia>),
    Remove(PeerKey),
}

/// 명령 큐 용량. 워커가 반복(≤ 약 1ms)마다 비우므로 차는 것은 워커가 멈췄을 때뿐이다.
pub const MEDIA_CMD_CAPACITY: usize = 16;

/// 제어 평면 쪽 끝: 명령 생산자(커맨드 스레드가 여럿이라 Mutex, 제어 평면끼리만 다툰다)와 반환 큐 소비자.
pub struct MediaPort {
    cmd: Mutex<HeapProd<MediaCmd>>,
    ret: Mutex<HeapCons<Box<PeerMedia>>>,
}

/// 워커 쪽 끝.
pub struct MediaPortRt {
    cmd: HeapCons<MediaCmd>,
    ret: HeapProd<Box<PeerMedia>>,
}

/// 명령·반환 링을 여기서 사전할당한다.
pub fn media_port() -> (MediaPort, MediaPortRt) {
    let (cp, cc) = HeapRb::<MediaCmd>::new(MEDIA_CMD_CAPACITY).split();
    let (rp, rc) = HeapRb::<Box<PeerMedia>>::new(MEDIA_CMD_CAPACITY).split();
    (MediaPort { cmd: Mutex::new(cp), ret: Mutex::new(rc) }, MediaPortRt { cmd: cc, ret: rp })
}

/// 명령 큐에 빈칸이 생기기를 기다리는 상한. 워커는 반복(≤ 약 1ms)마다 비우므로, 이만큼 차 있으면 워커가 멈춘 것이다.
pub const MEDIA_CMD_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

impl MediaPort {
    /// 명령 큐 생산자를 잡는다. 여러 명령을 한 번에 넣을 때는 빈칸(`vacant_len`)을 먼저 확인한다.
    pub fn lock_cmd(&self) -> MutexGuard<'_, HeapProd<MediaCmd>> {
        self.cmd.lock()
    }

    /// 빈칸이 `n`개 이상인 생산자. 커맨드가 몰려(한 워커 반복 안에 16개 넘게) 차 있으면 워커가 비울 때까지
    /// `MEDIA_CMD_WAIT`까지 기다린다(제어 평면 스레드라 잠깐 자도 된다). 그래도 차 있으면 워커가 멈춘 것 → 에러.
    pub fn reserve_cmd(&self, n: usize) -> Result<MutexGuard<'_, HeapProd<MediaCmd>>, String> {
        let t0 = std::time::Instant::now();
        let mut cmd = self.cmd.lock();
        while cmd.vacant_len() < n {
            if t0.elapsed() >= MEDIA_CMD_WAIT {
                return Err("오디오 워커가 응답하지 않습니다(미디어 명령 큐가 가득 참)".into());
            }
            MutexGuard::unlocked(&mut cmd, || std::thread::sleep(std::time::Duration::from_millis(1)));
        }
        Ok(cmd)
    }

    /// 워커가 돌려보낸 항목을 비운다(지터 버퍼·수신 큐 해제는 여기, 제어 평면 스레드에서).
    pub fn reclaim(&self) {
        let mut ret = self.ret.lock();
        while ret.try_pop().is_some() {}
    }
}

impl MediaPortRt {
    /// 워커 반복 맨 앞: 명령을 끝까지 비워 표에 반영한다. `table`은 용량을 미리 잡아 둔 `Vec`이다.
    /// 제거한 항목은 반환 큐로 보낸다(가득 차 있으면 그 자리에서 해제 — 드문 경로). 모르는 키의 제거는 무시한다.
    pub fn drain(&mut self, table: &mut Vec<Box<PeerMedia>>) {
        while let Some(cmd) = self.cmd.try_pop() {
            match cmd {
                MediaCmd::Add(m) => table.push(m),
                MediaCmd::Remove(key) => {
                    if let Some(i) = table.iter().position(|m| m.shared.key == key) {
                        let m = table.remove(i);
                        let _ = self.ret.try_push(m);
                    }
                }
            }
        }
    }
}

/// 전역 미디어 설정(엔진 소유). 새 피어의 지터 버퍼 설정이고, 프로필은 실행 중에 바뀐다.
pub struct MediaSettings {
    min_ms: f32,
    max_ms: f32,
    profile: AtomicF32,
}

impl MediaSettings {
    pub fn new(s: JitterSettings) -> Self {
        Self { min_ms: s.min_ms, max_ms: s.max_ms, profile: AtomicF32::new(s.profile) }
    }

    pub fn current(&self) -> JitterSettings {
        JitterSettings { min_ms: self.min_ms, max_ms: self.max_ms, profile: self.profile.load() }
    }

    pub fn set_profile(&self, profile: f32) {
        self.profile.store(profile);
    }

    /// 워커가 반복마다 읽는 지금 프로필.
    pub fn profile(&self) -> f32 {
        self.profile.load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::media_link::{media_link, MediaTx};
    use std::sync::atomic::AtomicBool;

    fn sample(n: u64) -> MediaStats {
        let js = JitterStats {
            buffer_ms: n as f32 * 0.5,
            target_ms: 7.25,
            jitter_ms: 1.5,
            loss_pct: 0.25,
            late: n + 1,
            concealed: n / 3,
            concealed_loss: n / 5,
            produced: n,
            dropped_frames: 2,
            samples_dropped: 3,
            samples_inserted: 4,
            rebuffers: 5,
            conceal_pct: 0.75,
            lead_ms: 2.5,
            jitter_est_ms: 3.5,
            corr_time_pct: 4.5,
            max_corr_cents: 5.5,
            late_dropped: 6,
            burst_events: 7,
            wall_buffer_ms: 8.5,
            tail_ms: 9.5,
            tail_adjusts: 10,
            tail_resets: 11,
            min_transit: -123_456,
        };
        MediaStats { playing: n % 2 == 1, js, rx_dropped: n * 7, first_audio_us: n * 11 }
    }

    #[test]
    fn pack_roundtrip_keeps_every_field() {
        for n in [0, 1, 99, u32::MAX as u64 + 5] {
            let s = sample(n);
            assert_eq!(unpack(&pack(&s)), s);
        }
    }

    #[test]
    fn cell_starts_as_default_and_returns_last_write() {
        let c = MediaStatsCell::default();
        assert_eq!(c.read(), MediaStats::default());
        c.write(&sample(41));
        assert_eq!(c.read(), sample(41));
    }

    /// 한 벌이면 기본값(첫 발행 전) 아니면 `sample(n)`이다.
    fn assert_whole(s: &MediaStats) {
        let n = s.js.produced;
        assert!(s.js.concealed <= s.js.produced);
        assert_eq!(*s, if n == 0 && s.js.late == 0 { MediaStats::default() } else { sample(n) }, "찢긴 스냅샷");
    }

    /// 쓰기 폭주(쉬지 않는 쓰는 스레드) 중에도 `read()`는 찢긴 값을 돌려주지 않는다(#116 M-1: 전에는 재시도 상한 뒤
    /// 검증 없는 값을 돌려줘 약 1e-4로 찢겼다). 읽는 스레드 넷 × 각 2만 회, 실제보다 수천 배 거센 조건이다.
    #[test]
    fn seqlock_read_never_returns_torn_under_write_storm() {
        let c = Arc::new(MediaStatsCell::default());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (c, stop) = (c.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    c.write(&sample(n));
                    n += 1;
                }
                n
            })
        };
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        assert_whole(&c.read());
                    }
                })
            })
            .collect();
        for r in readers {
            r.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(writer.join().unwrap() > 1, "쓰기가 돌지 않았다");
    }

    /// 쓰는 스레드 하나 + 읽는 스레드 넷: 성공한 읽기는 언제나 한 벌이다(`concealed ≤ produced`, 같은 n에서 나온 값).
    #[test]
    fn seqlock_reads_are_never_torn() {
        // 읽는 스레드가 모두 돈 뒤에 쓰기를 시작하고(Barrier), 쓰는 쪽이 가끔 양보해 느린 CI에서도 읽기가 끼어들 틈을 준다.
        // 검사 대상은 "성공한 읽기는 언제나 한 벌"이다 — 성공 횟수 자체는 스케줄러에 달려 있어 전체 합만 본다.
        let c = Arc::new(MediaStatsCell::default());
        let stop = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(5));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (c, stop, start) = (c.clone(), stop.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let mut ok = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(s) = c.try_read() {
                            assert_whole(&s);
                            ok += 1;
                        }
                    }
                    ok
                })
            })
            .collect();
        start.wait();
        for n in 1..200_000u64 {
            c.write(&sample(n));
            if n % 256 == 0 {
                std::thread::yield_now();
            }
        }
        stop.store(true, Ordering::Relaxed);
        let total: u64 = readers.into_iter().map(|r| r.join().unwrap()).sum();
        assert!(total > 0, "쓰는 동안 읽기가 한 번도 성공하지 못했다");
        // 쓰기가 멈춘 뒤에는 언제나 성공하고 마지막 값이어야 한다
        let last = c.try_read().expect("쓰기가 없을 때 try_read는 성공한다");
        assert_whole(&last);
    }

    fn media(key: u64, profile: f32) -> (Box<PeerMedia>, MediaTx) {
        let k = PeerKey::from_raw(key);
        let (tx, rx) = media_link(k);
        (PeerMedia::new(k, format!("p{key}"), key as u32, 0, JitterSettings { min_ms: 0.0, max_ms: 80.0, profile }, rx).1, tx)
    }

    /// 재시작 요청은 세대가 바뀐 반복에 한 번만 적용되고, 여러 요청은 한 번으로 합쳐진다.
    #[test]
    fn ingest_applies_restart_once_per_generation() {
        let (mut pm, _tx) = media(1, 0.5);
        assert_eq!(pm.restart_seen, 0);
        pm.shared.request_restart();
        pm.shared.request_restart();
        let _ = pm.ingest(0.5);
        assert_eq!(pm.restart_seen, 2);
        let _ = pm.ingest(0.5);
        assert_eq!(pm.restart_seen, 2);
    }

    /// 첫 소리(P1-20): 처음 한 번만 기록되고 지터 재시작(에코 측정)에도 다시 재지 않는다. 기준은 `added_us`.
    #[test]
    fn first_audio_recorded_once_relative_to_add() {
        let (_tx, rx) = media_link(PeerKey::from_raw(3));
        let (s, mut pm) = PeerMedia::new(PeerKey::from_raw(3), "p3".into(), 3, 1_000_000, JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 }, rx);
        pm.publish();
        assert_eq!(s.stats().first_audio_ms(s.added_us), None);
        pm.note_audio(1_250_500);
        pm.publish();
        assert_eq!(s.stats().first_audio_ms(s.added_us), Some(250.5));
        s.request_restart();
        let _ = pm.ingest(0.5);
        pm.note_audio(9_000_000);
        pm.publish();
        assert_eq!(s.stats().first_audio_ms(s.added_us), Some(250.5));
    }

    /// 프로필 ↔ 추가 경합(#115 리뷰 L4): 옛 프로필로 만든 피어도 워커의 다음 반복에서 지금 프로필로 맞춰진다.
    #[test]
    fn ingest_catches_up_profile_set_before_the_peer_reached_the_worker() {
        let (mut pm, _tx) = media(2, 0.2);
        let (_, jb) = pm.ingest(0.9);
        assert_eq!(jb.tuning(), crate::engine::jitter::JitterTuning::from_profile(0.9));
        let (_, jb) = pm.ingest(0.1);
        assert_eq!(jb.tuning(), crate::engine::jitter::JitterTuning::from_profile(0.1));
    }

    /// 추가 → 제거: 표에서 빠진 항목은 반환 큐로 가서 제어 평면에서 해제된다. 모르는 키의 제거는 무시한다.
    #[test]
    fn media_port_moves_ownership_and_returns_removed() {
        let (port, mut rt) = media_port();
        let mut table = Vec::with_capacity(MEDIA_CMD_CAPACITY);
        let js = JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 };
        let mk = |k: u64| {
            let key = PeerKey::from_raw(k);
            PeerMedia::new(key, format!("p{k}"), k as u32, 0, js, media_link(key).1)
        };
        let ((a, ma), (b, mb)) = (mk(10), mk(11));
        {
            let mut cmd = port.lock_cmd();
            assert!(cmd.try_push(MediaCmd::Add(ma)).is_ok());
            assert!(cmd.try_push(MediaCmd::Add(mb)).is_ok());
            assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(99))).is_ok());
            assert!(cmd.try_push(MediaCmd::Remove(a.key)).is_ok());
        }
        rt.drain(&mut table);
        assert_eq!(table.iter().map(|m| m.shared.key).collect::<Vec<_>>(), vec![b.key]);
        // 반환 큐에 있는 동안은 워커가 놓은 참조가 남아 있다(엔진 등록부 a, 반환 큐의 PeerMedia)
        assert_eq!(Arc::strong_count(&a), 2);
        port.reclaim();
        assert_eq!(Arc::strong_count(&a), 1, "해제는 제어 평면의 reclaim에서");
    }

    /// `ingest`는 큐를 끝까지 비워 지터 버퍼에 넣고, `publish`는 큐 넘침(`rx_dropped`)과 재생 여부를 발행한다.
    #[test]
    fn ingest_drains_queue_and_publish_reports_rx_dropped() {
        use crate::net::media_link::AUDIO_QUEUE_PACKETS;
        use crate::net::packet::{Header, PacketType};
        let (mut pm, mut tx) = media(5, 0.5);
        let fcode = crate::engine::config::frame_code_from_samples(240).unwrap();
        let h = |seq: u32| Header { ptype: PacketType::Audio, ssrc: 5, seq, timestamp: seq * 240, flags: 0, frame_code: fcode };
        let pcm = [0u8; 480];
        for i in 0..(AUDIO_QUEUE_PACKETS as u32 + 3) {
            tx.push(&h(i), &pcm, 1_000 + i as u64 * 5_000);
        }
        let (_, jb) = pm.ingest(0.5);
        // 128개가 지터 버퍼로 갔다(삽입된 패킷은 아직 pop 전이라 버퍼에 쌓여 있다)
        assert!(jb.stats().late == 0);
        pm.publish();
        let s = pm.shared.stats();
        assert_eq!(s.rx_dropped, 3);
        assert_eq!(s.playing, pm.jitter().is_playing());
        assert_eq!(tx.loss_cell().get().is_some(), s.playing);
        // 다시 비우면 남은 것이 없다
        let mut n = 0;
        pm.rx.drain(|_| n += 1);
        assert_eq!(n, 0);
    }

    /// 명령 큐는 용량에서 막힌다(엔진은 넣기 전에 빈칸을 확인한다).
    #[test]
    fn media_cmd_queue_is_bounded() {
        let (port, _rt) = media_port();
        let mut cmd = port.lock_cmd();
        for i in 0..MEDIA_CMD_CAPACITY {
            assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(i as u64 + 1))).is_ok());
        }
        assert_eq!(cmd.vacant_len(), 0);
        assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(100))).is_err());
    }
}
