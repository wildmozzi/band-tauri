//! 6엔진 실시간 soak(P1-14, GOAL 2-3 "6인 풀메시 60분 CPU ≤ 15%·재연결 없이 유지", 2-4).
//!
//! 가상 시간 하네스(`engine::sim::long`)와 달리 **실제 시계·실제 스레드·실제 UDP(127.0.0.1)**로 가짜 장치 엔진 6개를
//! 풀메시로 연결해 `SOAK_MINUTES`(기본 60) 동안 연주시키고, 1초마다 `Engine::stats()` 스냅샷을 모아 리포트를 낸다.
//! 측정 도구다: 수치 판정 기준은 없다(첫 실측 뒤 GOAL과 대조해 정한다). 실패는 두 가지뿐이다 —
//! 측정 구간 중 피어가 Connected를 벗어남(재연결), 패닉(테스트 스레드든 엔진 스레드든).
//!
//! 프리셋(클라이언트 `ENGINE_PRESETS`와 같은 값):
//! - `soak_balanced` — 기본(균형): 버퍼 128 · 5ms · Opus · 모노 · 지터 0..80ms · 프로필 0.5
//! - `soak_ultra_low` — 초저지연: 버퍼 64 · 2.5ms · PCM16 · 모노 · 지터 0..40ms · 프로필 0
//!
//! 가짜 장치는 엔진마다 다른 사인파 입력, 장치 클럭 오차 −50..+50ppm(깊이 보정이 60분 내내 일하게), 콜백 기상 지연
//! 0..300µs를 준다. 출력 기록(sink)은 끈다(60분 메모리).
//!
//! 실행(수치는 release 기준이 의미 있다, 한 번에 하나씩):
//! `SOAK_MINUTES=3 cargo test --release --features test-support --test soak soak_balanced -- --ignored --nocapture`
//! 리포트: `SOAK_OUT`(없으면 `<target>/soak-report/`)에 `soak-<프리셋>-<분>min.{md,json}`.
//!
//! `test-support` 피처에서만 컴파일한다: CI realtime 잡(디버그, 기본 피처)은 ignore된 통합 테스트를 모두 돌리므로
//! 60분짜리가 거기 끼지 않게 한다.
#![cfg(feature = "test-support")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::report::EngineStats;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::types::PeerStateKind;
use band_lib::net::NetOptions;
use band_lib::sys::cpu::{cycle_pct, process_cycles, tsc_now};
use parking_lot::Mutex;
use serde_json::json;

const PEERS: usize = 6;
const WARMUP: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_secs(1);

/// 두 프리셋 테스트가 같은 프로세스에서 겹쳐 돌지 않게 한다(엔진 12개면 CPU 수치가 의미 없다).
static SOAK_LOCK: Mutex<()> = Mutex::new(());
/// 어느 스레드든 패닉하면 선다(엔진 워커·수신 스레드 패닉은 테스트 스레드로 전파되지 않는다).
static PANICKED: AtomicBool = AtomicBool::new(false);

struct Preset {
    id: &'static str,
    label: &'static str,
    cfg: EngineConfig,
}

fn preset_balanced() -> Preset {
    Preset {
        id: "balanced",
        label: "기본(균형): 버퍼 128 · 5ms · Opus · 모노 · 프로필 0.5",
        cfg: EngineConfig {
            buffer_size: 128,
            frame_ms: 5.0,
            codec: CodecKind::Opus,
            channels: 1,
            input_channels: vec![0],
            jitter_min_ms: Some(0.0),
            jitter_max_ms: Some(80.0),
            jitter_profile: Some(0.5),
            ..Default::default()
        },
    }
}

fn preset_ultra_low() -> Preset {
    Preset {
        id: "ultra-low",
        label: "초저지연: 버퍼 64 · 2.5ms · PCM16 · 모노 · 프로필 0",
        cfg: EngineConfig {
            buffer_size: 64,
            frame_ms: 2.5,
            codec: CodecKind::Pcm16,
            channels: 1,
            input_channels: vec![0],
            jitter_min_ms: Some(0.0),
            jitter_max_ms: Some(40.0),
            jitter_profile: Some(0.0),
            ..Default::default()
        },
    }
}

fn soak_minutes() -> f64 {
    std::env::var("SOAK_MINUTES").ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|m| *m > 0.0).unwrap_or(60.0)
}

fn out_dir() -> PathBuf {
    match std::env::var_os("SOAK_OUT") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        // <target>/tmp의 부모 = 공유 target 디렉터리(워크트리 안에 target/을 만들지 않는다)
        _ => PathBuf::from(env!("CARGO_TARGET_TMPDIR")).parent().map(|p| p.join("soak-report")).unwrap_or_else(|| "soak-report".into()),
    }
}

fn test_net() -> NetOptions {
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None }
}

fn backend(i: usize) -> FakeBackend {
    let hz = 220.0 * (i + 1) as f64;
    let mut b = FakeBackend::new(
        48_000,
        Arc::new(move |start: u64, ch: usize, buf: &mut [f32]| {
            for (k, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + k as u64) as f64 / 48_000.0;
                frame.fill((t * hz * 2.0 * std::f64::consts::PI).sin() as f32 * 0.3);
            }
        }),
    );
    b.record_output = false;
    // 엔진마다 다른 장치 클럭(−50, −30, −10, +10, +30, +50ppm). 입력·출력은 같은 장치라 같은 오차.
    let ppm = (i as f64 - 2.5) * 20.0;
    b.in_drift_ppm = ppm;
    b.out_drift_ppm = ppm;
    b.wake_jitter_us = 300;
    b.seed = 0x50a0 + i as u64;
    b
}

fn addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn with_candidate(d: &LocalDescriptor, a: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: a.to_string() }], ..d.clone() }
}

/// 프로세스 작업 집합(RSS, 바이트). Windows 전용, 실패하면 None.
#[cfg(windows)]
fn rss_bytes() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
    }
    let mut c = ProcessMemoryCounters { cb: std::mem::size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
    // SAFETY: -1은 현재 프로세스 의사 핸들(닫지 않는다). 출력은 크기를 맞춘 지역 구조체.
    let ok = unsafe { K32GetProcessMemoryInfo(-1, &mut c, c.cb) };
    (ok != 0).then_some(c.working_set_size as u64)
}

#[cfg(not(windows))]
fn rss_bytes() -> Option<u64> {
    None
}

/// 정렬된 표본의 분위수(최근접 순위). 비었으면 NaN.
fn q(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return f32::NAN;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn sorted(mut v: Vec<f32>) -> Vec<f32> {
    v.retain(|x| x.is_finite());
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// 엔진 i → 피어 j 한 쌍의 측정 누적.
#[derive(Default, Clone)]
struct Link {
    peer_id: String,
    /// Connected가 아닌 상태로 관찰된 초(1초 표본 수).
    off_secs: u32,
    /// Connected → 다른 상태로 바뀐 횟수(1초 표본 기준이라 1초보다 짧은 이탈은 놓칠 수 있다).
    off_events: u32,
    /// 마지막으로 본 상태가 Connected였나.
    was_connected: bool,
    /// 측정 시작 시 누적 은닉 프레임·늦은 패킷.
    concealed0: u64,
    late0: u64,
    concealed1: u64,
    late1: u64,
    /// 5초 창 은닉률 최대(%).
    conceal_win_max: f32,
    loss_max: f32,
    buffer: Vec<f32>,
    target: Vec<f32>,
    playout: Vec<f32>,
    max_corr_cents: f32,
}

#[derive(Default)]
struct Minute {
    conceal_win: Vec<f32>,
    buffer: Vec<f32>,
    node_cpu: Vec<f32>,
    xruns: u64,
    rss: Option<u64>,
}

struct Report {
    preset: &'static str,
    label: &'static str,
    minutes: f64,
    elapsed_s: f64,
    connect_s: f64,
    ticks: u32,
    late_ticks: u32,
    links: Vec<Vec<Link>>,
    xruns_warmup: u64,
    xruns: Vec<u64>,
    stream_errors: u64,
    send_q_p99: Vec<f32>,
    send_q_max: f32,
    worker_pct: Vec<f32>,
    cb_p99_us: Vec<f32>,
    cb_max_us: f32,
    node_cpu_tick: Vec<f32>,
    node_cpu_total: f32,
    rss_start: Option<u64>,
    rss_end: Option<u64>,
    rss_max: Option<u64>,
    timeline: Vec<Minute>,
    panicked: bool,
    not_running: Vec<String>,
}

fn run(preset: Preset) {
    let _g = SOAK_LOCK.lock();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICKED.store(true, Ordering::SeqCst);
        prev(info);
    }));
    PANICKED.store(false, Ordering::SeqCst);

    let minutes = soak_minutes();
    let frames_per_s = 1000.0 / preset.cfg.frame_ms as f64;
    println!("soak {} — {} 분, {}", preset.id, minutes, preset.label);

    let engines: Vec<Engine> = (0..PEERS)
        .map(|i| {
            Engine::start(preset.cfg.clone(), Arc::new(backend(i)), Arc::new(MixControls::default()), Some((format!("p{i}"), test_net())))
                .unwrap()
        })
        .collect();
    let t_start = Instant::now();
    for (i, e) in engines.iter().enumerate() {
        for (j, o) in engines.iter().enumerate() {
            if i != j {
                e.add_peer(format!("p{j}"), &with_candidate(o.descriptor().unwrap(), addr(o))).unwrap();
            }
        }
    }
    while !engines.iter().all(|e| {
        let s = e.stats();
        s.peers.len() == PEERS - 1 && s.peers.iter().all(|p| p.state == PeerStateKind::Connected)
    }) {
        assert!(t_start.elapsed() < Duration::from_secs(30), "풀메시 연결 실패");
        std::thread::sleep(Duration::from_millis(50));
    }
    let connect_s = t_start.elapsed().as_secs_f64();
    std::thread::sleep(WARMUP);

    // 측정 시작 기준
    let s0: Vec<EngineStats> = engines.iter().map(|e| e.stats()).collect();
    let xr0: Vec<u64> = s0.iter().map(|s| s.xruns).collect();
    let se0: u64 = s0.iter().map(|s| s.stream_errors as u64).sum();
    let mut links: Vec<Vec<Link>> = s0
        .iter()
        .map(|s| {
            s.peers
                .iter()
                .map(|p| Link {
                    peer_id: p.peer_id.clone(),
                    was_connected: true,
                    concealed0: p.concealed,
                    late0: p.late,
                    concealed1: p.concealed,
                    late1: p.late,
                    ..Default::default()
                })
                .collect()
        })
        .collect();
    let rss_start = rss_bytes();
    let mut rss_max = rss_start;
    let (pc0, tsc0) = (process_cycles(), tsc_now());
    let (mut pc_prev, mut tsc_prev) = (pc0, tsc0);

    let dur = Duration::from_secs_f64(minutes * 60.0);
    let t0 = Instant::now();
    let mut next = t0 + TICK;
    let mut ticks = 0u32;
    let mut late_ticks = 0u32;
    let mut send_q_p99 = Vec::new();
    let mut send_q_max = 0f32;
    let mut worker_pct = Vec::new();
    let mut cb_p99_us = Vec::new();
    let mut cb_max_us = 0f32;
    let mut node_cpu_tick = Vec::new();
    let mut timeline: Vec<Minute> = Vec::new();
    let mut xr_minute_prev: u64 = xr0.iter().sum();
    let mut last: Vec<EngineStats> = s0;

    while t0.elapsed() < dur && !PANICKED.load(Ordering::SeqCst) {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > TICK / 2 {
            late_ticks += 1;
        }
        next += TICK;
        let stats: Vec<EngineStats> = engines.iter().map(|e| e.stats()).collect();
        let (pc, tsc) = (process_cycles(), tsc_now());
        let node = cycle_pct(pc, pc_prev, tsc, tsc_prev).map(|p| p / PEERS as f32 / 4.0);
        (pc_prev, tsc_prev) = (pc, tsc);
        let rss = rss_bytes();
        if let (Some(r), Some(m)) = (rss, rss_max) {
            rss_max = Some(r.max(m));
        }
        ticks += 1;

        let minute_idx = ((ticks - 1) / 60) as usize;
        while timeline.len() <= minute_idx {
            timeline.push(Minute::default());
        }
        let m = &mut timeline[minute_idx];
        if let Some(n) = node {
            node_cpu_tick.push(n);
            m.node_cpu.push(n);
        }
        m.rss = rss;

        for (i, s) in stats.iter().enumerate() {
            if let Some(sq) = &s.send_queue {
                if sq.packets > 0 {
                    send_q_p99.push(sq.p99_ms);
                    send_q_max = send_q_max.max(sq.max_ms);
                }
            }
            if let Some(w) = s.cpu.worker_pct {
                worker_pct.push(w);
            }
            if s.cpu.callbacks > 0 {
                cb_p99_us.push(s.cpu.callback_p99_us);
                cb_max_us = cb_max_us.max(s.cpu.callback_max_us);
            }
            for l in links[i].iter_mut() {
                let Some(p) = s.peers.iter().find(|p| p.peer_id == l.peer_id) else {
                    // 피어가 목록에서 사라짐 = 이탈로 센다
                    l.off_secs += 1;
                    if l.was_connected {
                        l.off_events += 1;
                    }
                    l.was_connected = false;
                    continue;
                };
                let connected = p.state == PeerStateKind::Connected;
                if !connected {
                    l.off_secs += 1;
                    if l.was_connected {
                        l.off_events += 1;
                    }
                }
                l.was_connected = connected;
                l.concealed1 = p.concealed;
                l.late1 = p.late;
                l.conceal_win_max = l.conceal_win_max.max(p.conceal_pct);
                l.loss_max = l.loss_max.max(p.loss_pct);
                l.buffer.push(p.buffer_ms);
                l.target.push(p.target_ms);
                if p.playout_ms > 0.0 {
                    l.playout.push(p.playout_ms);
                }
                l.max_corr_cents = l.max_corr_cents.max(p.max_corr_cents);
                m.conceal_win.push(p.conceal_pct);
                m.buffer.push(p.buffer_ms);
            }
        }
        let xr_now: u64 = stats.iter().map(|s| s.xruns).sum();
        m.xruns += xr_now - xr_minute_prev;
        xr_minute_prev = xr_now;
        last = stats;

        if ticks.is_multiple_of(60) {
            let off: u32 = links.iter().flatten().map(|l| l.off_secs).sum();
            println!(
                "  {:>4.0}분: 은닉(5초 창) 최대 {:.2}% · 버퍼 p50 {:.2}ms · CPU(1명, 4코어) {:.2}% · xrun {} · 이탈 {}초 · RSS {:.1}MB",
                t0.elapsed().as_secs_f64() / 60.0,
                timeline[minute_idx].conceal_win.iter().cloned().fold(0.0, f32::max),
                q(&sorted(timeline[minute_idx].buffer.clone()), 0.5),
                node.unwrap_or(f32::NAN),
                xr_now - xr0.iter().sum::<u64>(),
                off,
                rss.unwrap_or(0) as f64 / 1e6
            );
        }
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    let (pc1, tsc1) = (process_cycles(), tsc_now());
    let node_cpu_total = cycle_pct(pc1, pc0, tsc1, tsc0).map(|p| p / PEERS as f32 / 4.0).unwrap_or(f32::NAN);
    let rss_end = rss_bytes();
    let not_running: Vec<String> = last.iter().enumerate().filter(|(_, s)| !s.running).map(|(i, _)| format!("p{i}")).collect();

    let report = Report {
        preset: preset.id,
        label: preset.label,
        minutes,
        elapsed_s,
        connect_s,
        ticks,
        late_ticks,
        xruns_warmup: xr0.iter().sum(),
        xruns: last.iter().zip(&xr0).map(|(s, x)| s.xruns - x).collect(),
        stream_errors: last.iter().map(|s| s.stream_errors as u64).sum::<u64>() - se0,
        links,
        send_q_p99,
        send_q_max,
        worker_pct,
        cb_p99_us,
        cb_max_us,
        node_cpu_tick,
        node_cpu_total,
        rss_start,
        rss_end,
        rss_max,
        timeline,
        panicked: PANICKED.load(Ordering::SeqCst),
        not_running,
    };
    for mut e in engines {
        e.stop();
    }
    let _ = std::panic::take_hook();

    let (md, js) = render(&report, frames_per_s);
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let stem = format!("soak-{}-{}min", report.preset, fmt_minutes(minutes));
    let md_path = dir.join(format!("{stem}.md"));
    std::fs::write(&md_path, &md).unwrap();
    std::fs::write(dir.join(format!("{stem}.json")), serde_json::to_string_pretty(&js).unwrap()).unwrap();
    println!("\n{md}\n리포트: {}", md_path.display());

    // 실패 조건: 패닉, 엔진 정지, 측정 구간 중 Connected 이탈. 나머지는 측정만 한다.
    assert!(!report.panicked, "soak 중 패닉");
    assert!(report.not_running.is_empty(), "정지한 엔진: {:?}", report.not_running);
    let off: Vec<String> = report
        .links
        .iter()
        .enumerate()
        .flat_map(|(i, ls)| ls.iter().filter(|l| l.off_secs > 0).map(move |l| format!("p{i}→{} {}회 {}초", l.peer_id, l.off_events, l.off_secs)))
        .collect();
    assert!(off.is_empty(), "Connected 이탈: {off:?}");
}

fn fmt_minutes(m: f64) -> String {
    if m.fract() == 0.0 {
        format!("{m:.0}")
    } else {
        format!("{m}")
    }
}

fn mb(b: Option<u64>) -> String {
    b.map(|b| format!("{:.1}", b as f64 / 1e6)).unwrap_or_else(|| "n/a".into())
}

fn render(r: &Report, frames_per_s: f64) -> (String, serde_json::Value) {
    let frames = r.elapsed_s * frames_per_s;
    let link_rows: Vec<(usize, &Link, f64, f64)> = r
        .links
        .iter()
        .enumerate()
        .flat_map(|(i, ls)| {
            ls.iter().map(move |l| {
                let c = (l.concealed1 - l.concealed0) as f64;
                (i, l, c, (l.late1 - l.late0) as f64)
            })
        })
        .collect();
    let conceal_total: f64 = link_rows.iter().map(|(_, _, c, _)| c).sum();
    let conceal_all_pct = conceal_total / (frames * link_rows.len() as f64) * 100.0;
    let per_link_pct: Vec<f32> = link_rows.iter().map(|(_, _, c, _)| (c / frames * 100.0) as f32).collect();
    let per_link_sorted = sorted(per_link_pct.clone());
    let all_buffer = sorted(r.links.iter().flatten().flat_map(|l| l.buffer.iter().cloned()).collect());
    let all_playout = sorted(r.links.iter().flatten().flat_map(|l| l.playout.iter().cloned()).collect());
    let send_q = sorted(r.send_q_p99.clone());
    let node_tick = sorted(r.node_cpu_tick.clone());
    let worker = sorted(r.worker_pct.clone());
    let cb = sorted(r.cb_p99_us.clone());
    let off_secs: u32 = r.links.iter().flatten().map(|l| l.off_secs).sum();
    let off_events: u32 = r.links.iter().flatten().map(|l| l.off_events).sum();
    let xr_total: u64 = r.xruns.iter().sum();

    let mut md = String::new();
    use std::fmt::Write;
    let _ = writeln!(md, "# soak — {} ({} 분)\n", r.label, fmt_minutes(r.minutes));
    let _ = writeln!(
        md,
        "6엔진 풀메시(127.0.0.1 UDP), 가짜 장치(클럭 −50..+50ppm, 기상 지연 0..300µs), 실제 시계. 측정 {:.1}초({}회 표본, 늦은 표본 {}), 연결 {:.2}초 + 예열 {}초 제외.\n",
        r.elapsed_s,
        r.ticks,
        r.late_ticks,
        r.connect_s,
        WARMUP.as_secs()
    );
    let _ = writeln!(md, "| 항목 | 값 |\n|---|---|");
    let _ = writeln!(md, "| 피어 상태 이탈(Connected 아님, 30쌍 합) | {off_events}회 · {off_secs}초 |");
    let _ = writeln!(md, "| 패닉 / 정지한 엔진 | {} / {:?} |", r.panicked, r.not_running);
    let _ = writeln!(md, "| xrun(측정 구간, 6엔진 합 / 엔진별) | {xr_total} / {:?} (예열까지 {}) |", r.xruns, r.xruns_warmup);
    let _ = writeln!(md, "| 스트림 에러(측정 구간) | {} |", r.stream_errors);
    let _ = writeln!(
        md,
        "| 은닉률 전체(누적 은닉 프레임 / 재생 프레임) | {:.3}% |",
        conceal_all_pct
    );
    let _ = writeln!(
        md,
        "| 은닉률 피어 쌍별 p50 / 최대 | {:.3}% / {:.3}% |",
        q(&per_link_sorted, 0.5),
        per_link_sorted.last().cloned().unwrap_or(f32::NAN)
    );
    let _ = writeln!(
        md,
        "| 은닉률 5초 창 최대(전 쌍) | {:.2}% |",
        r.links.iter().flatten().map(|l| l.conceal_win_max).fold(0.0, f32::max)
    );
    let _ = writeln!(md, "| 버퍼 지연 p50 / p99 / 최대 (ms) | {:.2} / {:.2} / {:.2} |", q(&all_buffer, 0.5), q(&all_buffer, 0.99), all_buffer.last().cloned().unwrap_or(f32::NAN));
    let _ = writeln!(md, "| 재생 지연(playout) p50 / p99 (ms) | {:.2} / {:.2} |", q(&all_playout, 0.5), q(&all_playout, 0.99));
    let _ = writeln!(md, "| 송신 큐 지연: 2초 창 p99의 p50 / p99, 최대 (ms) | {:.3} / {:.3}, {:.3} |", q(&send_q, 0.5), q(&send_q, 0.99), r.send_q_max);
    let _ = writeln!(
        md,
        "| 프로세스 CPU 연주자 1명 몫(4코어 기준 %) 전체 평균 / 1초 p50 / p99 / 최대 | **{:.2}** / {:.2} / {:.2} / {:.2} |",
        r.node_cpu_total,
        q(&node_tick, 0.5),
        q(&node_tick, 0.99),
        node_tick.last().cloned().unwrap_or(f32::NAN)
    );
    let _ = writeln!(md, "| 워커 CPU(1코어 %) p50 / p99 | {:.2} / {:.2} |", q(&worker, 0.5), q(&worker, 0.99));
    let _ = writeln!(md, "| 출력 콜백 p99(µs) p50 / p99, 최대 | {:.0} / {:.0}, {:.0} |", q(&cb, 0.5), q(&cb, 0.99), r.cb_max_us);
    let _ = writeln!(md, "| RSS 처음 / 끝 / 최대 (MB) | {} / {} / {} |", mb(r.rss_start), mb(r.rss_end), mb(r.rss_max));

    let _ = writeln!(md, "\n## 피어 쌍별\n\n| 쌍 | 이탈 회/초 | 은닉 % | 늦은 패킷 | 5초 창 은닉 최대 % | 손실 최대 % | 버퍼 p50/p99 ms | 목표 p50 ms | 보정 최대 cent |\n|---|---|---|---|---|---|---|---|---|");
    for ((i, l, _, late), pct) in link_rows.iter().zip(&per_link_pct) {
        let b = sorted(l.buffer.clone());
        let t = sorted(l.target.clone());
        let _ = writeln!(
            md,
            "| p{i}→{} | {}/{} | {:.3} | {} | {:.2} | {:.2} | {:.2}/{:.2} | {:.2} | {:.2} |",
            l.peer_id,
            l.off_events,
            l.off_secs,
            pct,
            late,
            l.conceal_win_max,
            l.loss_max,
            q(&b, 0.5),
            q(&b, 0.99),
            q(&t, 0.5),
            l.max_corr_cents
        );
    }
    let _ = writeln!(md, "\n## 분 단위 추이\n\n| 분 | 5초 창 은닉 최대 % | 버퍼 p50 ms | CPU 1명 몫 평균 % | xrun | RSS MB |\n|---|---|---|---|---|---|");
    for (k, m) in r.timeline.iter().enumerate() {
        let b = sorted(m.buffer.clone());
        let cpu = if m.node_cpu.is_empty() { f32::NAN } else { m.node_cpu.iter().sum::<f32>() / m.node_cpu.len() as f32 };
        let _ = writeln!(
            md,
            "| {k} | {:.2} | {:.2} | {:.2} | {} | {} |",
            m.conceal_win.iter().cloned().fold(0.0, f32::max),
            q(&b, 0.5),
            cpu,
            m.xruns,
            mb(m.rss)
        );
    }

    let js = json!({
        "preset": r.preset,
        "label": r.label,
        "minutes": r.minutes,
        "elapsedS": r.elapsed_s,
        "connectS": r.connect_s,
        "warmupS": WARMUP.as_secs(),
        "ticks": r.ticks,
        "lateTicks": r.late_ticks,
        "panicked": r.panicked,
        "notRunning": r.not_running,
        "peerStateExcursions": { "events": off_events, "seconds": off_secs },
        "xruns": { "total": xr_total, "perEngine": r.xruns, "untilWarmupEnd": r.xruns_warmup },
        "streamErrors": r.stream_errors,
        "concealPct": {
            "overall": conceal_all_pct,
            "perLinkP50": q(&per_link_sorted, 0.5),
            "perLinkMax": per_link_sorted.last(),
            "window5sMax": r.links.iter().flatten().map(|l| l.conceal_win_max).fold(0.0, f32::max),
        },
        "bufferMs": { "p50": q(&all_buffer, 0.5), "p99": q(&all_buffer, 0.99), "max": all_buffer.last() },
        "playoutMs": { "p50": q(&all_playout, 0.5), "p99": q(&all_playout, 0.99) },
        "sendQueueMs": { "p99P50": q(&send_q, 0.5), "p99P99": q(&send_q, 0.99), "max": r.send_q_max },
        "nodeCpuPct4Core": { "mean": r.node_cpu_total, "tickP50": q(&node_tick, 0.5), "tickP99": q(&node_tick, 0.99), "tickMax": node_tick.last() },
        "workerPct1Core": { "p50": q(&worker, 0.5), "p99": q(&worker, 0.99) },
        "callbackP99Us": { "p50": q(&cb, 0.5), "p99": q(&cb, 0.99), "max": r.cb_max_us },
        "rssBytes": { "start": r.rss_start, "end": r.rss_end, "max": r.rss_max },
        "links": link_rows.iter().zip(&per_link_pct).map(|((i, l, _, late), pct)| {
            let b = sorted(l.buffer.clone());
            json!({
                "from": format!("p{i}"),
                "to": l.peer_id,
                "offEvents": l.off_events,
                "offSeconds": l.off_secs,
                "concealPct": pct,
                "late": late,
                "concealWindowMaxPct": l.conceal_win_max,
                "lossMaxPct": l.loss_max,
                "bufferP50Ms": q(&b, 0.5),
                "bufferP99Ms": q(&b, 0.99),
                "maxCorrCents": l.max_corr_cents,
            })
        }).collect::<Vec<_>>(),
        "timeline": r.timeline.iter().enumerate().map(|(k, m)| json!({
            "minute": k,
            "concealWindowMaxPct": m.conceal_win.iter().cloned().fold(0.0, f32::max),
            "bufferP50Ms": q(&sorted(m.buffer.clone()), 0.5),
            "nodeCpuPct4Core": if m.node_cpu.is_empty() { None } else { Some(m.node_cpu.iter().sum::<f32>() / m.node_cpu.len() as f32) },
            "xruns": m.xruns,
            "rssBytes": m.rss,
        })).collect::<Vec<_>>(),
    });
    (md, js)
}

#[test]
#[ignore = "soak(측정 도구, 기본 60분): SOAK_MINUTES=3 cargo test --release --features test-support --test soak soak_balanced -- --ignored --nocapture"]
fn soak_balanced() {
    run(preset_balanced());
}

#[test]
#[ignore = "soak(측정 도구, 기본 60분): SOAK_MINUTES=3 cargo test --release --features test-support --test soak soak_ultra_low -- --ignored --nocapture"]
fn soak_ultra_low() {
    run(preset_ultra_low());
}
