//! 플랫폼 CPU 계측(계층 0): TSC, 스레드·프로세스 누적 사이클, 사이클/TSC 비율.
//!
//! Windows x64에서만 값을 낸다(`RDTSC`, `QueryThreadCycleTime`, `QueryProcessCycleTime`). 다른 플랫폼은 None.
//! 창 회전·보고는 `engine::cpuprof`, 단위의 근거(스레드 사이클과 TSC가 같은 틱)는 그 파일 머리 주석.

use std::thread::JoinHandle;

/// 사이클 증분 / TSC 증분(%, 코어 1개 = 100). 어느 쪽이든 못 읽었거나 TSC가 안 늘었으면 None.
pub fn cycle_pct(cycles: Option<u64>, cycles0: Option<u64>, tsc: Option<u64>, tsc0: Option<u64>) -> Option<f32> {
    let dt = tsc?.checked_sub(tsc0?).filter(|&d| d > 0)?;
    let dc = cycles?.saturating_sub(cycles0?);
    Some((dc as f64 / dt as f64 * 100.0) as f32)
}

/// 지금 TSC(`RDTSC`). 사이클 측정을 지원하지 않는 플랫폼이면 None.
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn tsc_now() -> Option<u64> {
    // SAFETY: x86_64는 모두 RDTSC를 지원한다. 부작용 없는 레지스터 읽기.
    Some(unsafe { core::arch::x86_64::_rdtsc() })
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
pub fn tsc_now() -> Option<u64> {
    None
}

/// 스레드의 누적 사이클(TSC 단위, 커널+사용자). 지원하지 않으면 None.
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn thread_cycles<T>(h: &JoinHandle<T>) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;
    let mut c = 0u64;
    // SAFETY: JoinHandle이 살아 있는 동안(join 전) 스레드 핸들은 유효하고 THREAD_ALL_ACCESS다. 출력은 지역 변수.
    let ok = unsafe { QueryThreadCycleTime(h.as_raw_handle() as _, &mut c) };
    (ok != 0).then_some(c)
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
pub fn thread_cycles<T>(_h: &JoinHandle<T>) -> Option<u64> {
    None
}

/// 현재 프로세스의 누적 사이클(TSC 단위, 모든 스레드). 측정 도구용. 지원하지 않으면 None.
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn process_cycles() -> Option<u64> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::Win32::System::WindowsProgramming::QueryProcessCycleTime;
    let mut c = 0u64;
    // SAFETY: GetCurrentProcess는 닫을 필요 없는 의사 핸들이다. 출력은 지역 변수.
    let ok = unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut c) };
    (ok != 0).then_some(c)
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
pub fn process_cycles() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn cycle_pct_edges() {
        assert_eq!(cycle_pct(Some(50), Some(0), Some(1100), Some(100)), Some(5.0));
        assert_eq!(cycle_pct(Some(50), Some(0), Some(100), Some(100)), None, "TSC가 안 늘면 None");
        assert_eq!(cycle_pct(Some(50), Some(0), Some(50), Some(100)), None, "TSC가 거꾸로면 None");
        assert_eq!(cycle_pct(None, Some(0), Some(200), Some(100)), None);
        assert_eq!(cycle_pct(Some(10), Some(20), Some(200), Some(100)), Some(0.0), "사이클 감소는 0으로 포화");
    }

    /// 워커처럼 잠들었다 깨어 짧게 일하는 스레드의 CPU%를 정확히 재는지(리뷰 H1: `GetThreadTimes`는 이 패턴에서
    /// ±50% 틀렸다). 스레드가 스스로 `Instant`로 잰 바쁜 시간 합(기준)과 사이클/TSC 비율을 비교한다.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn cycles_track_sleepy_busy_thread() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        for (sleep_us, busy_us) in [(1000u64, 200u64), (5000, 500), (2500, 1000)] {
            let stop = Arc::new(AtomicBool::new(false));
            let busy_ns = Arc::new(AtomicU64::new(0));
            let (s2, b2) = (stop.clone(), busy_ns.clone());
            let h = std::thread::spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_micros(sleep_us));
                    // 기준 = 실제로 돈 시간만. 연속 두 읽기 사이가 20µs를 넘으면 선점(다른 테스트 스레드가 코어를
                    // 가져감)으로 보고 빼고 센다. 그렇지 않으면 병렬 `cargo test` 부하에서 기준이 부풀어 오른다.
                    let mut ran = Duration::ZERO;
                    let mut prev = Instant::now();
                    while ran < Duration::from_micros(busy_us) {
                        let now = std::hint::black_box(Instant::now());
                        let step = now - prev;
                        if step < Duration::from_micros(20) {
                            ran += step;
                        }
                        prev = now;
                    }
                    b2.fetch_add(ran.as_nanos() as u64, Ordering::Relaxed);
                }
            });
            std::thread::sleep(Duration::from_millis(100));
            let (c0, k0, b0, w0) = (thread_cycles(&h), tsc_now(), busy_ns.load(Ordering::Relaxed), Instant::now());
            std::thread::sleep(Duration::from_millis(1500));
            let (c1, k1, b1, wall) = (thread_cycles(&h), tsc_now(), busy_ns.load(Ordering::Relaxed), w0.elapsed());
            stop.store(true, Ordering::Relaxed);
            h.join().unwrap();
            let measured = cycle_pct(c1, c0, k1, k0).expect("QueryThreadCycleTime/RDTSC") as f64;
            let truth = (b1 - b0) as f64 / wall.as_nanos() as f64 * 100.0;
            // 기준은 바쁜 루프 안 시간뿐이라 깨기·잠들기 비용과 짧은 인터럽트만큼 측정값이 약간 크다. 창 경계의
            // 부분 패스(최대 1회)도 있다. GetThreadTimes의 ±50%와 구별되게 ±20%를 요구한다.
            let err = (measured - truth) / truth;
            eprintln!("sleep {sleep_us}µs / busy {busy_us}µs: 기준 {truth:.2}% vs 사이클 {measured:.2}% ({:+.1}%)", err * 100.0);
            assert!(err.abs() < 0.20, "sleep {sleep_us}µs / busy {busy_us}µs: 기준 {truth:.2}% vs 사이클 {measured:.2}%");
        }
        assert!(process_cycles().unwrap() > 0);
    }
}
