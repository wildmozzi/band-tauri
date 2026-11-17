//! 워커 기상 지연 실측(P2-17c): 워커와 같은 수면(`POLL` 0.5ms, `FINE_POLL` 0.15ms)을 MMCSS 스레드(제품과 같이 HighQoS 끔)에서 반복하고
//! 요청보다 늦게 깬 양(overshoot)의 분포를 잰다. 은닉 마감 여유(`DEADLINE_GUARD_US`)를 적응시킬 근거를 모으는 측정 도구다
//! (판정 없음, 표만 출력). 바쁜 일반 우선순위 스레드를 코어 수만큼 띄운 부하 조건도 잰다.
//!
//! 실행: `cargo test --release -p band-engine --test wake_latency -- --ignored --nocapture`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_engine::testing::engine::worker::{FINE_POLL, POLL};
use band_engine::testing::sys::prio::promote_current_thread;

fn q(v: &mut [u64], p: f64) -> u64 {
    v.sort_unstable();
    v[((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1]
}

/// `secs`초 동안 워커처럼 잔다: 10번 중 3번은 `FINE_POLL`(마감 근처), 나머지는 `POLL`. 수면별 overshoot(µs)를 돌려준다.
fn run(secs: f64) -> (Vec<u64>, Vec<u64>) {
    let _g = promote_current_thread();
    let (mut fine, mut poll) = (Vec::new(), Vec::new());
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let mut k = 0u64;
    while Instant::now() < end {
        let d = if k % 10 < 3 { FINE_POLL } else { POLL };
        let t0 = Instant::now();
        std::thread::sleep(d);
        let over = t0.elapsed().saturating_sub(d).as_micros() as u64;
        if d == FINE_POLL { fine.push(over) } else { poll.push(over) }
        k += 1;
    }
    (fine, poll)
}

fn report(label: &str, (mut fine, mut poll): (Vec<u64>, Vec<u64>)) {
    for (name, v) in [("FINE_POLL 150µs", &mut fine), ("POLL 500µs", &mut poll)] {
        let n = v.len();
        let (p50, p99, p999, max) = (q(v, 0.5), q(v, 0.99), q(v, 0.999), *v.iter().max().unwrap());
        println!("| {label} | {name} | {n} | {p50} | {p99} | {p999} | {max} |");
    }
}

#[test]
#[ignore]
fn worker_wake_overshoot() {
    let secs = std::env::var("WAKE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(20.0);
    println!("| 조건 | 수면 | 표본 | p50 µs | p99 µs | p99.9 µs | 최대 µs |\n|---|---|---|---|---|---|---|");
    report("유휴", run(secs));
    // 부하: 논리 코어 수만큼 일반 우선순위 바쁜 루프
    let stop = Arc::new(AtomicBool::new(false));
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let hogs: Vec<_> = (0..n)
        .map(|_| {
            let s = stop.clone();
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !s.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                    std::hint::black_box(x);
                }
            })
        })
        .collect();
    report(&format!("부하(바쁜 스레드 {n})"), run(secs));
    stop.store(true, Ordering::Relaxed);
    for h in hogs {
        let _ = h.join();
    }
}
