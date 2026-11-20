//! 원자 카운터(RT·워커 공유). 프론트로 나가는 보고 DTO는 `engine::report`(설계 문서 4절).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

// `SeqCell`이 쓰는 원자 타입·fence·spin 힌트. 보통 빌드는 std 그대로(코드 경로·기계어 불변)이고, `--cfg loom`
// 시험 빌드에서만 loom 모델 타입으로 바뀐다(#141 리뷰 L-2 — `tests::loom`이 메모리 순서를 모델 검사한다; loom은 개발 의존이라
// 시험 밖 빌드는 `--cfg loom`이어도 std다). 다른 원자값(`AtomicF32`·`AudioCounters`)은 바꾸지 않는다.
#[cfg(all(test, loom))]
use loom::{
    hint as seq_hint,
    sync::atomic::{fence as seq_fence, AtomicU32 as SeqU32, AtomicU64 as SeqU64},
};
#[cfg(not(all(test, loom)))]
use std::{
    hint as seq_hint,
    sync::atomic::{fence as seq_fence, AtomicU32 as SeqU32, AtomicU64 as SeqU64},
};

/// f32를 비트로 저장하는 원자값. RT 콜백에서 안전하게 쓸 수 있다.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    #[inline]
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// RT 콜백과 워커가 공유하는 오디오 카운터.
#[derive(Debug, Default)]
pub struct AudioCounters {
    pub input_level: AtomicF32,
    pub output_level: AtomicF32,
    /// 출력 언더런 + 입력 오버런 횟수.
    pub xruns: AtomicU64,
    /// 마지막 입력 콜백 프레임 수(장치 버퍼 크기 추정).
    pub in_cb_frames: AtomicU32,
    pub out_cb_frames: AtomicU32,
    /// 출력 콜백이 장치에 내보낸 누적 프레임 수(언더런으로 0을 채운 부분 포함 = 장치 재생 시간).
    /// 장치 클럭 추정에는 이것을 쓴다. 소비 샘플 수는 언더런 때 덜 늘어 재생 시간보다 뒤처진다(PR #5 H-1).
    pub out_played_frames: AtomicU64,
    /// 출력 콜백 시작/끝 횟수. 워커는 둘이 같고 그 사이 바뀌지 않았을 때만(콜백이 돌지 않는 동안 읽은)
    /// 소스 링 점유량으로 깊이 측정·마감 판단을 한다. 콜백이 링을 비운 직후 이전 마감 시각과 짝지으면
    /// 한 주기 이르게 은닉했다.
    pub out_cb_begin: AtomicU64,
    pub out_cb_end: AtomicU64,
    /// 입력 콜백이 받은 누적 프레임 수(장치 분리 감지용).
    pub in_captured_frames: AtomicU64,
    /// 스트림 에러 횟수(cpal 에러 콜백, 자체 WASAPI 호출 실패; 비치명 포함). 워커가 로그로 남긴다.
    pub stream_errors: AtomicU32,
    /// 백엔드가 장치 분실(분리·무효화·기본 장치 변경)을 직접 알렸다. 콜백 정지 감지(1초)보다 빠르다.
    pub device_lost: AtomicBool,
    /// 스트림이 복구되지 않는 오류로 멈췄다(치명 HRESULT, 재시작을 반복해도 실패). 장치는 남아 있을 수 있다.
    pub stream_failed: AtomicBool,
    /// 일시 오류가 반복되어 스트림을 재시작한 횟수(자체 WASAPI).
    pub stream_restarts: AtomicU32,
    /// 캡처 패킷의 DATA_DISCONTINUITY(장치가 데이터를 버림 = 입력 오버런, `xruns`에도 더함). 시작 직후 첫 패킷 제외.
    pub input_discontinuities: AtomicU64,
    /// 장치가 SILENT로 표시한 캡처 패킷 수.
    pub input_silent_packets: AtomicU64,
    /// 오디오 I/O 스레드의 우선순위 설정 결과(자체 WASAPI가 적는다; cpal·가짜 장치는 `Unmanaged`).
    pub audio_thread: crate::prio_slot::PrioSlot,
    /// 입·출력 콜백 실행시간 히스토그램(P1-10). 콜백은 원자 덧셈만 한다.
    pub rt_timing: crate::rt_timing::RtTiming,
}

/// seqlock 칸: 버전 + `[AtomicU64; N]`(②b-6a, `engine-stage2b-design.md` 3.2절·6.1절). 쓰는 쪽은 하나뿐이고 기다리지
/// 않는다. 읽는 쪽은 발행 중이면 다시 읽는다. 필드가 전부 원자값이라 `unsafe`가 없다(찢긴 읽기도 정의된 동작이고,
/// 버전 비교가 한 벌임을 보장한다). 값의 칸 배치는 감싸는 타입이 맡는다. 작성자가 하나임은 타입으로 보장한다:
/// - 크레이트 밖: 작성 끝 `SeqWriter`(`SeqCell::pair`가 하나만 만들고 `Clone`이 아니며 `write(&mut self)`)로만 쓴다 —
///   `band-engine`의 메트로놈 상태 보고(`metronome::MetronomeRt`). ⑩-1에서 `write`를 `pub`으로 열었던 것을 ⑩-2에서
///   이 타입으로 닫았다(#160 리뷰 L2).
/// - 크레이트 안: `peer_media::MediaStatsCell`(private `write`, 워커 단독 소유 `PeerMedia::publish(&mut self)`만 부른다)이
///   `SeqCell::write`(`pub(crate)`)를 직접 쓴다.
///
/// `write`·`try_read`는 할당·락·시스템콜이 없다. `read`는 재시도 중 `yield_now`를 부르므로 비RT 독자만 쓴다(워커는 쓰는
/// 쪽이다 — `tests/architecture.rs`의 워커 락 검사가 워커 디렉터리의 `.read(`를 막는다).
#[derive(Debug)]
pub struct SeqCell<const N: usize> {
    ver: SeqU32,
    words: [SeqU64; N],
}

impl<const N: usize> Default for SeqCell<N> {
    /// 전부 0, 버전 0(짝수 = 읽을 수 있음).
    fn default() -> Self {
        Self { ver: SeqU32::new(0), words: std::array::from_fn(|_| SeqU64::new(0)) }
    }
}

/// 읽기에서 바쁜 재시도를 이만큼 한 뒤에는 `yield_now`로 쓰는 쪽(워커)에 CPU를 넘긴다. 발행은 수십 ns이고 간격은
/// 워커 반복(≥150µs)이라 실제로는 1~2회에 끝난다.
const SPINS_BEFORE_YIELD: usize = 64;

impl<const N: usize> SeqCell<N> {
    /// (읽는 쪽 칸, 작성 끝 하나). 크레이트 밖에서 칸에 쓰는 유일한 길이다. 칸은 `Arc`로 나눠 갖는다(만들 때 한 번 할당 —
    /// 엔진 시작 같은 제어 평면에서 부른다).
    pub fn pair() -> (Arc<Self>, SeqWriter<N>) {
        let cell = Arc::new(Self::default());
        (cell.clone(), SeqWriter(cell))
    }

    /// 쓰기. 쓰는 스레드는 하나여야 한다(둘이면 버전이 깨져 독자가 섞인 한 벌을 받는다). 크레이트 밖은 `SeqWriter`로만
    /// 쓰고, 크레이트 안에서는 작성자 하나를 `&mut self`로 보장하는 `MediaStatsCell`(`PeerMedia::publish`)만 부른다.
    pub(crate) fn write(&self, w: &[u64; N]) {
        let v = self.ver.load(Ordering::Relaxed);
        self.ver.store(v.wrapping_add(1), Ordering::Relaxed); // 홀수: 쓰는 중
        seq_fence(Ordering::Release); // 아래 필드 store가 홀수 버전보다 먼저 보이지 않게
        for (a, &x) in self.words.iter().zip(w) {
            a.store(x, Ordering::Relaxed);
        }
        self.ver.store(v.wrapping_add(2), Ordering::Release); // 짝수: 한 벌 완성
    }

    /// 한 번 읽어 본다. 발행 중이었거나 그 사이 발행이 끼었으면 None.
    pub(crate) fn try_read(&self) -> Option<[u64; N]> {
        let v1 = self.ver.load(Ordering::Acquire);
        if v1 & 1 != 0 {
            return None;
        }
        let w: [u64; N] = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
        seq_fence(Ordering::Acquire); // 필드 load가 아래 버전 load보다 뒤로 가지 않게
        (self.ver.load(Ordering::Relaxed) == v1).then_some(w)
    }

    /// 일관된 한 벌. 성공할 때까지 다시 읽는다(찢긴 값은 절대 돌려주지 않는다). `SPINS_BEFORE_YIELD`회마다
    /// `yield_now` — 쓰는 쪽이 발행 도중 선점됐으면 그 스레드가 마치게 한다. 비RT 독자만 부른다(RT 경로는 `try_read`).
    pub fn read(&self) -> [u64; N] {
        let mut spins = 0;
        loop {
            if let Some(w) = self.try_read() {
                return w;
            }
            spins += 1;
            if spins % SPINS_BEFORE_YIELD == 0 {
                std::thread::yield_now();
            } else {
                seq_hint::spin_loop(); // loom 빌드에서는 모델 스케줄러에 양보한다
            }
        }
    }
}

/// `SeqCell`의 작성 끝(#160 리뷰 L2). `SeqCell::pair`만 만들고 `Clone`이 아니며 `write`가 `&mut self`다 — 한 칸에 작성자가
/// 하나임이 타입으로 보장된다. 쓰기는 `SeqCell::write`와 같다(할당·락·시스템콜 없음, 기다리지 않음).
#[derive(Debug)]
pub struct SeqWriter<const N: usize>(Arc<SeqCell<N>>);

impl<const N: usize> SeqWriter<N> {
    /// 한 벌을 낸다.
    pub fn write(&mut self, w: &[u64; N]) {
        self.0.write(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn atomic_f32_roundtrip() {
        let a = AtomicF32::new(0.25);
        assert_eq!(a.load(), 0.25);
        a.store(-1.5);
        assert_eq!(a.load(), -1.5);
    }

    /// n번째 한 벌: 칸 i = n·(i+1). 첫 칸이 n이다.
    fn frame<const N: usize>(n: u64) -> [u64; N] {
        std::array::from_fn(|i| n.wrapping_mul(i as u64 + 1))
    }

    /// 한 벌 = 모든 칸이 같은 n에서 나왔다. 쓰기 전 기본값(전부 0)도 n = 0인 한 벌이다.
    fn assert_whole<const N: usize>(w: &[u64; N]) {
        assert_eq!(*w, frame::<N>(w[0]), "찢긴 한 벌");
    }

    #[test]
    fn seq_cell_roundtrip() {
        let c = SeqCell::<5>::default();
        assert_eq!(c.read(), [0; 5]);
        assert_eq!(c.try_read(), Some([0; 5]));
        c.write(&frame(7));
        assert_eq!(c.read(), frame::<5>(7));
        c.write(&[u64::MAX; 5]);
        assert_eq!(c.try_read(), Some([u64::MAX; 5]));
    }

    /// 작성 끝(`SeqWriter`)으로 쓴 것을 칸 쪽에서 읽는다(#160 리뷰 L2).
    #[test]
    fn seq_writer_pair_roundtrip() {
        let (cell, mut w) = SeqCell::<4>::pair();
        assert_eq!(cell.read(), [0; 4]);
        w.write(&frame(3));
        assert_eq!(cell.read(), frame::<4>(3));
        assert_eq!(cell.try_read(), Some(frame::<4>(3)));
    }

    /// 발행 중(홀수 버전)이면 `try_read`는 None이고, 짝수로 돌아오면 다시 읽힌다.
    #[test]
    fn seq_cell_try_read_refuses_while_writing() {
        let c = SeqCell::<3>::default();
        c.ver.store(1, Ordering::Relaxed);
        assert_eq!(c.try_read(), None);
        c.ver.store(2, Ordering::Release);
        assert_eq!(c.try_read(), Some([0; 3]));
    }

    /// 쓰는 스레드 하나(폭주) + 읽는 스레드 넷(`read` 둘, `try_read` 둘): 돌려받은 값은 언제나 한 벌이다.
    /// 칸이 64개라 쓰기 한 번이 길어 찢길 틈이 넓다(`MediaStatsCell`보다 거센 조건).
    ///
    /// 쓰는 쪽은 `BURST`번을 쉬지 않고 쓴 뒤 읽기 `GAP_READS`번 길이만큼만 쉰다(그 틈은 자기 칸을 읽어서 만든다 — 빌드
    /// 프로필·CPU가 달라도 "읽기 몇 번이 들어갈 길이"가 유지된다). 틈이 없으면 `try_read`의 성공 여부가 스케줄러에만 달린다:
    /// 짝수 버전 창(쓰기 사이)이 64칸 읽기보다 짧아, 쓰는 스레드가 선점될 때만 읽힌다. 예전 시험은 시도 2만 번(약 1ms —
    /// 스케줄러 한 구간보다 짧다)으로 끝나서 그 사이 선점이 없으면 성공 0으로 실패했다(P2-28).
    /// `try_read` 가지는 횟수가 아니라 **조건**으로 끝난다: 성공 `MIN_OK`번(값을 실제로 검사했다)과 실패 `MIN_BUSY`번(쓰기와
    /// 실제로 겹쳤다 — 겹치지 않으면 찢길 기회도 없다)을 둘 다 채울 때까지 돈다. 시간 상한을 넘기면 단언이 실패한다.
    #[test]
    fn seq_cell_never_torn_under_write_storm() {
        use std::time::{Duration, Instant};
        const N: usize = 64;
        const BURST: u64 = 64;
        const GAP_READS: usize = 8;
        const MIN_TRIES: u64 = 20_000;
        const MIN_OK: u64 = 200;
        const MIN_BUSY: u64 = 200;
        // 정상이면 수십 ms에 끝난다. 읽기가 전혀 안 되는 회귀에서 매달리지 않고 실패하게 하는 상한이다.
        const LIMIT: Duration = Duration::from_secs(60);
        let c = Arc::new(SeqCell::<N>::default());
        let stop = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(2));
        let writer = {
            let (c, stop, start) = (c.clone(), stop.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                let mut n = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    c.write(&frame::<N>(n));
                    n += 1;
                    if n.is_multiple_of(BURST) {
                        for _ in 0..GAP_READS {
                            std::hint::black_box(c.try_read());
                        }
                    }
                }
                n
            })
        };
        // 쓰기가 실제로 겹치게: 읽기 시작 전에 쓰는 쪽이 돌고 있어야 한다(#141 리뷰 L-1).
        start.wait();
        let deadline = Instant::now() + LIMIT;
        while c.try_read().is_none_or(|w| w[0] < 64) {
            assert!(Instant::now() < deadline, "쓰는 쪽이 도는 동안 try_read가 한 번도 성공하지 못했다(시작 대기)");
            std::thread::yield_now();
        }
        let spawn_try = || {
            let c = c.clone();
            std::thread::spawn(move || {
                let (mut tries, mut ok, mut busy) = (0u64, 0u64, 0u64);
                while tries < MIN_TRIES || ok < MIN_OK || busy < MIN_BUSY {
                    // 시각은 가끔만 본다(시계 호출이 읽기 사이 간격을 바꾸지 않게).
                    if tries % 4096 == 4095 && Instant::now() >= deadline {
                        break;
                    }
                    tries += 1;
                    match c.try_read() {
                        Some(w) => {
                            assert_whole(&w);
                            ok += 1;
                        }
                        None => busy += 1,
                    }
                }
                (tries, ok, busy)
            })
        };
        let spawn_read = || {
            let c = c.clone();
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    assert_whole(&c.read());
                }
            })
        };
        // 스레드 순서는 예전과 같다(read, try_read, read, try_read).
        let (r0, t0, r1, t1) = (spawn_read(), spawn_try(), spawn_read(), spawn_try());
        // `try_read` 가지를 먼저 거둔다: 읽기가 전혀 안 되는 회귀에서는 `read()`가 끝나지 않으므로, 그것을 기다리기 전에
        // 진행성 단언이 실패해야 한다(#141 리뷰 L-1의 "값을 실제로 검사했다"도 이 하한이 맡는다).
        for (k, t) in [t0, t1].into_iter().enumerate() {
            let (tries, ok, busy) = t.join().unwrap();
            assert!(ok >= MIN_OK, "try_read가 {LIMIT:?} 안에 {MIN_OK}번 성공하지 못했다(가지 {k}: 시도 {tries}, 성공 {ok}, 발행 중 {busy})");
            assert!(busy >= MIN_BUSY, "읽기가 쓰기와 겹치지 않았다 — 폭주가 아니다(가지 {k}: 시도 {tries}, 성공 {ok}, 발행 중 {busy})");
        }
        // `read()`는 언제나 한 벌을 돌려준다(검사는 스레드 안에서 하고, 실패하면 join이 panic을 옮긴다).
        r0.join().unwrap();
        r1.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        assert!(writer.join().unwrap() > 64, "쓰기가 돌지 않았다");
        assert_whole(&c.try_read().expect("쓰기가 없을 때 try_read는 성공한다"));
    }

    /// 메모리 순서 모델 검사(#141 리뷰 L-2). loom이 쓰는 쪽·읽는 쪽 스레드의 모든 교차와, C++11 모델이 허용하는 오래된
    /// 값 읽기(Relaxed load가 앞선 store를 보는 경우)를 다 돌려 본다 — 위 폭풍 시험은 x86(TSO)에서만 돌아 약한 순서
    /// 결함을 볼 수 없다. 보통 빌드에서는 컴파일되지 않는다. 실행:
    /// `RUSTFLAGS="--cfg loom" cargo test -p band-dsp --release --lib loom`(필터 필수 — 다른 시험은 loom 모델 밖에서
    /// loom 원자값을 만들어 실패한다).
    ///
    /// `mutant_*`: 같은 알고리즘의 복사본에서 순서 하나를 약하게 하면 loom이 찢긴 한 벌을 잡는다(`should_panic`). 모델이 그
    /// 결함을 볼 수 있다는 증명이다 — 이것이 없으면 제품 코드 시험이 통과해도 "loom이 못 봐서"일 수 있다. 복사본은
    /// 손잡이를 다 켜면 제품 코드와 같다(`mutant_all_knobs_on_passes`).
    #[cfg(loom)]
    mod loom {
        use super::{assert_whole, frame};
        use crate::stats::{seq_fence, SeqCell};
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        /// 칸 수. 둘이면 "한 칸은 새 값, 한 칸은 옛 값"이 생길 수 있어 충분하고, 상태 공간이 작다.
        const N: usize = 2;

        /// 작성자 하나가 두 벌을 잇달아 내고, 읽는 쪽은 `try_read`를 두 번 한다. 돌려받은 값은 언제나 한 벌이다.
        #[test]
        fn loom_try_read_never_torn() {
            loom::model(|| {
                let (cell, mut w) = SeqCell::<N>::pair();
                let t = loom::thread::spawn(move || {
                    w.write(&frame(1));
                    w.write(&frame(2));
                });
                for _ in 0..2 {
                    if let Some(v) = cell.try_read() {
                        assert_whole(&v);
                    }
                }
                t.join().unwrap();
                // 작성자가 끝난 뒤(join = happens-before)에는 반드시 마지막 한 벌이 읽힌다.
                assert_eq!(cell.try_read(), Some(frame::<N>(2)));
            });
        }

        /// `read`(재시도 고리)도 언제나 한 벌을 돌려주고, 작성자가 발행 도중이어도 끝난다(loom의 `spin_loop`는 양보).
        #[test]
        fn loom_read_never_torn() {
            loom::model(|| {
                let (cell, mut w) = SeqCell::<N>::pair();
                let t = loom::thread::spawn(move || w.write(&frame(1)));
                let v = cell.read();
                assert_whole(&v);
                assert!(v[0] <= 1);
                t.join().unwrap();
                assert_eq!(cell.read(), frame::<N>(1));
            });
        }

        /// 변이 손잡이. 다 켜면 제품 코드(`SeqCell::write`·`try_read`)와 같다.
        #[derive(Clone, Copy)]
        struct Knobs {
            /// 쓰는 쪽: 홀수 버전 뒤 Release fence.
            writer_fence: bool,
            /// 쓰는 쪽: 짝수 버전 store가 Release.
            writer_release: bool,
            /// 읽는 쪽: 첫 버전 load가 Acquire.
            reader_acquire: bool,
            /// 읽는 쪽: 필드 load 뒤 Acquire fence.
            reader_fence: bool,
            /// 읽는 쪽: 버전 재확인.
            recheck: bool,
        }

        const ALL_ON: Knobs =
            Knobs { writer_fence: true, writer_release: true, reader_acquire: true, reader_fence: true, recheck: true };

        fn ord(strong: bool, o: Ordering) -> Ordering {
            if strong {
                o
            } else {
                Ordering::Relaxed
            }
        }

        fn write_m(c: &SeqCell<N>, w: &[u64; N], k: Knobs) {
            let v = c.ver.load(Ordering::Relaxed);
            c.ver.store(v.wrapping_add(1), Ordering::Relaxed);
            if k.writer_fence {
                seq_fence(Ordering::Release);
            }
            for (a, &x) in c.words.iter().zip(w) {
                a.store(x, Ordering::Relaxed);
            }
            c.ver.store(v.wrapping_add(2), ord(k.writer_release, Ordering::Release));
        }

        fn try_read_m(c: &SeqCell<N>, k: Knobs) -> Option<[u64; N]> {
            let v1 = c.ver.load(ord(k.reader_acquire, Ordering::Acquire));
            if v1 & 1 != 0 {
                return None;
            }
            let w: [u64; N] = std::array::from_fn(|i| c.words[i].load(Ordering::Relaxed));
            if k.reader_fence {
                seq_fence(Ordering::Acquire);
            }
            (!k.recheck || c.ver.load(Ordering::Relaxed) == v1).then_some(w)
        }

        /// `loom_try_read_never_torn`과 같은 시나리오를 손잡이로 돌린다.
        fn run_mutant(k: Knobs) {
            loom::model(move || {
                let c = Arc::new(SeqCell::<N>::default());
                let wc = c.clone();
                let t = loom::thread::spawn(move || {
                    write_m(&wc, &frame(1), k);
                    write_m(&wc, &frame(2), k);
                });
                for _ in 0..2 {
                    if let Some(v) = try_read_m(&c, k) {
                        assert_whole(&v);
                    }
                }
                t.join().unwrap();
            });
        }

        #[test]
        fn loom_mutant_all_knobs_on_passes() {
            run_mutant(ALL_ON);
        }

        #[test]
        #[should_panic(expected = "찢긴 한 벌")]
        fn loom_mutant_no_writer_fence_is_caught() {
            run_mutant(Knobs { writer_fence: false, ..ALL_ON });
        }

        #[test]
        #[should_panic(expected = "찢긴 한 벌")]
        fn loom_mutant_relaxed_even_version_is_caught() {
            run_mutant(Knobs { writer_release: false, ..ALL_ON });
        }

        #[test]
        #[should_panic(expected = "찢긴 한 벌")]
        fn loom_mutant_relaxed_first_version_load_is_caught() {
            run_mutant(Knobs { reader_acquire: false, ..ALL_ON });
        }

        #[test]
        #[should_panic(expected = "찢긴 한 벌")]
        fn loom_mutant_no_reader_fence_is_caught() {
            run_mutant(Knobs { reader_fence: false, ..ALL_ON });
        }

        #[test]
        #[should_panic(expected = "찢긴 한 벌")]
        fn loom_mutant_no_version_recheck_is_caught() {
            run_mutant(Knobs { recheck: false, ..ALL_ON });
        }
    }
}
