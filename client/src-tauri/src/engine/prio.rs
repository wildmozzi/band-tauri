//! 비RT 오디오 스레드(워커, 네트워크 수신, 자체 WASAPI 스레드)의 스케줄링 보강 (리뷰 M11, L11, P1-2).
//!
//! - Windows: MMCSS "Pro Audio" 작업으로 등록하고 작업 안 우선순위를 `AVRT_PRIORITY_HIGH`로 올린다.
//!   등록이 실패하면 `THREAD_PRIORITY_HIGHEST`로 대신한다. 가드가 drop되면(스레드 종료 직전) 등록을 해제한다.
//! - Windows: 엔진이 도는 동안 시스템 타이머 해상도를 1ms로 둔다(`timeBeginPeriod(1)`/`timeEndPeriod(1)`).
//!   프로세스 안 참조 카운트로 감싸서 엔진을 여러 번 시작·정지하거나 둘 이상 띄워도 begin/end 짝이 맞는다.
//! - x86/x86_64: FTZ/DAZ를 켜서 PLC·페이드 꼬리의 비정규 수(denormal)로 인한 CPU 스파이크를 막는다.
//! - 그 밖의 플랫폼은 no-op(`ThreadPriority::Unsupported`, 타이머 비활성).
//!
//! 이 모듈의 함수는 스레드 시작/엔진 시작 시 한 번 부른다. RT 콜백 경로에서 부르지 않는다(로그·시스템콜).
//! 결과는 `PrioSlot`(원자값)에 남겨 `EngineStats.threadPriority`로 노출한다.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::Serialize;

/// 스레드 우선순위 설정 결과(`EngineStats.threadPriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadPriority {
    /// 우리 코드가 설정하지 않음(스레드 없음, 또는 백엔드/OS가 관리 — cpal·ASIO 콜백 스레드).
    Unmanaged,
    /// MMCSS "Pro Audio" 등록 성공.
    Mmcss,
    /// MMCSS 등록 실패 → `THREAD_PRIORITY_HIGHEST`로 대신함.
    Elevated,
    /// MMCSS와 대체 우선순위 모두 실패(기본 우선순위로 동작).
    Failed,
    /// 측정용으로 꺼 둠(`set_engine_promotion(false)`).
    Disabled,
    /// Windows가 아님(no-op).
    Unsupported,
}

impl ThreadPriority {
    const ALL: [ThreadPriority; 6] = [
        ThreadPriority::Unmanaged,
        ThreadPriority::Mmcss,
        ThreadPriority::Elevated,
        ThreadPriority::Failed,
        ThreadPriority::Disabled,
        ThreadPriority::Unsupported,
    ];
    fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0) as u8
    }
    fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or(ThreadPriority::Unmanaged)
    }
}

/// 스레드가 자기 우선순위 결과를 적어 두는 원자 슬롯. 기본값은 `Unmanaged`.
#[derive(Debug, Default)]
pub struct PrioSlot(AtomicU8);

impl PrioSlot {
    pub fn load(&self) -> ThreadPriority {
        ThreadPriority::from_u8(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, p: ThreadPriority) {
        self.0.store(p.to_u8(), Ordering::Relaxed)
    }
}

/// 프론트로 나가는 스레드 우선순위 요약(`EngineStats.threadPriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPriorityStats {
    pub worker: ThreadPriority,
    /// 네트워크 수신 스레드(네트워크 없이 시작했으면 `unmanaged`).
    pub net_recv: ThreadPriority,
    /// 오디오 I/O 스레드. 자체 WASAPI는 우리가 등록하고, cpal·ASIO·가짜 장치는 `unmanaged`(백엔드가 관리).
    pub audio: ThreadPriority,
    /// 엔진이 시스템 타이머 해상도 1ms를 쥐고 있다.
    pub timer_1ms: bool,
}

impl ThreadPriorityStats {
    pub fn stopped() -> Self {
        Self {
            worker: ThreadPriority::Unmanaged,
            net_recv: ThreadPriority::Unmanaged,
            audio: ThreadPriority::Unmanaged,
            timer_1ms: false,
        }
    }
}

// ============================================================================
// OS 호출 표(테스트에서 실패 경로를 주입한다)
// ============================================================================

/// MMCSS/우선순위 OS 호출. 핸들 0은 실패.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct ThreadApi {
    pub register: fn() -> usize,
    pub set_high: fn(usize) -> bool,
    pub revert: fn(usize),
    pub raise: fn() -> bool,
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, GetCurrentThread,
        SetThreadPriority, AVRT_PRIORITY_HIGH, THREAD_PRIORITY_HIGHEST,
    };

    pub fn register() -> usize {
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        // SAFETY: name은 NUL 종료 UTF-16 문자열이고 index는 유효한 출력 포인터다.
        let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        h as usize
    }
    pub fn set_high(h: usize) -> bool {
        // SAFETY: h는 register가 돌려준 유효한 MMCSS 핸들이다.
        unsafe { AvSetMmThreadPriority(h as _, AVRT_PRIORITY_HIGH) != 0 }
    }
    pub fn revert(h: usize) {
        // SAFETY: h는 register가 돌려준 유효한 핸들이고 한 번만 해제한다.
        unsafe {
            AvRevertMmThreadCharacteristics(h as _);
        }
    }
    pub fn raise() -> bool {
        // SAFETY: GetCurrentThread는 의사 핸들을 돌려주며 항상 유효하다.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) != 0 }
    }
    pub fn time_begin(ms: u32) -> bool {
        // SAFETY: 인자 검사만 하는 단순 호출. TIMERR_NOERROR(0)이면 성공.
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(ms) == 0 }
    }
    pub fn time_end(ms: u32) {
        // SAFETY: 성공한 timeBeginPeriod(ms)와 짝으로만 부른다.
        unsafe {
            windows_sys::Win32::Media::timeEndPeriod(ms);
        }
    }
}

#[cfg(windows)]
static OS_THREAD: ThreadApi = ThreadApi { register: os::register, set_high: os::set_high, revert: os::revert, raise: os::raise };
#[cfg(windows)]
static OS_TIMER: TimerApi = TimerApi { begin: os::time_begin, end: os::time_end };

// ============================================================================
// 스레드 등록
// ============================================================================

/// 엔진 스레드(워커, net 수신) 승격과 타이머 해상도를 켤지. 측정(적용 전/후 비교)용 스위치이며 기본은 켬.
static ENGINE_PROMOTION: AtomicBool = AtomicBool::new(true);

/// 측정용: 엔진 스레드의 MMCSS 등록과 1ms 타이머를 끈다/켠다. 다음에 시작하는 엔진부터 적용된다.
/// 장치 스레드(자체 WASAPI, 가짜 장치)는 실제 장치 콜백 스레드를 대신하므로 이 스위치와 무관하게 승격한다.
#[doc(hidden)]
pub fn set_engine_promotion(enabled: bool) {
    set_engine_promotion_parts(enabled, enabled);
}

/// 1ms 타이머를 켤지(측정용, 기본 켬). MMCSS와 따로 끌 수 있어야 각각의 기여를 분리해 잴 수 있다(리뷰 L3).
static ENGINE_TIMER: AtomicBool = AtomicBool::new(true);

/// 측정용: 엔진 스레드 MMCSS 등록과 1ms 타이머를 따로 켜고 끈다. 다음에 시작하는 엔진부터 적용된다.
#[doc(hidden)]
pub fn set_engine_promotion_parts(mmcss: bool, timer_1ms: bool) {
    ENGINE_PROMOTION.store(mmcss, Ordering::SeqCst);
    ENGINE_TIMER.store(timer_1ms, Ordering::SeqCst);
}

pub fn engine_promotion_enabled() -> bool {
    ENGINE_PROMOTION.load(Ordering::SeqCst)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn engine_timer_enabled() -> bool {
    ENGINE_TIMER.load(Ordering::SeqCst)
}

pub struct AudioThreadGuard {
    status: ThreadPriority,
    /// MMCSS 핸들(0이면 없음)과 해제 함수.
    mmcss: usize,
    revert: fn(usize),
    /// MMCSS 등록은 등록한 스레드에서 풀어야 하므로 가드를 다른 스레드로 옮기지 못하게 `!Send`로 둔다(리뷰 L1).
    _not_send: PhantomData<*const ()>,
}

impl AudioThreadGuard {
    fn new(status: ThreadPriority, mmcss: usize, revert: fn(usize)) -> Self {
        Self { status, mmcss, revert, _not_send: PhantomData }
    }

    pub fn status(&self) -> ThreadPriority {
        self.status
    }
}

/// 현재 스레드를 오디오 처리용으로 설정한다(FTZ/DAZ + Windows MMCSS). 결과를 로그에 남긴다.
pub fn promote_current_thread() -> AudioThreadGuard {
    enable_flush_denormals();
    #[cfg(windows)]
    let g = promote_with(&OS_THREAD);
    #[cfg(not(windows))]
    let g = AudioThreadGuard::new(ThreadPriority::Unsupported, 0, |_| {});
    let name = std::thread::current().name().unwrap_or("?").to_string();
    match g.status {
        ThreadPriority::Mmcss => log::info!("스레드 {name}: MMCSS \"Pro Audio\" 등록"),
        ThreadPriority::Elevated => log::warn!("스레드 {name}: MMCSS 등록 실패 → THREAD_PRIORITY_HIGHEST"),
        ThreadPriority::Failed => log::warn!("스레드 {name}: MMCSS·우선순위 상향 모두 실패(기본 우선순위)"),
        _ => {}
    }
    g
}

/// 엔진 스레드(워커, net 수신)용: 측정 스위치가 꺼져 있으면 FTZ/DAZ만 켜고 `Disabled`. 결과를 `slot`에 적는다.
pub fn promote_engine_thread(slot: &PrioSlot) -> AudioThreadGuard {
    let g = if engine_promotion_enabled() {
        promote_current_thread()
    } else {
        enable_flush_denormals();
        AudioThreadGuard::new(ThreadPriority::Disabled, 0, |_| {})
    };
    slot.store(g.status);
    g
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn promote_with(api: &ThreadApi) -> AudioThreadGuard {
    let h = (api.register)();
    if h != 0 {
        // 작업 안 상대 우선순위. 실패해도 등록 자체는 유효하다(기본 NORMAL).
        let _ = (api.set_high)(h);
        AudioThreadGuard::new(ThreadPriority::Mmcss, h, api.revert)
    } else {
        let status = if (api.raise)() { ThreadPriority::Elevated } else { ThreadPriority::Failed };
        AudioThreadGuard::new(status, 0, api.revert)
    }
}

impl Drop for AudioThreadGuard {
    fn drop(&mut self) {
        if self.mmcss != 0 {
            (self.revert)(self.mmcss);
            self.mmcss = 0;
        }
    }
}

// ============================================================================
// 타이머 해상도
// ============================================================================

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct TimerApi {
    pub begin: fn(u32) -> bool,
    pub end: fn(u32),
}

/// 프로세스 안 1ms 타이머 참조 카운트. 0→1에서 begin, 1→0에서 end를 부른다.
pub(crate) struct TimerRefs(std::sync::Mutex<u32>);

impl TimerRefs {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) const fn new() -> Self {
        Self(std::sync::Mutex::new(0))
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, u32> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(windows)]
static TIMER_REFS: TimerRefs = TimerRefs::new();

const TIMER_MS: u32 = 1;

/// 엔진 수명 동안 쥐는 1ms 타이머 해상도. drop하면 놓는다(한 번만).
pub struct TimerResolution {
    held: Option<(&'static TimerRefs, &'static TimerApi)>,
}

impl TimerResolution {
    /// 타이머 측정 스위치가 켜져 있고 Windows면 1ms 해상도를 요청한다. 실패해도 엔진은 돈다(`active()`가 false).
    pub fn acquire() -> Self {
        #[cfg(windows)]
        if engine_timer_enabled() {
            let t = Self::acquire_with(&TIMER_REFS, &OS_TIMER);
            if !t.active() {
                log::warn!("timeBeginPeriod(1) 실패: 기본 타이머 해상도로 동작");
            }
            return t;
        }
        Self { held: None }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn acquire_with(refs: &'static TimerRefs, api: &'static TimerApi) -> Self {
        let mut n = refs.lock();
        if *n == 0 && !(api.begin)(TIMER_MS) {
            return Self { held: None };
        }
        *n += 1;
        Self { held: Some((refs, api)) }
    }

    pub fn active(&self) -> bool {
        self.held.is_some()
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if let Some((refs, api)) = self.held.take() {
            let mut n = refs.lock();
            *n = n.saturating_sub(1);
            if *n == 0 {
                (api.end)(TIMER_MS);
            }
        }
    }
}

/// MXCSR의 FTZ(bit 15)와 DAZ(bit 6)를 켠다(현재 스레드에만 적용).
#[allow(deprecated)]
pub fn enable_flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: MXCSR 읽기/쓰기는 x86_64에서 항상 사용 가능하며 FTZ/DAZ 비트만 바꾼다.
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn promote_and_flush() {
        let g = promote_current_thread();
        #[cfg(windows)]
        assert!(
            matches!(g.status(), ThreadPriority::Mmcss | ThreadPriority::Elevated | ThreadPriority::Failed),
            "Windows에서는 등록을 시도해야 함: {:?}",
            g.status()
        );
        #[cfg(not(windows))]
        assert_eq!(g.status(), ThreadPriority::Unsupported);
        #[cfg(target_arch = "x86_64")]
        {
            // 비정규 수 연산 결과가 0으로 flush되는지 확인
            let tiny = std::hint::black_box(f32::MIN_POSITIVE);
            let r = std::hint::black_box(tiny * 0.5);
            assert_eq!(r, 0.0, "FTZ가 켜져 있어야 함");
        }
        drop(g);
    }

    #[test]
    fn slot_roundtrip() {
        let s = PrioSlot::default();
        assert_eq!(s.load(), ThreadPriority::Unmanaged);
        for p in ThreadPriority::ALL {
            s.store(p);
            assert_eq!(s.load(), p);
        }
        assert_eq!(serde_json::to_string(&ThreadPriority::Mmcss).unwrap(), "\"mmcss\"");
        let st = serde_json::to_value(ThreadPriorityStats::stopped()).unwrap();
        assert_eq!(st["netRecv"], "unmanaged");
        assert_eq!(st["timer1ms"], false);
    }

    // 가짜 OS 호출: 테스트는 병렬로 돌므로 호출 기록은 스레드 로컬에 둔다.
    thread_local! {
        static REG: Cell<u32> = const { Cell::new(0) };
        static HIGH: Cell<u32> = const { Cell::new(0) };
        static REVERT: Cell<u32> = const { Cell::new(0) };
        static RAISE: Cell<u32> = const { Cell::new(0) };
    }
    fn bump(c: &'static std::thread::LocalKey<Cell<u32>>) {
        c.with(|c| c.set(c.get() + 1));
    }
    fn get(c: &'static std::thread::LocalKey<Cell<u32>>) -> u32 {
        c.with(|c| c.get())
    }

    #[test]
    fn mmcss_success_sets_priority_and_reverts_once() {
        static API: ThreadApi = ThreadApi {
            register: || {
                bump(&REG);
                0x1234
            },
            set_high: |h| {
                assert_eq!(h, 0x1234);
                bump(&HIGH);
                true
            },
            revert: |h| {
                assert_eq!(h, 0x1234);
                bump(&REVERT);
            },
            raise: || {
                bump(&RAISE);
                true
            },
        };
        let g = promote_with(&API);
        assert_eq!(g.status(), ThreadPriority::Mmcss);
        assert_eq!((get(&REG), get(&HIGH), get(&RAISE), get(&REVERT)), (1, 1, 0, 0));
        drop(g);
        assert_eq!(get(&REVERT), 1, "drop에서 정확히 한 번 해제");
    }

    #[test]
    fn mmcss_priority_failure_keeps_registration() {
        static API: ThreadApi = ThreadApi {
            register: || 7,
            set_high: |_| false,
            revert: |_| bump(&REVERT),
            raise: || panic!("등록에 성공했으면 대체 경로를 타지 않는다"),
        };
        let g = promote_with(&API);
        assert_eq!(g.status(), ThreadPriority::Mmcss);
        drop(g);
        assert_eq!(get(&REVERT), 1);
    }

    #[test]
    fn mmcss_failure_falls_back_without_revert() {
        static API: ThreadApi = ThreadApi {
            register: || 0,
            set_high: |_| panic!("핸들 없이 우선순위를 설정하면 안 됨"),
            revert: |_| bump(&REVERT),
            raise: || {
                bump(&RAISE);
                true
            },
        };
        let g = promote_with(&API);
        assert_eq!(g.status(), ThreadPriority::Elevated);
        drop(g);
        assert_eq!((get(&RAISE), get(&REVERT)), (1, 0), "등록 실패 시 해제를 부르지 않음");
    }

    #[test]
    fn mmcss_and_fallback_failure_reports_failed() {
        static API: ThreadApi = ThreadApi { register: || 0, set_high: |_| false, revert: |_| bump(&REVERT), raise: || false };
        let g = promote_with(&API);
        assert_eq!(g.status(), ThreadPriority::Failed);
        drop(g);
        assert_eq!(get(&REVERT), 0);
    }

    #[test]
    fn engine_promotion_switch_disables_and_records() {
        // 전역 스위치를 건드리므로 이 테스트 안에서 원래대로 돌려놓는다. 다른 단위 테스트는 스위치를 읽지 않는다.
        let slot = PrioSlot::default();
        set_engine_promotion(false);
        let g = promote_engine_thread(&slot);
        set_engine_promotion(true);
        assert_eq!(g.status(), ThreadPriority::Disabled);
        assert_eq!(slot.load(), ThreadPriority::Disabled);
        drop(g);
        let g = promote_engine_thread(&slot);
        assert_eq!(slot.load(), g.status());
        assert_ne!(g.status(), ThreadPriority::Disabled);
    }

    // 타이머: 테스트마다 고유한 정적 카운터/호출 기록을 쓴다(병렬 실행 격리).
    macro_rules! timer_fixture {
        ($refs:ident, $begins:ident, $ends:ident, $api:ident, $ok:expr) => {
            static $refs: TimerRefs = TimerRefs::new();
            static $begins: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            static $ends: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            static $api: TimerApi = TimerApi {
                begin: |ms| {
                    assert_eq!(ms, 1);
                    $begins.fetch_add(1, Ordering::SeqCst);
                    $ok
                },
                end: |ms| {
                    assert_eq!(ms, 1);
                    $ends.fetch_add(1, Ordering::SeqCst);
                },
            };
        };
    }

    #[test]
    fn timer_begin_end_pairs_across_repeated_and_overlapping_engines() {
        timer_fixture!(REFS, BEGINS, ENDS, API, true);
        let counts = || (BEGINS.load(Ordering::SeqCst), ENDS.load(Ordering::SeqCst));
        // 시작·정지 반복
        for i in 1..=3 {
            let t = TimerResolution::acquire_with(&REFS, &API);
            assert!(t.active());
            assert_eq!(counts(), (i, i - 1));
            drop(t);
            assert_eq!(counts(), (i, i));
        }
        // 겹치는 두 엔진: 첫 시작에서만 begin, 마지막 정지에서만 end
        let a = TimerResolution::acquire_with(&REFS, &API);
        let b = TimerResolution::acquire_with(&REFS, &API);
        assert_eq!(counts(), (4, 3));
        drop(a);
        assert_eq!(counts(), (4, 3), "다른 엔진이 아직 쥐고 있음");
        drop(b);
        assert_eq!(counts(), (4, 4));
        assert_eq!(*REFS.lock(), 0);
    }

    #[test]
    fn timer_begin_failure_never_calls_end() {
        timer_fixture!(REFS, BEGINS, ENDS, API, false);
        let t = TimerResolution::acquire_with(&REFS, &API);
        assert!(!t.active());
        let u = TimerResolution::acquire_with(&REFS, &API);
        assert!(!u.active());
        drop(t);
        drop(u);
        assert_eq!(BEGINS.load(Ordering::SeqCst), 2, "실패하면 다음 엔진이 다시 시도");
        assert_eq!(ENDS.load(Ordering::SeqCst), 0, "성공하지 않은 begin에는 end를 부르지 않음");
        assert_eq!(*REFS.lock(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn real_timer_resolution_pairs() {
        // 실제 winmm: 성공해야 하고 drop으로 놓는다(짝이 깨지면 다음 acquire 카운트가 어긋난다).
        let t = TimerResolution::acquire_with(&TIMER_REFS, &OS_TIMER);
        assert!(t.active());
        drop(t);
    }
}
