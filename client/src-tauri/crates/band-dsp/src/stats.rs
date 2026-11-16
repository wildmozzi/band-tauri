//! 원자 카운터(RT·워커 공유). 프론트로 나가는 보고 DTO는 `engine::report`(설계 문서 4절).

use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};

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
/// 버전 비교가 한 벌임을 보장한다). 값의 칸 배치와 단일 작성자 보장은 감싸는 타입이 맡는다 — 사용자는 둘이다:
/// `peer_media::MediaStatsCell`(private `write`, 워커 단독 소유 `PeerMedia`만 부른다)과 메트로놈 상태 보고
/// (`metronome::MetronomeRt::publish(&mut self)`, `Clone`이 아닌 워커 쪽 끝). `write`는 크레이트 안에서만 보인다(#141 리뷰 M-1).
/// `write`·`try_read`는 할당·락·시스템콜이 없다. `read`는 재시도 중 `yield_now`를 부르므로 비RT 독자만 쓴다(워커는 쓰는
/// 쪽이다 — `tests/architecture.rs`의 워커 락 검사가 워커 디렉터리의 `.read(`를 막는다).
#[derive(Debug)]
pub struct SeqCell<const N: usize> {
    ver: AtomicU32,
    words: [AtomicU64; N],
}

impl<const N: usize> Default for SeqCell<N> {
    /// 전부 0, 버전 0(짝수 = 읽을 수 있음).
    fn default() -> Self {
        Self { ver: AtomicU32::new(0), words: std::array::from_fn(|_| AtomicU64::new(0)) }
    }
}

/// 읽기에서 바쁜 재시도를 이만큼 한 뒤에는 `yield_now`로 쓰는 쪽(워커)에 CPU를 넘긴다. 발행은 수십 ns이고 간격은
/// 워커 반복(≥150µs)이라 실제로는 1~2회에 끝난다.
const SPINS_BEFORE_YIELD: usize = 64;

impl<const N: usize> SeqCell<N> {
    /// 쓰기. 쓰는 스레드는 하나여야 한다(둘이면 버전이 깨져 독자가 섞인 한 벌을 받는다). 타입으로 막지 않으므로 규약이다:
    /// 감싸는 타입이 작성자 하나를 `&mut self`로 보장하는 곳에서만 부른다 — `MediaStatsCell`(`PeerMedia::publish`),
    /// `band`의 `MetronomeRt::publish`. 새 사용자도 작성 끝을 하나로 묶을 것. (⑩-1에서 `pub(crate)` → `pub`: `band`의
    /// `metronome::sched`가 크레이트 밖이 됐다.)
    pub fn write(&self, w: &[u64; N]) {
        let v = self.ver.load(Ordering::Relaxed);
        self.ver.store(v.wrapping_add(1), Ordering::Relaxed); // 홀수: 쓰는 중
        fence(Ordering::Release); // 아래 필드 store가 홀수 버전보다 먼저 보이지 않게
        for (a, &x) in self.words.iter().zip(w) {
            a.store(x, Ordering::Relaxed);
        }
        self.ver.store(v.wrapping_add(2), Ordering::Release); // 짝수: 한 벌 완성
    }

    /// 한 번 읽어 본다. 발행 중이었거나 그 사이 발행이 끼었으면 None.
    pub fn try_read(&self) -> Option<[u64; N]> {
        let v1 = self.ver.load(Ordering::Acquire);
        if v1 & 1 != 0 {
            return None;
        }
        let w: [u64; N] = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
        fence(Ordering::Acquire); // 필드 load가 아래 버전 load보다 뒤로 가지 않게
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
                std::hint::spin_loop();
            }
        }
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

    /// 발행 중(홀수 버전)이면 `try_read`는 None이고, 짝수로 돌아오면 다시 읽힌다.
    #[test]
    fn seq_cell_try_read_refuses_while_writing() {
        let c = SeqCell::<3>::default();
        c.ver.store(1, Ordering::Relaxed);
        assert_eq!(c.try_read(), None);
        c.ver.store(2, Ordering::Release);
        assert_eq!(c.try_read(), Some([0; 3]));
    }

    /// 쉬지 않는 쓰는 스레드 하나 + 읽는 스레드 넷(`read` 둘, `try_read` 둘): 돌려받은 값은 언제나 한 벌이다.
    /// 칸이 64개라 쓰기 한 번이 길어 찢길 틈이 넓다(`MediaStatsCell`의 27칸보다 거센 조건).
    #[test]
    fn seq_cell_never_torn_under_write_storm() {
        const N: usize = 64;
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
                    if n.is_multiple_of(256) {
                        std::thread::yield_now();
                    }
                }
                n
            })
        };
        // 쓰기가 실제로 겹치게: 읽기 시작 전에 쓰는 쪽이 돌고 있어야 한다(#141 리뷰 L-1).
        start.wait();
        while c.try_read().is_none_or(|w| w[0] < 64) {
            std::thread::yield_now();
        }
        let readers: Vec<_> = (0..4u32)
            .map(|k| {
                let c = c.clone();
                std::thread::spawn(move || {
                    let (mut full, mut ok) = (0u64, 0u64);
                    for _ in 0..20_000 {
                        if k.is_multiple_of(2) {
                            assert_whole(&c.read());
                            full += 1;
                        } else if let Some(w) = c.try_read() {
                            assert_whole(&w);
                            ok += 1;
                        }
                    }
                    (full, ok)
                })
            })
            .collect();
        let (full, ok) = readers.into_iter().map(|r| r.join().unwrap()).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        stop.store(true, Ordering::Relaxed);
        assert!(writer.join().unwrap() > 64, "쓰기가 돌지 않았다");
        assert_eq!(full, 40_000, "read()는 언제나 한 벌을 돌려준다");
        // `try_read` 가지도 실제로 값을 검사했다(#141 리뷰 L-1). 성공 **비율**은 스케줄러에 달려 하한만 본다: 4만 번 가운데
        // 한 번도 성공하지 못하려면 매번 발행과 겹쳐야 한다(쓰는 쪽은 256회마다 양보한다).
        assert!(ok > 0, "try_read가 한 번도 성공하지 못했다");
        assert_whole(&c.try_read().expect("쓰기가 없을 때 try_read는 성공한다"));
    }
}
