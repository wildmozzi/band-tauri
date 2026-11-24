//! 스레드 정책 × 환경 교차 측정(P1-21 3단계 ② — P 코어 "우선 배치" 기본값을 정하는 근거).
//!
//! 정책 4종(끔 · 고정 · 선호 힌트(2안) · 조건부 고정(3안) — **모두 HighQoS 켬**) × 환경 2종(조용함 / 경합)을 라운드마다 순서를
//! 뒤집어 가며 잰다. 실행 하나 = 새 프로세스(환경변수 `BAND_THREAD_POLICY`만 다르다)에서 6엔진 풀메시(`tests/support/mesh_cpu.rs` —
//! `cpu_profile`과 같은 본체)를 구성마다 예열 + 측정 창만큼 돌린 것이다. 낸 값: 워커 CPU, 반복 본문 `iter_body` p50/p99, xrun,
//! 최대 은닉률, 코어 배치(`corePlacement` — 워커·수신 스레드가 P/E/LP-E에서 깨어난 비율), 조건부 고정의 풀기·다시 묶기 횟수.
//!
//! 경합 환경: **다른 프로세스**(이 실행파일의 `--child-hog`)가 P 코어 논리 프로세서 수만큼 바쁜 스레드를 띄운다. 스레드마다
//! P 코어 CPU set에 묶이고(= "다른 프로그램이 P 코어를 차지") 엔진 스레드와 같은 MMCSS "Pro Audio"로 등록한다(`--hog-prio mmcss`,
//! 기본). 2단계 측정이 재지 못한 "같은 우선순위 부하가 P 코어를 채운 상황"이다.
//!
//! 먼저 프로브로 선호 힌트가 실제로 soft한지 본다: 보통 우선순위 바쁜 스레드를 P 코어 수의 2배만큼 띄워 정책을 걸고, P 코어에
//! 머문 비율과 프로세스가 쓴 코어 수를 잰다(고정이면 P 100%·코어 ≈ P 수, soft면 E로 넘쳐 코어 ≈ 스레드 수).
//!
//! ```text
//! cd client/src-tauri
//! cargo run --release -p band-engine --example policy_matrix                                   # 본측정: 10라운드, 창 20초(약 3시간 17분)
//! cargo run --release -p band-engine --example policy_matrix -- --minutes 90                   # 시간에 맞춰 라운드 수를 정한다(90분 → 4라운드)
//! cargo run --release -p band-engine --example policy_matrix -- --dry-run                      # 계획과 예상 소요 시간만
//! cargo run --release -p band-engine --example policy_matrix -- --rounds 2 --window-s 5 --warmup-s 2 --configs opus2.5,pcm5 --cooldown-s 2   # 연기 시험
//! ```
//!
//! 결과는 `--out` 폴더(기본 `policy-matrix-<UTC 시각>`)에 `results.md`(표) · `results.json`(요약 + 실행별 원값) · `runs.jsonl`
//! (실행이 끝날 때마다 한 줄 — 중간에 멈춰도 남는다). 인자는 `--help`.
//!
//! 주의: 경합 환경은 실시간 우선순위 스레드로 P 코어를 채운다 — 도는 동안 같은 PC의 다른 일이 느려진다. 부하 프로세스는 부모가
//! 죽어도 `--max-s` 뒤 스스로 끝난다. 수치 판정은 없다(표만 낸다).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use band_dsp::config::CodecKind;
use band_engine::testing::sys::cpu::{cycle_pct, process_cycles, tsc_now};
use band_engine::testing::sys::placement::{apply_policy_to_current_thread, current_core_kind, describe_cores, p_core_count};
use band_engine::testing::sys::prio::promote_current_thread;
use serde_json::{json, Value};

#[path = "../tests/support/mesh_cpu.rs"]
mod mesh_cpu;

/// 정책: (이름, `BAND_THREAD_POLICY` 값, 표 머리). 모두 HighQoS 켬 — 다른 것은 P 코어 정책뿐이다.
const POLICIES: [(&str, &str, &str); 4] =
    [("off", "highqos", "끔"), ("pin", "highqos+pin", "고정"), ("prefer", "highqos+prefer", "선호 힌트(2안)"), ("adaptive", "highqos+adaptive", "조건부 고정(3안)")];
const ENVS: [(&str, &str); 2] = [("quiet", "조용함"), ("contended", "경합")];
/// 구성: (이름, 코덱, 프레임 ms, 5피어 Opus 개별 인코딩 강제). `cpu_profile`의 6구성과 같다.
const CONFIGS: [(&str, CodecKind, f32, bool); 6] = [
    ("opus2.5", CodecKind::Opus, 2.5, false),
    ("pcm2.5", CodecKind::Pcm16, 2.5, false),
    ("pcm2.5+opus", CodecKind::Pcm16, 2.5, true),
    ("opus5", CodecKind::Opus, 5.0, false),
    ("pcm5", CodecKind::Pcm16, 5.0, false),
    ("pcm5+opus", CodecKind::Pcm16, 5.0, true),
];
/// 구성 하나에 예열·측정 창 말고 드는 시간(엔진 시작·풀메시 연결·정지) 어림값(초) — 소요 시간 추정에만 쓴다. 225H 연기 시험
/// (2026-11-24, 16실행)에서 실행 하나가 구성 2개 × 8초 + 약 1초였다.
const CONFIG_OVERHEAD_S: f64 = 0.5;
/// 실행 하나(프로세스 시작·부하 올리기·정리·전원 확인 몫)에 드는 시간 어림값(초).
const RUN_OVERHEAD_S: f64 = 2.0;

const HELP: &str = "\
정책 × 환경 교차 측정 (P1-21 3단계 ②)

  --rounds N         라운드 수(기본 10). 라운드 하나 = 환경 × 정책 실행 한 바퀴(순서는 라운드마다 뒤집는다)
  --minutes M        총 소요 시간(분)에 맞춰 라운드 수를 정한다(--rounds 대신. 최소 1라운드)
  --window-s S       구성마다 측정 창(초, 기본 20 — 조건부 고정의 판단 창이 1초·기다림이 10초부터라 2단계의 12초보다 길게 잡는다)
  --warmup-s S       구성마다 예열(초, 기본 3)
  --cooldown-s S     실행 사이 쉬는 시간(초, 기본 5)
  --configs a,b      잴 구성(기본 전부): opus2.5,pcm2.5,pcm2.5+opus,opus5,pcm5,pcm5+opus
  --policies a,b     잴 정책(기본 전부): off,pin,prefer,adaptive  (모두 HighQoS 켬)
  --envs a,b         잴 환경(기본 전부): quiet,contended
  --hog-threads N    경합 부하의 바쁜 스레드 수(기본 = P 코어 논리 프로세서 수)
  --hog-duty D       부하 스레드가 바쁜 비율 0..1(기본 1.0 — 주기 10ms 중 D만큼 돌고 나머지는 잔다)
  --hog-prio P       부하 스레드 우선순위: mmcss(기본 — 엔진 스레드와 같은 \"Pro Audio\") | normal
  --no-probe         선호 힌트 soft 확인 프로브를 건너뛴다
  --out DIR          결과 폴더(기본 policy-matrix-<UTC 시각>)
  --dry-run          계획과 예상 소요 시간만 찍고 끝낸다
";

#[derive(Clone)]
struct Opts {
    rounds: usize,
    minutes: Option<f64>,
    window_s: f64,
    warmup_s: f64,
    cooldown_s: f64,
    configs: Vec<String>,
    policies: Vec<String>,
    envs: Vec<String>,
    hog_threads: Option<usize>,
    hog_duty: f64,
    hog_prio: String,
    probe: bool,
    out: Option<PathBuf>,
    dry_run: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            rounds: 10,
            minutes: None,
            window_s: 20.0,
            warmup_s: 3.0,
            cooldown_s: 5.0,
            configs: CONFIGS.iter().map(|c| c.0.to_string()).collect(),
            policies: POLICIES.iter().map(|p| p.0.to_string()).collect(),
            envs: ENVS.iter().map(|e| e.0.to_string()).collect(),
            hog_threads: None,
            hog_duty: 1.0,
            hog_prio: "mmcss".into(),
            probe: true,
            out: None,
            dry_run: false,
        }
    }
}

/// `--이름 값` 인자 목록에서 값을 꺼낸다.
struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }

    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(i) = self.0.iter().position(|a| a == name) else { return Ok(None) };
        if i + 1 >= self.0.len() {
            return Err(format!("{name}: 값이 없다"));
        }
        let v = self.0.remove(i + 1);
        self.0.remove(i);
        Ok(Some(v))
    }

    fn num<T: std::str::FromStr>(&mut self, name: &str) -> Result<Option<T>, String> {
        match self.value(name)? {
            Some(v) => v.parse().map(Some).map_err(|_| format!("{name}: 숫자가 아니다({v})")),
            None => Ok(None),
        }
    }

    /// 쉼표 목록. 모든 항목이 `known`에 있어야 한다.
    fn list(&mut self, name: &str, known: &[&str]) -> Result<Option<Vec<String>>, String> {
        let Some(v) = self.value(name)? else { return Ok(None) };
        let items: Vec<String> = v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
        if items.is_empty() {
            return Err(format!("{name}: 비어 있다"));
        }
        if let Some(bad) = items.iter().find(|i| !known.contains(&i.as_str())) {
            return Err(format!("{name}: 모르는 값 {bad} (가능: {})", known.join(",")));
        }
        Ok(Some(items))
    }

    fn done(self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(format!("모르는 인자: {}", self.0.join(" ")))
        }
    }
}

fn parse_opts(mut a: Args) -> Result<Opts, String> {
    let mut o = Opts::default();
    if let Some(v) = a.num("--rounds")? {
        o.rounds = v;
    }
    o.minutes = a.num("--minutes")?;
    if let Some(v) = a.num("--window-s")? {
        o.window_s = v;
    }
    if let Some(v) = a.num("--warmup-s")? {
        o.warmup_s = v;
    }
    if let Some(v) = a.num("--cooldown-s")? {
        o.cooldown_s = v;
    }
    if let Some(v) = a.list("--configs", &CONFIGS.map(|c| c.0))? {
        o.configs = v;
    }
    if let Some(v) = a.list("--policies", &POLICIES.map(|p| p.0))? {
        o.policies = v;
    }
    if let Some(v) = a.list("--envs", &ENVS.map(|e| e.0))? {
        o.envs = v;
    }
    o.hog_threads = a.num("--hog-threads")?;
    if let Some(v) = a.num("--hog-duty")? {
        o.hog_duty = v;
    }
    if let Some(v) = a.value("--hog-prio")? {
        o.hog_prio = v;
    }
    o.probe = !a.flag("--no-probe");
    o.out = a.value("--out")?.map(PathBuf::from);
    o.dry_run = a.flag("--dry-run");
    a.done()?;
    let positive = |x: f64| x.is_finite() && x > 0.0;
    if o.rounds == 0 || !positive(o.window_s) || !(o.warmup_s >= 0.0 && o.cooldown_s >= 0.0) {
        return Err("--rounds ≥ 1, --window-s > 0, --warmup-s·--cooldown-s ≥ 0 이어야 한다".into());
    }
    if !positive(o.hog_duty) || o.hog_duty > 1.0 {
        return Err("--hog-duty는 0 초과 1 이하".into());
    }
    if !["mmcss", "normal"].contains(&o.hog_prio.as_str()) {
        return Err("--hog-prio는 mmcss | normal".into());
    }
    if o.minutes.is_some_and(|m| !positive(m)) {
        return Err("--minutes는 0보다 커야 한다".into());
    }
    Ok(o)
}

/// 실행 하나(환경 × 정책, 구성 전부)의 예상 시간(초).
fn run_seconds(o: &Opts) -> f64 {
    o.configs.len() as f64 * (o.warmup_s + o.window_s + CONFIG_OVERHEAD_S) + RUN_OVERHEAD_S + o.cooldown_s
}

/// 라운드 하나의 예상 시간(초).
fn round_seconds(o: &Opts) -> f64 {
    run_seconds(o) * (o.envs.len() * o.policies.len()) as f64
}

/// `--minutes`가 있으면 그 시간에 들어가는 라운드 수(최소 1), 없으면 `--rounds`.
fn planned_rounds(o: &Opts) -> usize {
    match o.minutes {
        Some(m) => ((m * 60.0 / round_seconds(o)).floor() as usize).max(1),
        None => o.rounds,
    }
}

/// 라운드 `round`의 실행 순서: 짝수 라운드는 정순, 홀수 라운드는 환경·정책 모두 역순(느린 변화 — 온도·다른 작업 — 가 한 조건에
/// 몰리지 않게).
fn round_order(o: &Opts, round: usize) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = o.envs.iter().flat_map(|e| o.policies.iter().map(move |p| (e.clone(), p.clone()))).collect();
    if round % 2 == 1 {
        v.reverse();
    }
    v
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    let child = argv.first().cloned().unwrap_or_default();
    let result = match child.as_str() {
        "--child-measure" => child_measure(Args(argv.split_off(1))),
        "--child-hog" => child_hog(Args(argv.split_off(1))),
        "--child-probe" => child_probe(Args(argv.split_off(1))),
        _ => parse_opts(Args(argv)).and_then(orchestrate),
    };
    if let Err(e) = result {
        eprintln!("policy_matrix: {e}\n(--help)");
        std::process::exit(2);
    }
}

// ───────────────────────── 자식: 측정 ─────────────────────────

/// 환경변수 `BAND_THREAD_POLICY`(부모가 정함)로 구성들을 차례로 재고 구성마다 `ROW|{json}` 한 줄을 낸다.
fn child_measure(mut a: Args) -> Result<(), String> {
    let configs = a.list("--configs", &CONFIGS.map(|c| c.0))?.ok_or("--configs 필요")?;
    let warmup = Duration::from_secs_f64(a.num("--warmup-s")?.unwrap_or(3.0));
    let window = Duration::from_secs_f64(a.num("--window-s")?.unwrap_or(12.0));
    a.done()?;
    for name in configs {
        let (_, codec, frame_ms, force) = CONFIGS.into_iter().find(|c| c.0 == name).ok_or("구성")?;
        let r = mesh_cpu::run(codec, frame_ms, force, warmup, window);
        let policy: Value = serde_json::from_str(&r.policy_applied).unwrap_or(Value::Null);
        let row = json!({
            "config": name,
            "workerPct": r.worker_avg,
            "workerPctMax": r.worker_max,
            "bodyP50Us": r.body[0],
            "bodyP99Us": r.body[1],
            "bodyMaxUs": r.body[2],
            "perNodePct": r.per_node_pct,
            "concealMaxPct": r.conceal_max,
            "xruns": r.xruns,
            "placement": r.placement,
            "threadPolicy": policy,
        });
        println!("ROW|{row}");
        std::io::stdout().flush().ok();
    }
    Ok(())
}

// ───────────────────────── 자식: 부하 생성기 ─────────────────────────

/// 바쁜 스레드 하나의 표본: 코어 종류별 [P, E, LP-E] 횟수.
#[derive(Default)]
struct KindCounts([AtomicU64; 3]);

impl KindCounts {
    fn sample(&self) {
        if let Some(k) = current_core_kind() {
            self.0[k].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get(&self) -> [u64; 3] {
        [0, 1, 2].map(|i| self.0[i].load(Ordering::Relaxed))
    }
}

fn shares(c: [u64; 3]) -> [f64; 3] {
    let n: u64 = c.iter().sum();
    c.map(|x| if n > 0 { x as f64 * 100.0 / n as f64 } else { 0.0 })
}

/// 표준 입력이 닫히거나 한 줄이 오면 `stop`을 켠다(부모가 끝내라고 알리는 길). `max` 뒤에도 켠다(부모가 죽었을 때).
fn stop_on_stdin_or_timeout(stop: Arc<AtomicBool>, max: Duration) {
    let s = stop.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        s.store(true, Ordering::Relaxed);
    });
    std::thread::spawn(move || {
        std::thread::sleep(max);
        stop.store(true, Ordering::Relaxed);
    });
}

/// 경합 부하: 바쁜 스레드 `--threads`개. 스레드마다 `--policy`(기본 `pin` = P 코어 CPU set)를 걸고 `--prio`로 올린 뒤, 주기
/// `--period-ms` 중 `--duty`만큼 돌고 나머지는 잔다. 끝날 때 `HOG|{json}`(실제로 쓴 코어 수·머문 코어 종류·우선순위 결과).
fn child_hog(mut a: Args) -> Result<(), String> {
    let threads: usize = a.num("--threads")?.ok_or("--threads 필요")?;
    let duty: f64 = a.num("--duty")?.unwrap_or(1.0);
    let period = Duration::from_secs_f64(a.num::<f64>("--period-ms")?.unwrap_or(10.0) / 1000.0);
    let prio = a.value("--prio")?.unwrap_or_else(|| "mmcss".into());
    let policy = a.value("--policy")?.unwrap_or_else(|| "pin".into());
    let max = Duration::from_secs_f64(a.num("--max-s")?.unwrap_or(600.0));
    a.done()?;
    let stop = Arc::new(AtomicBool::new(false));
    stop_on_stdin_or_timeout(stop.clone(), max);
    let counts = Arc::new(KindCounts::default());
    let mmcss_ok = Arc::new(AtomicU64::new(0));
    let applied = Arc::new(std::sync::Mutex::new(String::new()));
    let (c0, k0, t0) = (process_cycles(), tsc_now(), Instant::now());
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let (stop, counts, mmcss_ok, applied, prio, policy) = (stop.clone(), counts.clone(), mmcss_ok.clone(), applied.clone(), prio.clone(), policy.clone());
            std::thread::Builder::new()
                .name(format!("hog-{i}"))
                .spawn(move || {
                    let res = apply_policy_to_current_thread(&policy).unwrap_or_else(|e| e);
                    *applied.lock().unwrap() = res;
                    let _guard = (prio == "mmcss").then(|| {
                        let g = promote_current_thread();
                        if g.is_mmcss() {
                            mmcss_ok.fetch_add(1, Ordering::Relaxed);
                        }
                        g
                    });
                    // 스레드마다 주기 시작을 어긋나게(모두 같은 순간에 자지 않게)
                    std::thread::sleep(period.mul_f64(i as f64 / threads.max(1) as f64));
                    let busy = period.mul_f64(duty);
                    while !stop.load(Ordering::Relaxed) {
                        let t = Instant::now();
                        while t.elapsed() < busy {
                            for _ in 0..2_000 {
                                std::hint::spin_loop();
                            }
                            counts.sample();
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                        }
                        if let Some(rest) = period.checked_sub(t.elapsed()) {
                            std::thread::sleep(rest);
                        }
                    }
                })
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    for h in handles {
        let _ = h.join();
    }
    let cores = cycle_pct(process_cycles(), c0, tsc_now(), k0).map(|p| p as f64 / 100.0);
    let s = shares(counts.get());
    let out = json!({
        "threads": threads, "duty": duty, "periodMs": period.as_secs_f64() * 1000.0, "prio": prio, "policy": policy,
        "seconds": t0.elapsed().as_secs_f64(), "cpuCores": cores, "pPct": s[0], "ePct": s[1], "lpEPct": s[2],
        "mmcssThreads": mmcss_ok.load(Ordering::Relaxed), "applied": serde_json::from_str::<Value>(&applied.lock().unwrap()).unwrap_or(Value::Null),
    });
    println!("HOG|{out}");
    Ok(())
}

// ───────────────────────── 자식: soft 확인 프로브 ─────────────────────────

/// 보통 우선순위 바쁜 스레드 `--threads`개에 `--policy`를 걸고 `--seconds` 동안 돌린 뒤 `PROBE|{json}`: P/E/LP-E에 머문 비율과
/// 프로세스가 쓴 코어 수. 스레드 수가 P 코어 수보다 많을 때 고정이면 코어 ≈ P 수(P 100%), soft면 코어 ≈ 스레드 수(E로 넘침).
fn child_probe(mut a: Args) -> Result<(), String> {
    let policy = a.value("--policy")?.ok_or("--policy 필요")?;
    let threads: usize = a.num("--threads")?.ok_or("--threads 필요")?;
    let seconds: f64 = a.num("--seconds")?.unwrap_or(3.0);
    a.done()?;
    let stop = Arc::new(AtomicBool::new(false));
    let counts = Arc::new(KindCounts::default());
    let applied = Arc::new(std::sync::Mutex::new(String::new()));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let (stop, counts, applied, policy) = (stop.clone(), counts.clone(), applied.clone(), policy.clone());
            std::thread::spawn(move || {
                let res = apply_policy_to_current_thread(&policy).unwrap_or_else(|e| e);
                *applied.lock().unwrap() = res;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..2_000 {
                        std::hint::spin_loop();
                    }
                    counts.sample();
                }
            })
        })
        .collect();
    // 스레드가 정책을 걸고 자리를 잡을 시간을 준 뒤 창을 연다
    std::thread::sleep(Duration::from_millis(300));
    let n0 = counts.get();
    let (c0, k0) = (process_cycles(), tsc_now());
    std::thread::sleep(Duration::from_secs_f64(seconds));
    let cores = cycle_pct(process_cycles(), c0, tsc_now(), k0).map(|p| p as f64 / 100.0);
    let n1 = counts.get();
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    let s = shares([0, 1, 2].map(|i| n1[i] - n0[i]));
    let out = json!({
        "policy": policy, "threads": threads, "seconds": seconds, "cpuCores": cores, "pPct": s[0], "ePct": s[1], "lpEPct": s[2],
        "applied": serde_json::from_str::<Value>(&applied.lock().unwrap()).unwrap_or(Value::Null),
    });
    println!("PROBE|{out}");
    Ok(())
}

// ───────────────────────── 부모: 교차 실행 ─────────────────────────

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("실행파일 경로: {e}"))
}

/// 자식 표준 출력에서 `tag|` 줄의 JSON을 모은다.
fn tagged(stdout: &[u8], tag: &str) -> Vec<Value> {
    String::from_utf8_lossy(stdout).lines().filter_map(|l| l.strip_prefix(tag)).filter_map(|j| serde_json::from_str(j).ok()).collect()
}

/// 돌고 있는 부하 프로세스. 놓이면(패닉 포함) 끝낸다.
struct Hog(Option<Child>);

impl Hog {
    fn start(o: &Opts, threads: usize, max_s: f64) -> Result<Self, String> {
        let child = Command::new(exe()?)
            .args(["--child-hog", "--threads", &threads.to_string(), "--duty", &o.hog_duty.to_string(), "--prio", &o.hog_prio, "--max-s", &max_s.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("부하 프로세스 시작 실패: {e}"))?;
        // 스레드가 올라와 P 코어를 채울 시간
        std::thread::sleep(Duration::from_secs(1));
        Ok(Self(Some(child)))
    }

    /// 끝내라고 알리고 보고(`HOG|`)를 받는다.
    fn finish(mut self) -> Value {
        let Some(mut c) = self.0.take() else { return Value::Null };
        if let Some(mut stdin) = c.stdin.take() {
            let _ = stdin.write_all(b"\n");
        }
        match c.wait_with_output() {
            Ok(out) => tagged(&out.stdout, "HOG|").into_iter().next().unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }
}

impl Drop for Hog {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 전원 상태 한 낱말(라운드마다 기록 — 2단계 측정은 도중에 배터리 → AC로 바뀌어 한 라운드를 버렸다). Windows만, 못 읽으면 unknown.
fn power_state() -> String {
    if !cfg!(windows) {
        return "unknown".into();
    }
    // BatteryStatus 2 = AC 연결. 배터리가 없는 PC는 출력이 비어 AC로 본다.
    let out = Command::new("powershell").args(["-NoProfile", "-Command", "(Get-CimInstance Win32_Battery | Select-Object -First 1).BatteryStatus"]).output();
    match out {
        Ok(o) if o.status.success() => match String::from_utf8_lossy(&o.stdout).trim() {
            "" | "2" | "6" | "7" | "8" | "9" => "ac".into(),
            "1" | "4" | "5" => "battery".into(),
            other => format!("status{other}"),
        },
        _ => "unknown".into(),
    }
}

fn utc_stamp() -> String {
    // 의존 없이 UTC 연-월-일 시:분:초(그레고리력 — days_from_civil의 역).
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn fmt_hm(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 {
        format!("{}시간 {}분", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}분 {}초", s / 60, s % 60)
    } else {
        format!("{s}초")
    }
}

fn orchestrate(o: Opts) -> Result<(), String> {
    let rounds = planned_rounds(&o);
    let p_count = p_core_count();
    let hog_threads = o.hog_threads.unwrap_or(p_count.max(1));
    let probe_s = if o.probe { 3.0 * 4.5 } else { 0.0 };
    let total = round_seconds(&o) * rounds as f64 + probe_s;
    let cores = describe_cores();
    println!("코어 구성: {cores}");
    println!(
        "계획: {rounds}라운드 × 환경 {} × 정책 {} × 구성 {} (예열 {}s + 창 {}s, 실행 사이 {}s) — 예상 {}",
        o.envs.join("/"),
        o.policies.join("/"),
        o.configs.len(),
        o.warmup_s,
        o.window_s,
        o.cooldown_s,
        fmt_hm(total)
    );
    if o.envs.iter().any(|e| e == "contended") {
        println!("경합 부하: 바쁜 스레드 {hog_threads}개(P 코어 CPU set, 우선순위 {}, 바쁜 비율 {})", o.hog_prio, o.hog_duty);
        if p_count == 0 {
            println!("주의: 이 기계는 P 코어 정책이 skipped다(하이브리드 아님·물리 P 코어 4개 미만) — 부하는 묶이지 않고 정책 차이도 나지 않는다");
        }
    }
    if o.dry_run {
        return Ok(());
    }
    let out_dir = o.out.clone().unwrap_or_else(|| PathBuf::from(format!("policy-matrix-{}", utc_stamp())));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut runs_file = std::fs::File::create(out_dir.join("runs.jsonl")).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let started_at = utc_stamp();

    // 0. 선호 힌트가 soft한지: 바쁜 스레드를 P 코어 수의 2배만큼(최소 4)
    let mut probes = Vec::new();
    if o.probe {
        let threads = (p_count * 2).max(4);
        for policy in ["off", "prefer", "pin"] {
            let out = Command::new(exe()?)
                .args(["--child-probe", "--policy", policy, "--threads", &threads.to_string(), "--seconds", "3"])
                .stderr(Stdio::null())
                .output()
                .map_err(|e| format!("프로브 시작 실패: {e}"))?;
            match tagged(&out.stdout, "PROBE|").into_iter().next() {
                Some(v) => {
                    println!("프로브 {policy}: 스레드 {threads} → 쓴 코어 {:.2}, P {:.1}% / E {:.1}% / LP-E {:.1}%", num(&v, "cpuCores"), num(&v, "pPct"), num(&v, "ePct"), num(&v, "lpEPct"));
                    probes.push(v);
                }
                None => println!("프로브 {policy}: 결과 없음"),
            }
        }
    }

    // 1. 교차 실행
    let mut runs: Vec<Value> = Vec::new();
    let per_run = run_seconds(&o);
    for round in 0..rounds {
        let power = power_state();
        for (env, policy) in round_order(&o, round) {
            let band_policy = POLICIES.iter().find(|p| p.0 == policy).map(|p| p.1).ok_or("정책")?;
            let hog = if env == "contended" { Some(Hog::start(&o, hog_threads, per_run + 60.0)?) } else { None };
            let t = Instant::now();
            let out = Command::new(exe()?)
                .args(["--child-measure", "--configs", &o.configs.join(","), "--warmup-s", &o.warmup_s.to_string(), "--window-s", &o.window_s.to_string()])
                .env("BAND_THREAD_POLICY", band_policy)
                .output()
                .map_err(|e| format!("측정 프로세스 시작 실패: {e}"))?;
            let hog_report = hog.map(Hog::finish).unwrap_or(Value::Null);
            let rows = tagged(&out.stdout, "ROW|");
            let ok = out.status.success() && rows.len() == o.configs.len();
            let stderr_tail: String = String::from_utf8_lossy(&out.stderr).lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            let run = json!({
                "round": round, "env": env, "policy": policy, "bandThreadPolicy": band_policy, "power": power, "ok": ok,
                "seconds": t.elapsed().as_secs_f64(), "rows": rows, "hog": hog_report,
                "error": if ok { Value::Null } else { Value::String(stderr_tail) },
            });
            writeln!(runs_file, "{run}").and_then(|_| runs_file.flush()).map_err(|e| e.to_string())?;
            println!(
                "라운드 {}/{rounds} {env:9} {policy:8} {} ({:.0}s, 경과 {} / 예상 {})",
                round + 1,
                if ok { "완료".to_string() } else { format!("실패 — 구성 {}/{}개만", run["rows"].as_array().map_or(0, |r| r.len()), o.configs.len()) },
                t.elapsed().as_secs_f64(),
                fmt_hm(started.elapsed().as_secs_f64()),
                fmt_hm(total)
            );
            runs.push(run);
            std::thread::sleep(Duration::from_secs_f64(o.cooldown_s));
        }
    }

    // 2. 요약
    let summary = summarize(&o, &runs);
    let meta = json!({
        "startedAtUtc": started_at, "elapsedSeconds": started.elapsed().as_secs_f64(), "cores": cores, "pLogicalProcessors": p_count,
        "rounds": rounds, "windowS": o.window_s, "warmupS": o.warmup_s, "cooldownS": o.cooldown_s,
        "configs": o.configs, "policies": o.policies, "envs": o.envs,
        "hog": { "threads": hog_threads, "duty": o.hog_duty, "prio": o.hog_prio, "policy": "pin" },
        "policyEnv": POLICIES.iter().map(|p| (p.0.to_string(), Value::String(p.1.into()))).collect::<serde_json::Map<_, _>>(),
    });
    let md = render_md(&o, &meta, &probes, &runs, &summary);
    let result = json!({ "meta": meta, "probes": probes, "summary": summary, "runs": runs });
    std::fs::write(out_dir.join("results.json"), serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::write(out_dir.join("results.md"), &md).map_err(|e| e.to_string())?;
    println!("\n{md}");
    println!("결과: {}", out_dir.join("results.md").display());
    Ok(())
}

// ───────────────────────── 요약·표 ─────────────────────────

fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// 실행 한 줄(`ROW`)에서 지표 값을 꺼낸다. 없는 값은 None(표본에서 뺀다).
fn metric(row: &Value, name: &str) -> Option<f64> {
    let share = |thread: usize, kind: usize| -> Option<f64> {
        let t = row.get("placement")?.get(thread)?.as_array()?;
        let v: Vec<f64> = t.iter().filter_map(Value::as_f64).collect();
        let n: f64 = v.iter().sum();
        (v.len() == 3 && n > 0.0).then(|| (v[kind] * 100.0 / n).max(0.0))
    };
    let adaptive = |key: &str| row.get("threadPolicy")?.get("adaptive")?.get(key).cloned();
    match name {
        "workerPPct" => share(0, 0),
        "workerEPct" => share(0, 1),
        "workerLpEPct" => share(0, 2),
        "recvPPct" => share(1, 0),
        "releases" => adaptive("releases")?.as_f64(),
        "rebinds" => adaptive("rebinds")?.as_f64(),
        "endedReleased" => adaptive("released")?.as_bool().map(|b| if b { 100.0 } else { 0.0 }),
        "pBusyPct" => adaptive("pBusyPct")?.as_f64(),
        _ => row.get(name).and_then(Value::as_f64).filter(|x| x.is_finite()),
    }
}

/// 표본 통계: n, 평균, 표준편차(n − 1), 중앙값. 빈 표본은 n = 0.
fn stats(xs: &[f64]) -> Value {
    if xs.is_empty() {
        return json!({ "n": 0 });
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let sd = if xs.len() > 1 { (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt() } else { 0.0 };
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let median = if s.len() % 2 == 1 { s[s.len() / 2] } else { (s[s.len() / 2 - 1] + s[s.len() / 2]) / 2.0 };
    json!({ "n": xs.len(), "mean": mean, "sd": sd, "median": median })
}

const METRICS: [&str; 14] = [
    "workerPct", "bodyP50Us", "bodyP99Us", "bodyMaxUs", "xruns", "concealMaxPct", "workerPPct", "workerEPct", "workerLpEPct", "recvPPct", "releases", "rebinds", "endedReleased", "pBusyPct",
];

/// 환경 → 구성 → 정책 → 지표 → 통계(+ xrun 원값, 실패한 실행 수).
fn summarize(o: &Opts, runs: &[Value]) -> Value {
    let mut out = serde_json::Map::new();
    for env in &o.envs {
        let mut by_config = serde_json::Map::new();
        for config in &o.configs {
            let mut by_policy = serde_json::Map::new();
            for policy in &o.policies {
                let mine: Vec<&Value> = runs.iter().filter(|r| r["env"] == **env && r["policy"] == **policy).collect();
                let rows: Vec<&Value> = mine.iter().filter_map(|r| r["rows"].as_array()?.iter().find(|row| row["config"] == **config)).collect();
                let mut cell: BTreeMap<String, Value> = METRICS.iter().map(|m| (m.to_string(), stats(&rows.iter().filter_map(|r| metric(r, m)).collect::<Vec<_>>()))).collect();
                let xr: Vec<u64> = rows.iter().filter_map(|r| r["xruns"].as_u64()).collect();
                cell.insert("xrunRuns".into(), json!(xr.iter().filter(|x| **x > 0).count()));
                cell.insert("xrunValues".into(), json!(xr));
                cell.insert("missing".into(), json!(mine.len() - rows.len()));
                by_policy.insert(policy.clone(), json!(cell));
            }
            by_config.insert(config.clone(), Value::Object(by_policy));
        }
        out.insert(env.clone(), Value::Object(by_config));
    }
    Value::Object(out)
}

fn cell_text(cell: &Value, metric: &str, digits: usize) -> String {
    let s = &cell[metric];
    match s["n"].as_u64() {
        None | Some(0) => "—".into(),
        Some(1) => format!("{:.digits$}", num(s, "mean")),
        Some(_) => format!("{:.digits$} ± {:.digits$} ({:.digits$})", num(s, "mean"), num(s, "sd"), num(s, "median")),
    }
}

fn render_md(o: &Opts, meta: &Value, probes: &[Value], runs: &[Value], summary: &Value) -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let label = |list: &[(&str, &str)], k: &str| list.iter().find(|x| x.0 == k).map_or(k.to_string(), |x| x.1.to_string());
    let policy_label = |k: &str| POLICIES.iter().find(|x| x.0 == k).map_or(k.to_string(), |x| x.2.to_string());
    let rounds = meta["rounds"].as_u64().unwrap_or(0);
    let _ = writeln!(md, "# 스레드 정책 × 환경 교차 측정 (policy_matrix)\n");
    let _ = writeln!(md, "- 시작(UTC) {}, 소요 {}", meta["startedAtUtc"].as_str().unwrap_or(""), fmt_hm(num(meta, "elapsedSeconds")));
    let _ = writeln!(md, "- 기계: {}", meta["cores"].as_str().unwrap_or(""));
    let _ = writeln!(md, "- {rounds}라운드(짝수 라운드 정순, 홀수 라운드 역순), 구성마다 예열 {}s + 측정 창 {}s, 실행 사이 {}s", o.warmup_s, o.window_s, o.cooldown_s);
    let _ = writeln!(md, "- 정책(모두 HighQoS 켬): {}", o.policies.iter().map(|p| format!("{} = `{}`", policy_label(p), POLICIES.iter().find(|x| x.0 == p).map_or("", |x| x.1))).collect::<Vec<_>>().join(", "));
    let _ = writeln!(
        md,
        "- 경합 환경: 다른 프로세스의 바쁜 스레드 {}개(P 코어 CPU set에 묶음, 우선순위 {}, 바쁜 비율 {})",
        meta["hog"]["threads"], o.hog_prio, o.hog_duty
    );
    let powers: std::collections::BTreeSet<&str> = runs.iter().filter_map(|r| r["power"].as_str()).collect();
    let _ = writeln!(md, "- 전원(라운드마다 기록): {}", powers.into_iter().collect::<Vec<_>>().join(", "));
    let failed = runs.iter().filter(|r| r["ok"] == false).count();
    if failed > 0 {
        let _ = writeln!(md, "- **실패한 실행 {failed}개**(측정 프로세스가 끝까지 돌지 못함 — 아래 5절). 그 실행의 빠진 구성은 표본에서 빠졌다");
    }
    if rounds < 5 {
        let _ = writeln!(md, "- **표본이 적다(라운드 {rounds})** — 도구가 도는지 보는 용도다. 차이를 말할 수 없다");
    }
    let _ = writeln!(md, "\n값은 평균 ± 표준편차 (중앙값), 표본 하나면 값 하나. 6엔진 평균(xrun은 6엔진 합, 최대 은닉은 6엔진 최대).\n");

    if !probes.is_empty() {
        let _ = writeln!(md, "## 0. 선호 힌트가 soft한지 — 보통 우선순위 바쁜 스레드 {}개\n", probes[0]["threads"]);
        let _ = writeln!(md, "| 스레드에 건 정책 | 프로세스가 쓴 코어 수 | P (%) | E (%) | LP-E (%) | 적용 결과 |");
        let _ = writeln!(md, "|---|---|---|---|---|---|");
        for p in probes {
            let _ = writeln!(md, "| {} | {:.2} | {:.1} | {:.1} | {:.1} | `{}` |", p["policy"].as_str().unwrap_or(""), num(p, "cpuCores"), num(p, "pPct"), num(p, "ePct"), num(p, "lpEPct"), p["applied"]);
        }
        let _ = writeln!(md, "\n고정(`pin`)이면 P 100%에 쓴 코어 수 ≈ P 코어 수, soft면 스레드가 E로 넘쳐 쓴 코어 수 ≈ 스레드 수다.\n");
    }

    let tables: [(&str, &str, usize); 8] = [
        ("workerPct", "워커 CPU (%, 1코어)", 2),
        ("bodyP50Us", "워커 반복 본문 `iter_body` p50 (µs)", 1),
        ("bodyP99Us", "워커 반복 본문 `iter_body` p99 (µs)", 0),
        ("xruns", "xrun (측정 창, 6엔진 합)", 1),
        ("concealMaxPct", "최대 은닉률 (%)", 2),
        ("workerPPct", "코어 배치 — 워커가 P 코어에서 깨어난 비율 (%)", 1),
        ("workerLpEPct", "코어 배치 — 워커가 LP-E 코어에서 깨어난 비율 (%)", 1),
        ("recvPPct", "코어 배치 — 수신 스레드가 P 코어에서 깨어난 비율 (%)", 1),
    ];
    for (i, env) in o.envs.iter().enumerate() {
        let _ = writeln!(md, "## {}. {}\n", i + 1, label(&ENVS, env));
        for (m, title, digits) in tables {
            let _ = writeln!(md, "### {title}\n");
            let _ = writeln!(md, "| 구성 | {} |", o.policies.iter().map(|p| policy_label(p)).collect::<Vec<_>>().join(" | "));
            let _ = writeln!(md, "|---|{}", "---|".repeat(o.policies.len()));
            for config in &o.configs {
                let cells: Vec<String> = o.policies.iter().map(|p| cell_text(&summary[env][config][p], m, digits)).collect();
                let _ = writeln!(md, "| {config} | {} |", cells.join(" | "));
            }
            let _ = writeln!(md);
        }
        if o.policies.iter().any(|p| p == "adaptive") {
            let _ = writeln!(md, "### 조건부 고정(3안) — 측정 창이 끝났을 때의 상태(엔진 0)\n");
            let _ = writeln!(md, "| 구성 | 푼 횟수 | 다시 묶은 횟수 | 풀린 채 끝난 실행 (%) | 마지막 창의 P 코어 바쁨 (%) |");
            let _ = writeln!(md, "|---|---|---|---|---|");
            for config in &o.configs {
                let c = &summary[env][config]["adaptive"];
                let _ = writeln!(md, "| {config} | {} | {} | {} | {} |", cell_text(c, "releases", 1), cell_text(c, "rebinds", 1), cell_text(c, "endedReleased", 0), cell_text(c, "pBusyPct", 0));
            }
            let _ = writeln!(md, "\n횟수는 예열을 포함한 엔진 시작부터의 누적이다.\n");
        }
    }

    let n = o.envs.len();
    let _ = writeln!(md, "## {}. xrun 원값 — 실행별(라운드 순서)\n", n + 1);
    let _ = writeln!(md, "평균은 실행 한두 개가 끌어올리므로 원값과 \"xrun이 난 실행 수\"를 함께 본다.\n");
    let _ = writeln!(md, "| 환경 | 구성 | {} |", o.policies.iter().map(|p| policy_label(p)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(md, "|---|---|{}", "---|".repeat(o.policies.len()));
    for env in &o.envs {
        for config in &o.configs {
            let cells: Vec<String> = o
                .policies
                .iter()
                .map(|p| {
                    let c = &summary[env][config][p];
                    let vals: Vec<String> = c["xrunValues"].as_array().map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default();
                    format!("{}/{} ({})", c["xrunRuns"], vals.len(), vals.join(" "))
                })
                .collect();
            let _ = writeln!(md, "| {} | {config} | {} |", label(&ENVS, env), cells.join(" | "));
        }
    }

    let hogs: Vec<&Value> = runs.iter().map(|r| &r["hog"]).filter(|h| !h.is_null()).collect();
    if !hogs.is_empty() {
        let col = |k: &str| stats(&hogs.iter().filter_map(|h| h[k].as_f64()).collect::<Vec<_>>());
        let line = |k: &str, d: usize| cell_text(&json!({ "x": col(k) }), "x", d);
        let _ = writeln!(md, "\n## {}. 경합 부하가 실제로 건 부하(부하 프로세스 보고, 실행 {}개)\n", n + 2, hogs.len());
        let _ = writeln!(md, "| 쓴 코어 수 | P에 머문 비율 (%) | E (%) | MMCSS 등록된 스레드 수 |");
        let _ = writeln!(md, "|---|---|---|---|");
        let _ = writeln!(md, "| {} | {} | {} | {} |", line("cpuCores", 2), line("pPct", 1), line("ePct", 1), line("mmcssThreads", 1));
        let _ = writeln!(md, "\n쓴 코어 수가 스레드 수 × 바쁜 비율보다 뚜렷이 낮으면 부하가 P 코어에서 엔진 스레드나 다른 일과 다투다 밀린 것이다.");
    }

    if failed > 0 {
        let _ = writeln!(md, "\n## {}. 실패한 실행\n", n + 3);
        for r in runs.iter().filter(|r| r["ok"] == false) {
            let _ = writeln!(
                md,
                "- 라운드 {} {} {}: 구성 {}개 완료. 마지막 오류 출력:\n\n```\n{}\n```",
                r["round"].as_u64().unwrap_or(0) + 1,
                r["env"].as_str().unwrap_or(""),
                r["policy"].as_str().unwrap_or(""),
                r["rows"].as_array().map_or(0, |a| a.len()),
                r["error"].as_str().unwrap_or("")
            );
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Args {
        Args(s.split_whitespace().map(String::from).collect())
    }

    #[test]
    fn parses_options_and_rejects_unknown() {
        let o = parse_opts(args("--rounds 2 --window-s 5 --warmup-s 2 --configs opus2.5,pcm5 --policies off,adaptive --envs contended --hog-threads 8 --hog-duty 0.75 --hog-prio normal --no-probe --out x --cooldown-s 1")).unwrap();
        assert_eq!((o.rounds, o.window_s, o.warmup_s, o.cooldown_s), (2, 5.0, 2.0, 1.0));
        assert_eq!((o.configs, o.policies, o.envs), (vec!["opus2.5".to_string(), "pcm5".into()], vec!["off".to_string(), "adaptive".into()], vec!["contended".to_string()]));
        assert_eq!((o.hog_threads, o.hog_duty, o.hog_prio.as_str(), o.probe, o.out), (Some(8), 0.75, "normal", false, Some(PathBuf::from("x"))));
        let d = parse_opts(args("")).unwrap();
        assert_eq!((d.rounds, d.configs.len(), d.policies.len(), d.envs.len(), d.probe), (10, 6, 4, 2, true));
        for bad in ["--rounds 0", "--configs opus3", "--policies soft", "--envs loud", "--hog-duty 0", "--hog-duty 1.5", "--hog-prio high", "--window-s 0", "--rounds", "--frobnicate 1", "--minutes 0"] {
            assert!(parse_opts(args(bad)).is_err(), "{bad}");
        }
    }

    /// 총 소요 시간: 라운드 하나 = 환경 × 정책 × (구성 × (예열 + 창 + 구성 준비) + 실행 준비 + 쉬는 시간). `--minutes`는 그 시간에
    /// 들어가는 라운드 수(최소 1).
    #[test]
    fn time_budget_sets_rounds() {
        let o = parse_opts(args("--window-s 30")).unwrap();
        // 6 × (3 + 30 + 0.5) + 2 + 5 = 208초, × 8 = 1664초 ≈ 27.7분
        assert!((run_seconds(&o) - 208.0).abs() < 1e-9);
        assert!((round_seconds(&o) - 1664.0).abs() < 1e-9);
        assert_eq!(planned_rounds(&o), 10);
        assert_eq!(planned_rounds(&Opts { minutes: Some(90.0), ..o.clone() }), 3);
        // 기본값(창 20초): 6 × 23.5 + 7 = 148초, × 8 = 1184초 → 10라운드 ≈ 3시간 17분, 90분이면 4라운드
        let d = Opts::default();
        assert!((round_seconds(&d) - 1184.0).abs() < 1e-9);
        assert_eq!(planned_rounds(&Opts { minutes: Some(90.0), ..d }), 4);
        assert_eq!(planned_rounds(&Opts { minutes: Some(5.0), ..o.clone() }), 1, "한 라운드보다 짧아도 1라운드는 돈다");
        assert_eq!(planned_rounds(&Opts { minutes: Some(300.0), ..o }), 10);
    }

    /// 순서 교차: 짝수 라운드는 환경·정책 정순, 홀수 라운드는 통째로 역순. 라운드마다 모든 조합이 한 번씩.
    #[test]
    fn rounds_alternate_order() {
        let o = Opts::default();
        let r0 = round_order(&o, 0);
        let r1 = round_order(&o, 1);
        assert_eq!(r0.len(), 8);
        assert_eq!(r0[0], ("quiet".to_string(), "off".to_string()));
        assert_eq!(r0[7], ("contended".to_string(), "adaptive".to_string()));
        assert_eq!(r1, r0.iter().rev().cloned().collect::<Vec<_>>());
        assert_eq!(round_order(&o, 2), r0);
    }

    fn row(config: &str, worker: f64, xruns: u64, p: f64, adaptive: Option<(u64, bool)>) -> Value {
        json!({
            "config": config, "workerPct": worker, "bodyP50Us": 50.0, "bodyP99Us": 400.0, "bodyMaxUs": 900.0, "perNodePct": 20.0, "concealMaxPct": 0.0, "xruns": xruns,
            "placement": [[p, 100.0 - p, 0.0], [p, 100.0 - p, 0.0]],
            "threadPolicy": { "adaptive": adaptive.map(|(releases, released)| json!({ "released": released, "releases": releases, "rebinds": 0, "pBusyPct": 95 })) },
        })
    }

    #[test]
    fn summary_and_markdown_cover_every_cell() {
        let o = parse_opts(args("--configs opus2.5,pcm5 --policies off,adaptive")).unwrap();
        let run = |round: u64, env: &str, policy: &str, rows: Vec<Value>, ok: bool| json!({ "round": round, "env": env, "policy": policy, "power": "ac", "ok": ok, "rows": rows, "hog": if env == "contended" { json!({ "cpuCores": 3.9, "pPct": 100.0, "ePct": 0.0, "mmcssThreads": 4 }) } else { Value::Null }, "error": if ok { Value::Null } else { json!("panicked: 풀메시 연결 실패") } });
        let runs = vec![
            run(0, "quiet", "off", vec![row("opus2.5", 12.0, 0, 10.0, None), row("pcm5", 5.0, 1, 20.0, None)], true),
            run(1, "quiet", "off", vec![row("opus2.5", 14.0, 3, 30.0, None), row("pcm5", 6.0, 0, 40.0, None)], true),
            run(0, "quiet", "adaptive", vec![row("opus2.5", 10.0, 0, 100.0, Some((0, false))), row("pcm5", 4.0, 0, 100.0, Some((0, false)))], true),
            run(0, "contended", "off", vec![row("opus2.5", 13.0, 2, 5.0, None), row("pcm5", 5.5, 0, 5.0, None)], true),
            // 실패한 실행: 첫 구성만 끝났다
            run(0, "contended", "adaptive", vec![row("opus2.5", 11.0, 9, 60.0, Some((1, true)))], false),
        ];
        let s = summarize(&o, &runs);
        let cell = &s["quiet"]["opus2.5"]["off"];
        assert_eq!(cell["workerPct"]["n"], 2);
        assert!((num(&cell["workerPct"], "mean") - 13.0).abs() < 1e-9);
        assert!((num(&cell["workerPct"], "sd") - 2f64.sqrt()).abs() < 1e-9);
        assert!((num(&cell["workerPPct"], "median") - 20.0).abs() < 1e-9);
        assert_eq!((cell["xrunRuns"].clone(), cell["xrunValues"].clone(), cell["missing"].clone()), (json!(1), json!([0, 3]), json!(0)));
        assert_eq!(cell["releases"]["n"], 0, "조건부 고정이 아닌 정책은 상태가 없다");
        let ad = &s["contended"]["opus2.5"]["adaptive"];
        assert_eq!((ad["releases"]["mean"].as_f64(), ad["endedReleased"]["mean"].as_f64(), ad["pBusyPct"]["mean"].as_f64()), (Some(1.0), Some(100.0), Some(95.0)));
        assert_eq!(s["contended"]["pcm5"]["adaptive"]["missing"], 1, "실패한 실행의 빠진 구성");
        assert_eq!(s["contended"]["pcm5"]["adaptive"]["workerPct"]["n"], 0);

        let meta = json!({ "startedAtUtc": "20261124-000000", "elapsedSeconds": 125.0, "cores": "논리 14", "rounds": 2, "hog": { "threads": 4 } });
        let probes = vec![json!({ "policy": "prefer", "threads": 8, "cpuCores": 7.6, "pPct": 50.0, "ePct": 50.0, "lpEPct": 0.0, "applied": { "highQos": "off", "pCores": "applied" } })];
        let md = render_md(&o, &meta, &probes, &runs, &s);
        for want in [
            "## 0. 선호 힌트가 soft한지",
            "| prefer | 7.60 | 50.0 | 50.0 | 0.0 |",
            "## 1. 조용함",
            "## 2. 경합",
            "| 구성 | 끔 | 조건부 고정(3안) |",
            "| opus2.5 | 13.00 ± 1.41 (13.00) | 10.00 |",
            "| pcm5 | 5.50 | — |",
            "### 조건부 고정(3안) — 측정 창이 끝났을 때의 상태",
            "## 3. xrun 원값",
            "| 조용함 | opus2.5 | 1/2 (0 3) | 0/1 (0) |",
            "## 4. 경합 부하가 실제로 건 부하",
            "## 5. 실패한 실행",
            "풀메시 연결 실패",
            "**표본이 적다(라운드 2)**",
            "**실패한 실행 1개**",
        ] {
            assert!(md.contains(want), "없음: {want}\n{md}");
        }
    }

    #[test]
    fn stats_and_helpers() {
        assert_eq!(stats(&[]), json!({ "n": 0 }));
        let s = stats(&[1.0, 2.0, 3.0, 10.0]);
        assert_eq!((s["n"].as_u64(), s["mean"].as_f64(), s["median"].as_f64()), (Some(4), Some(4.0), Some(2.5)));
        assert_eq!(shares([1, 1, 2]), [25.0, 25.0, 50.0]);
        assert_eq!(shares([0, 0, 0]), [0.0, 0.0, 0.0]);
        assert_eq!((fmt_hm(59.4), fmt_hm(125.0), fmt_hm(15_000.0)), ("59초".to_string(), "2분 5초".to_string(), "4시간 10분".to_string()));
        assert_eq!(tagged(b"x\nROW|{\"a\":1}\nROW|bad\nHOG|{}\n", "ROW|"), vec![json!({ "a": 1 })]);
        let stamp = utc_stamp();
        assert!(stamp.len() == 15 && stamp.starts_with("20"), "{stamp}");
    }
}
