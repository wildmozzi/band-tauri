//! 워커 경로 할당·해제 계수(테스트 바이너리 전용, 엔진 ⑧ 후속).
//!
//! 계수 할당자 `Counting`을 테스트 바이너리의 글로벌 할당자로 건다 — 이 크레이트의 단위 테스트 바이너리는 아래 `cfg(test)`
//! 정적 변수가, 하네스(`tests/sim`, ⑩-3)는 자기 `main.rs`가 건다(글로벌 할당자는 최종 바이너리가 정한다). 걸린 바이너리에서는
//! 지금 스레드가 `counted` 구간 안일 때의 할당·재할당·**해제**만 센다. 표시도 카운터도 스레드별이라(`tests/rt_alloc.rs`와 같은
//! 방식, P3-17) 병렬로 도는 다른 시험의 할당이 섞이지 않는다. 구간 밖에서는 스레드 로컬 `Cell` 하나를 읽고 `System`에 넘길 뿐이다.
//!
//! 쓰는 곳:
//! - 워커 루프(`Worker::run`, 시험 구성): 반복 하나 전체(수면 제외)를 구간으로 감싸 `WorkerStats::iter_allocs`에 더한다 —
//!   `engine/tests.rs`가 시작·워밍업 뒤 정상 구간과 사건(피어 추가·교체·제거, 코덱 전환, 메트로놈, 녹음, 에코 측정) 동안 0을
//!   단언한다. 그래서 정의가 계층 0(`sys`)에 있다(`testing::alloc`이 다시 내보낸다 — 워커가 테스트 표면(7)을 쓰면 위 계층 사용).
//! - 하네스 `tests/sim/tune.rs`의 `run_multi`가 워커 단계 호출(`PeerSources::service`·`close`)만 구간으로 감싼다 — 하네스
//!   자신의 할당(패킷 `Vec`, 통계 표본)과 제어 평면 몫(`PeerMedia::new`, `MediaPort::reclaim`)은 구간 밖이다. 워커 단위 시험
//!   (`worker/tests.rs`의 레이트 불일치 시험)도 같은 방식으로 쓴다.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// 계수 할당자. 모든 호출을 `System`에 넘기고, `counted` 구간 안이면 이 스레드의 계수를 하나 올린다.
pub struct Counting;

thread_local! {
    // const 초기화·Drop 없는 Cell이라 접근해도 할당하지 않는다(할당자 안에서 써도 재귀 없음, 스레드 종료 중에도 접근 가능).
    static MARKED: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    if MARKED.with(|f| f.get()) {
        EVENTS.with(|n| n.set(n.get() + 1));
    }
}

// SAFETY: 모든 호출을 `System`에 그대로 넘긴다. 더하는 것은 스레드 로컬 계수뿐이다.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        unsafe { System.dealloc(p, l) }
    }
}

#[cfg(test)]
#[global_allocator]
static GLOBAL: Counting = Counting;

/// `f`를 구간으로 표시하고 돌린다. 결과와, 그동안 **이 스레드**가 한 할당·재할당·해제 횟수를 돌려준다(구간 안에서 띄운 다른
/// 스레드의 것은 세지 않는다). 겹쳐 부르지 않는다(단언). `f`가 패닉해도 표시는 풀린다.
pub fn counted<R>(f: impl FnOnce() -> R) -> (R, u64) {
    struct Unmark;
    impl Drop for Unmark {
        fn drop(&mut self) {
            MARKED.with(|x| x.set(false));
        }
    }
    assert!(!MARKED.with(|x| x.get()), "counted 구간을 겹쳐 불렀다");
    let before = EVENTS.with(|n| n.get());
    MARKED.with(|x| x.set(true));
    let unmark = Unmark;
    let r = f();
    drop(unmark);
    (r, EVENTS.with(|n| n.get()) - before)
}

/// 자기 검증: 구간 안의 할당과 해제를 각각 잡고, 구간 밖 것은 세지 않는다.
#[test]
fn counter_sees_alloc_and_free_inside_the_region_only() {
    let (v, n) = counted(|| std::hint::black_box(vec![1u8; 64]));
    assert_eq!(n, 1, "할당 1회");
    let ((), n) = counted(|| drop(v));
    assert_eq!(n, 1, "해제 1회");
    let outside = std::hint::black_box(vec![2u8; 64]);
    let ((), n) = counted(|| {});
    assert_eq!(n, 0);
    drop(outside);
}
