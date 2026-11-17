//! 모듈 의존 방향 검사(`docs/overhaul/engine-architecture.md` 5절).
//!
//! `src/**/*.rs`의 `use`·경로(`crate::…`, `super::…`, `self::…`, 자식 모듈 이름으로 시작하는 상대 경로)를 읽어
//! 모듈 노드 사이 간선을 만들고 규칙을 본다.
//!
//! 크레이트 셋(엔진 R-1 ⑩-1·⑩-2, `engine-stage5-10-scope.md` 7.2절), 파서 하나:
//! - 이 크레이트 `band-engine`(`crates/band-engine/src`, 노드 표 `NODES` — 이름에 접두가 없다, 뿌리 `lib.rs`는 `band_engine`),
//! - 결정적 코어 `band-dsp`(`crates/band-dsp/src`, 노드 표 `DSP_NODES` — 이름이 `band_dsp::`로 시작),
//! - 앱 `band`(워크스페이스 루트 `src`, 노드 표 `APP_NODES` — 이름이 `band_lib::`로 시작, 계층 6 어댑터).
//!
//! `band-dsp`·앱의 모듈은 앞에 크레이트 이름을 붙인 경로로 보고(`crate::`도 그 뿌리), 위 크레이트의 `band_dsp::…`·
//! `band_engine::…` 경로는 그 노드로 가는 간선이다 — 계층·`FORBIDDEN`("net은 미디어 상태를 모른다")·`ADAPTER_MAY_USE`
//! ("어댑터는 제어 평면·계약 DTO만")가 크레이트 경계를 넘어서도 그대로 선다. 반대 방향(아래 크레이트 → 위 크레이트)과
//! 매니페스트에 없는 외부 크레이트는 컴파일러가 막는다(의존이 없다) — 그래서 의존 목록은 매니페스트 검사
//! (`dsp_crate_uses_only_dsp_deps`, `engine_crate_deps_follow_graph`)로 고정한다.
//! 1. 계층: 노드는 자기 계층 이하만 쓴다. 같은 계층은 허용하되 순환(강한 연결 요소, 길이 무관)은 금지.
//!    어댑터(6)는 `ADAPTER_MAY_USE`만. 테스트 표면 `testing`(7)은 맨 위라 무엇이든 쓰고, 아무도 그것을 쓰지 못한다.
//! 2. 예외 래칫: 지금 어긋난 간선은 `EXCEPTIONS`에 "풀 단계"와 함께 고정돼 있다.
//!    - 목록 밖 위반은 0이어야 한다. **새 예외 추가는 금지**: 새 어긋남은 고친다. 꼭 필요하면 PR 본문에 사유를 적고
//!      리뷰에서 High로 다룬다(`MAX_EXCEPTIONS`를 올려야 해서 diff에 보인다).
//!    - 목록 안 항목이 해소되면(간선이 사라지면) 실패한다 → 지우고 `MAX_EXCEPTIONS`를 내린다.
//!    - 풀 단계는 아직 열린 단계(`OPEN_STAGES`)여야 한다.
//!
//! 파서 전제(어기면 `parser_assumptions_hold`가 실패):
//! - 단위 테스트 모듈 `#[cfg(test)] mod tests { … }`는 파일 끝에 있다. 그 안은 보지 않는다(테스트는 계층을 건너 도우미를 쓸 수 있다).
//! - 원시 문자열 `r"…"`, `r#"…"#`는 지운다(`#` 개수 일치). 주석·문자열·문자 리터럴 안의 경로는 간선이 아니다.
//!
//! 간선 목록 보기: `cargo test --test architecture -- --nocapture`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 노드 → (계층, 이름). 계층 번호가 작을수록 아래(기초)다. 문서 5절 표와 같다.
const NODES: &[(&str, u8)] = &[
    // 0 공용 기초(플랫폼 호출 포함 — `sys::*`는 OS API를 부르므로 `band-dsp`에 들지 않는다, ⑩-0). 결정적 기초는 `DSP_NODES`.
    ("sys", 0),
    ("sys::prio", 0),
    ("sys::cpu", 0),
    ("sys::placement", 0),
    ("sys::time", 0),
    // 1 알고리즘·상태(스레드·소켓 없음). 이 크레이트에 남은 것 — `band-dsp`가 쓰지 않는 순수 노드와 OS 난수·정규식을 쓰는 노드.
    ("engine::device::types", 1),
    ("net::connlog", 1),
    ("net::crypto", 1),
    // Ping 평문 조립·해석과 에코 상태(순수 — 소켓·스레드·락 없음, ⑥-7). 하네스(`tests/sim/loss_codec.rs`)가 쓴다.
    ("net::peer::ping", 1),
    ("diag::mask", 1),
    ("engine::measure::analysis", 1),
    ("engine::metronome::plan", 1),
    ("engine::wasapi::format", 1),
    // 2 E 장치 I/O·RT
    ("engine::rt", 2),
    // 녹음 파일 쓰기(파일 I/O·벽시계, 녹음 쓰기 스레드 전용). ⑩-0에서 계층 1에서 올림(`engine-stage5-10-scope.md` 7.1절).
    ("engine::wav", 2),
    ("engine::flac", 2),
    ("engine::device", 2),
    ("engine::device::backend", 2),
    ("engine::device::cpal", 2),
    ("engine::device::select", 2),
    ("engine::device::fake", 2),
    ("engine::wasapi", 2),
    ("engine::wasapi::enumerate", 2),
    ("engine::wasapi::stream", 2),
    ("engine::wasapi::probe", 2),
    ("engine::record", 2),
    ("engine::record::rt_ring", 2),
    ("engine::record::marks", 2),
    ("engine::record::sidecar", 2),
    ("engine::record::writer", 2),
    ("engine::metronome", 2),
    ("engine::metronome::click", 2),
    ("engine::metronome::sched", 2),
    // 3 D 네트워크
    ("net", 3),
    ("net::peer", 3),
    // 계약 DTO(어댑터가 쓰는 디스크립터). `net::peer`의 다른 항목을 쓰지 않는다(⑥-7). `conn`은 `Peer`와 서로 불러 `net::peer`로 접힌다.
    ("net::peer::descriptor", 3),
    ("net::relay", 3),
    ("net::stun", 3),
    ("net::qos", 3),
    // 송신 큐 지연·락 대기 계측(`Mutex`·`Instant::now`). 칸 함수는 `band_dsp::histo`로 내리고 나머지는 계층 3(⑩-0).
    ("net::txlat", 3),
    // 4 C 워커 파이프라인
    ("engine::worker", 4),
    ("engine::measure", 4),
    ("engine::measure::loopback", 4),
    ("engine::measure::echo", 4),
    // 5 B 제어 평면
    // 미디어 명령 큐의 제어 평면 쪽 끝(`MediaPort`: Mutex·수면·시계). 링은 `band_dsp::peer_media`가 만든다(⑩-0). ⑩-0~⑩-3에는
    // 하네스(당시 `engine::sim`, 계층 4)가 써서 계층 4였다. 하네스가 테스트 타깃(`tests/sim`, `testing` 경유)으로 나간 뒤 제품에서
    // 쓰는 곳은 `engine`(5)뿐이라 ⑨b-2에서 문서 7.3절 처음 안대로 5로 올렸다 — 이제 워커(4)가 부르면 계층 규칙으로 실패한다
    // (전에는 `FORBIDDEN`의 `engine::worker → engine::media_port`가 막았다, #159 L2).
    ("engine::media_port", 5),
    ("engine", 5),
    ("engine::cpuprof", 5),
    ("engine::report", 5),
    ("manager", 5),
    ("settings", 5),
    ("diag", 5),
    ("diag::timeline", 5),
    ("diag::export", 5),
    // 크레이트 뿌리(`lib.rs` — 모듈 선언뿐). ⑩-1까지는 앱 뿌리(`crate`, 계층 6)였다. 어댑터(6)는 앱 크레이트의 `APP_NODES`.
    ("band_engine", 5),
    // 7 테스트 표면(`cfg(any(test, feature = "test-support"))`, ⑨a). 모든 계층을 다시 내보낸다. 제품 노드는 위 계층을 못 쓰므로
    // 이 노드를 쓰는 제품 코드도 계층 규칙으로 실패한다(제품 빌드에서는 컴파일도 안 된다 — CI `Clippy (제품 구성)`).
    ("testing", 7),
];

/// `band-dsp`(`crates/band-dsp/src`)의 노드(⑩-1). 계층은 옮기기 전(`engine::*`·`net::*`일 때)과 같다. 이 크레이트의
/// `band_dsp::…` 경로도 이 노드로 간다. 정의(ADR 제안 3): 스레드를 만들지 않고, 소켓·파일·장치·OS API를 부르지 않고,
/// 벽시계를 읽지 않는 코드.
const DSP_NODES: &[(&str, u8)] = &[
    // 0
    ("band_dsp::config", 0),
    ("band_dsp::packet", 0),
    // 스레드 우선순위 결과 슬롯(원자값·enum). `sys::prio`(OS 호출)가 쓰고 `band_dsp::stats`가 필드로 갖는다(⑩-0).
    ("band_dsp::prio_slot", 0),
    // 1
    ("band_dsp::codec", 1),
    ("band_dsp::resample", 1),
    ("band_dsp::jitter", 1),
    ("band_dsp::relay_codec", 1),
    ("band_dsp::mixer", 1),
    ("band_dsp::peer_media", 1),
    ("band_dsp::stats", 1),
    ("band_dsp::rt_timing", 1),
    // 지연 히스토그램 칸 함수(순수). `rt_timing`·`net::txlat`이 쓴다(⑩-0).
    ("band_dsp::histo", 1),
    ("band_dsp::clock", 1),
    ("band_dsp::media_link", 1),
    ("band_dsp::types", 1),
    // 크레이트 뿌리(`lib.rs` — 모듈 선언뿐)
    ("band_dsp", 1),
];

/// 앱 `band`(워크스페이스 루트 `src`, lib 이름 `band_lib`)의 노드(⑩-2). 전부 계층 6 어댑터 — Tauri 진입점(`lib.rs`의 `run`),
/// 커맨드(`commands`), 실행 파일(`main.rs`). `ADAPTER_MAY_USE`만 쓸 수 있다. 엔진·dsp는 앱에 의존하지 않으므로(컴파일러가
/// 막는다) 이 노드로 오는 간선은 앱 안에서만 생긴다.
const APP_NODES: &[(&str, u8)] = &[("band_lib", 6), ("band_lib::commands", 6), ("band_lib::main", 6)];

/// 현재 어긋남(고정): (from, to, 풀 단계). 위 계층으로 가는 간선, 같은 계층 순환을 끊을 간선, 어댑터의 직접 의존.
/// 문서 5.3절 표와 같다. 단계를 끝내면 여기서 지운다(남아 있으면 `module_dependencies_follow_layers`가 실패).
const EXCEPTIONS: &[(&str, &str, &str)] = &[];

/// 예외 수 상한(래칫). 단계를 끝내 예외를 지우면 이 값도 내린다. 올리는 것은 금지(위 주석).
const MAX_EXCEPTIONS: usize = 0;

/// 아직 끝나지 않은 단계(문서 9절). 단계를 끝내면 지운다.
const OPEN_STAGES: &[&str] = &["③"];

/// 계층 규칙상 허용(아래로 가는 간선)이지만 금지하는 간선(엔진 ③, `engine-stage3-design.md` 7.1절):
/// net은 미디어 상태(지터 버퍼·믹스 제어)를 모른다. 미디어 쪽과는 `band_dsp::media_link`로만 잇는다.
/// `from`이 `"net"`이면 `net`과 `net::*` 모든 노드에 적용한다.
const FORBIDDEN: &[(&str, &str, &str)] = &[
    ("net", "band_dsp::jitter", "③-b"),
    ("net", "band_dsp::mixer", "③-b"),
    // net은 수신 큐(`band_dsp::media_link`)에 넣기만 한다. 피어 미디어 상태는 워커 소유(③-a3).
    ("net", "band_dsp::peer_media", "③-a3"),
    // ⑩-0~⑨b-1의 `engine::worker → engine::media_port`(#159 L2)는 ⑨b-2에서 `media_port`를 계층 5로 올려 계층 규칙이 대신한다.
];

/// `band-dsp`로 옮기면 안 되는 노드(7.1절 표의 "아니오"와 "나눈다"의 제어 쪽). 이 크레이트의 `NODES`에 있어야 하고, 같은 이름의
/// 모듈이 `DSP_NODES`에 생기면 실패한다(`dsp_excluded_nodes_stay_in_band`).
const DSP_EXCLUDED: &[&str] = &[
    // OS 난수(키 생성). engine에 남겨 dsp에서 암호 의존을 뺀다(PR #125 검토 L10).
    "net::crypto",
    // 파일 I/O·벽시계(녹음 쓰기 스레드).
    "engine::wav",
    "engine::flac",
    // OS 호출(MMCSS·타이머 해상도·스레드 시간·벽시계).
    "sys",
    "sys::prio",
    "sys::cpu",
    "sys::placement",
    "sys::time",
    // `regex`·`sha2`. 쓰는 곳이 record·diag뿐.
    "diag::mask",
    // 제어 쪽 끝(Mutex·수면·시계), 송신 지연·락 대기 계측(Mutex·Instant::now).
    "engine::media_port",
    "net::txlat",
];

/// `band-dsp`의 시험 전용 표면(⑩-1, #159 리뷰 M1의 경계 항목 처리 결과): `#[cfg(any(test, feature = "test-support"))] pub` 항목.
/// `band-dsp`의 `cfg(test)`는 이 크레이트의 시험 빌드에서 꺼지므로, 이 크레이트(통합 시험·하네스·단위 시험)가 쓰는 시험 훅은
/// `band-dsp`의 `test-support` 피처 뒤에 두고 이 크레이트의 `test-support`가 켠다. (파일(`band-dsp/src` 기준), 이름, 쓰는 곳
/// (매니페스트 기준, 쉼표)). 검사(`dsp_test_support_items_are_listed`): 실제 집합 = 이 목록(새 훅이 생기면 여기 적는다 — 제품 API가
/// 아니라 시험 표면이 늘어난다는 뜻이다), 적힌 사용처는 `band-dsp` 밖이고 실제로 그 이름을 부른다.
///
/// ⑩-0의 나머지 경계 항목(`pub(crate)` 9개 — `histo`의 칸 함수 셋, `PeerMedia`의 공급·수신 다섯, `SeqCell::write`)은 `pub`이 됐다.
/// `histo::bucket_value`는 쓰는 곳이 `net::txlat` 시험의 대조 하나라 그 대조를 `histo` 시험으로 옮기고 `pub(crate)`로 남겼다.
/// `pub(crate)`가 크레이트 밖에서 안 보이는 것은 이제 컴파일러가 막으므로 목록으로 고정하지 않는다.
///
/// ⑨b-1(공개 표면 좁히기, 6.3절): `band-dsp` 밖(이 크레이트·앱·하네스)이 쓰지 않는 `pub`은 `pub(crate)`로 내렸다. 밖에서는
/// 시험만 쓰고 `band-dsp` 안에서는 안 쓰는 항목은 여기로 옮겼다(`JitterStats::corrections`). 안에서도 쓰는 항목(하네스가 직접
/// 모는 `JitterBuffer::{new, insert, stats}`·`JitterStats`의 칸 등)은 `pub`으로 남는다 — 가시성을 피처로 가를 수 없다.
/// 모듈 단위는 `dsp_public_surface_is_used_outside`가 지킨다.
const DSP_TEST_SUPPORT: &[(&str, &str, &str)] = &[
    ("codec.rs", "opus_decoders_created", "tests/rt_alloc.rs"),
    ("peer_media.rs", "has_feed_resampler", "src/engine/worker/tests.rs"),
    ("peer_media.rs", "jitter", "tests/sim/tune.rs"),
    ("peer_media.rs", "rx_dropped", "tests/sim/tune.rs"),
    ("peer_media.rs", "drain", "src/engine/media_port.rs"),
    // ⑨b-1: 밖은 시험만 부른다(제품 보고는 `MediaStats`의 칸을 읽는다).
    ("jitter/stats.rs", "corrections", "src/engine/worker/tests.rs, tests/loopback.rs"),
];

/// `band-dsp`의 `[dependencies]`(7.2절 그래프). 개발 의존(`rand`·`sha2`·`serde_json` — 지터 단위 테스트)은 보지 않는다.
const DSP_EXTERN_ALLOWED: &[&str] = &["opus", "rubato", "ringbuf", "serde", "log"];

/// 어댑터(계층 6)가 쓸 수 있는 노드: 제어 평면과 계약 DTO(설정·보고·디스크립터)를 가진 모듈만. 로직은 제어 평면에서 부른다.
const ADAPTER_MAY_USE: &[&str] = &[
    "manager",
    // 앱 설정(녹음 폴더, ⑦-2). 저장 형식·우선순위는 여기, Tauri 경로 조회·대화상자는 어댑터.
    "settings",
    "diag",
    "diag::export",
    "band_dsp::config",
    "engine::device::types",
    "engine::device::select",
    "engine::measure",
    "engine::metronome::plan",
    "engine::record::sidecar",
    "engine::report",
    "net",
    "net::peer",
    // 벽시계(진단 내보내기 파일 이름 시각). 로직이 아닌 계층 0 함수(④-1에서 diag::now_ms를 옮김).
    "sys::time",
    "band_lib::commands",
];

/// 이 크레이트(`band-engine`) 소스.
fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 워크스페이스 루트(`client/src-tauri` — 앱 패키지 `band`).
fn ws_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// 앱(`band`) 소스(⑩-2).
fn app_src_dir() -> PathBuf {
    ws_dir().join("src")
}

/// `band-dsp` 소스(⑩-1).
fn dsp_dir() -> PathBuf {
    ws_dir().join("crates").join("band-dsp")
}

fn dsp_src_dir() -> PathBuf {
    dsp_dir().join("src")
}

/// `band-dsp` 모듈 경로의 첫 마디(노드 이름 `band_dsp::…`와 같다).
const DSP_ROOT: &str = "band_dsp";
/// 앱 모듈 경로의 첫 마디(lib 이름, 노드 이름 `band_lib::…`와 같다).
const APP_ROOT: &str = "band_lib";
/// 이 크레이트의 이름(앱이 쓰는 경로 `band_engine::…`의 첫 마디, 뿌리 노드 이름).
const ENGINE_ROOT: &str = "band_engine";

/// 파일 → 보고용 상대 경로: 이 크레이트는 `src` 기준(`engine/rt.rs`), 다른 크레이트는 워크스페이스 루트 기준
/// (`crates/band-dsp/src/jitter/mod.rs`, `src/commands.rs`).
fn rel_of(f: &Path) -> String {
    let r = f.strip_prefix(src_dir()).or_else(|_| f.strip_prefix(ws_dir())).unwrap_or(f);
    r.display().to_string().replace('\\', "/")
}

/// `rel_of`의 반대: `crates/…`·`src/…`면 워크스페이스 루트 기준, 아니면 이 크레이트 `src` 기준.
fn src_file(rel: &str) -> PathBuf {
    if rel.starts_with("crates/") || rel.starts_with("src/") {
        ws_dir().join(rel)
    } else {
        src_dir().join(rel)
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("src 읽기") {
        let p = e.expect("항목").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `#[path = "x.rs"] mod name;`로 붙인 파일 → 그 모듈 경로(부모 모듈 + name).
fn path_attr_modules(files: &[PathBuf]) -> BTreeMap<PathBuf, Vec<String>> {
    let mut out = BTreeMap::new();
    for f in files {
        let text = std::fs::read_to_string(f).expect("읽기");
        let mut lines = text.lines().map(str::trim).peekable();
        while let Some(l) = lines.next() {
            let Some(rest) = l.strip_prefix("#[path = \"") else { continue };
            let target = rest.trim_end_matches("\"]");
            while lines.peek().is_some_and(|n| n.starts_with("#[")) {
                lines.next();
            }
            let Some(name) = lines.next().and_then(|n| n.strip_prefix("mod ")).map(|n| n.trim_end_matches(';').trim()) else {
                continue;
            };
            let dir = f.parent().expect("부모");
            let mut module = module_of_file(f);
            module.push(name.to_string());
            out.insert(dir.join(target), module);
        }
    }
    out
}

/// 파일 → 모듈 경로(세그먼트). `src/engine/mod.rs` → [engine], `src/engine/worker/send.rs` → [engine, worker, send].
/// `band-dsp`는 앞에 `band_dsp`: `crates/band-dsp/src/jitter/target.rs` → [band_dsp, jitter, target], `lib.rs` → [band_dsp].
/// 앱은 앞에 `band_lib`: `src/commands.rs`(워크스페이스 루트) → [band_lib, commands], `main.rs` → [band_lib, main].
fn module_of_file(file: &Path) -> Vec<String> {
    let (rel, mut segs) = if let Ok(r) = file.strip_prefix(dsp_src_dir()) {
        (r, vec![DSP_ROOT.to_string()])
    } else if let Ok(r) = file.strip_prefix(app_src_dir()) {
        (r, vec![APP_ROOT.to_string()])
    } else {
        (file.strip_prefix(src_dir()).expect("src 아래"), Vec::new())
    };
    segs.extend(rel.iter().map(|s| s.to_string_lossy().into_owned()));
    let last = segs.pop().unwrap_or_default();
    let stem = last.trim_end_matches(".rs");
    if stem != "mod" && stem != "lib" {
        segs.push(stem.to_string());
    }
    segs
}

/// 세 크레이트의 노드 표.
fn all_nodes() -> impl Iterator<Item = &'static (&'static str, u8)> {
    NODES.iter().chain(DSP_NODES).chain(APP_NODES)
}

/// 모듈 경로 → 노드: 가장 긴 접두가 노드 표(`NODES`·`DSP_NODES`·`APP_NODES`)에 있는 것. 이 크레이트의 뿌리 `lib.rs`(빈 경로)는 `band_engine`.
fn node_of(path: &[String]) -> Option<&'static str> {
    if path.is_empty() {
        return Some(ENGINE_ROOT);
    }
    (1..=path.len()).rev().find_map(|n| {
        let key = path[..n].join("::");
        all_nodes().find(|(k, _)| *k == key).map(|(k, _)| *k)
    })
}

/// 주석과 문자열 리터럴을 지운다(경로처럼 보이는 문서·메시지를 간선으로 세지 않게).
fn strip(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if let Some(end) = raw_string_end(b, i) {
            i = end;
            out.push_str("\"\"");
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
        } else if b[i] == b'\'' && b.get(i + 1) == Some(&b'\\') {
            // 이스케이프 문자 리터럴 '\n', '\"', '\u{..}'
            i += 2;
            while i < b.len() && b[i] != b'\'' {
                i += 1;
            }
            i += 1;
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            // 문자 리터럴 'x'(수명 'a는 그대로 둔다)
            i += 3;
        } else {
            let ch = src[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// `i`가 원시 문자열(`r"…"`, `r#"…"#`, `br"…"`) 시작이면 그 끝 다음 위치.
fn raw_string_end(b: &[u8], i: usize) -> Option<usize> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let byte_prefix = i > 0 && b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2]));
    if b[i] != b'r' || (i > 0 && ident(b[i - 1]) && !byte_prefix) {
        return None;
    }
    let mut j = i + 1;
    let mut hashes = 0;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    if b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        if b[j] == b'"' && b[j + 1..].iter().take(hashes).filter(|&&c| c == b'#').count() == hashes {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(b.len())
}

/// 단위 테스트 모듈이 파일 끝에 있는지(그 뒤에 코드가 없는지). 주석·문자열을 지운 코드 기준.
fn unit_tests_at_end(src: &str) -> bool {
    let code = strip(src);
    let cut = without_unit_tests(&code).len();
    if cut == code.len() {
        return true;
    }
    let rest = &code[cut..];
    let Some(open) = rest.find('{') else { return false };
    let mut depth = 0usize;
    for (k, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return rest[open + k + 1..].trim().is_empty();
                }
            }
            _ => {}
        }
    }
    false
}

/// 단위 테스트 모듈 앞까지만 남긴다.
fn without_unit_tests(src: &str) -> &str {
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let rest = src[at + line.len()..].trim_start();
        if line.trim() == "#[cfg(test)]" && rest.strip_prefix("mod tests").is_some_and(|r| r.trim_start().starts_with('{')) {
            return &src[..at];
        }
        at += line.len();
    }
    src
}

/// `use a::{b, c::{d, e}};`를 펼친다.
fn expand_use(tree: &str, prefix: &str, out: &mut Vec<String>) {
    let tree = tree.trim();
    if let Some(open) = tree.find('{') {
        let head = tree[..open].trim().trim_end_matches("::");
        let inner = &tree[open + 1..tree.rfind('}').unwrap_or(tree.len())];
        let base = join(prefix, head);
        let mut depth = 0;
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    expand_use(&inner[start..i], &base, out);
                    start = i + 1;
                }
                _ => {}
            }
        }
        expand_use(&inner[start..], &base, out);
    } else if !tree.is_empty() {
        let path = tree.split(" as ").next().unwrap_or(tree).trim();
        out.push(join(prefix, path));
    }
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}::{b}"),
    }
}

/// 이 파일이 선언한 자식 모듈 이름(`mod a;`, `pub mod a;`, 인라인 `mod a { … }`). 상대 경로 `a::…`의 해석에 쓴다.
fn declared_children(code: &str) -> BTreeSet<String> {
    let b = code.as_bytes();
    let mut out = BTreeSet::new();
    let mut from = 0;
    while let Some(i) = find_word(&code[from..], "mod ") {
        let at = from + i;
        let rest = &code[at + 4..];
        let n = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        let after = rest[n..].trim_start();
        if n > 0 && (after.starts_with(';') || after.starts_with('{')) && (at == 0 || b[at - 1] != b':') {
            out.insert(rest[..n].to_string());
        }
        from = at + 4;
    }
    out
}

/// 파일 안의 크레이트 내부 경로와 그 위치(모듈 경로로 해석 전): `use` 문(펼침), 본문의 `crate::`/`super::`/`self::`
/// 경로, 자식 모듈 이름으로 시작하는 상대 경로(`peer::Peer`).
fn raw_paths(code: &str, children: &BTreeSet<String>) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    // use 문(범위는 본문 스캔에서 뺀다 — `use crate::{a, b}`의 `crate::`를 거짓 간선으로 다시 잡지 않게)
    let mut uses: Vec<(usize, usize)> = Vec::new();
    let mut base = 0;
    while let Some(i) = find_word(&code[base..], "use ") {
        let at = base + i;
        let after = &code[at + 4..];
        let end = after.find(';').unwrap_or(after.len());
        let mut v = Vec::new();
        expand_use(&after[..end], "", &mut v);
        out.extend(v.into_iter().map(|p| (at, p)));
        uses.push((at, at + 4 + end));
        base = at + 4 + end;
    }
    let in_use = |s: usize| uses.iter().any(|&(a, e)| a <= s && s < e);
    // 본문의 경로(타입·함수 경로)
    let b = code.as_bytes();
    let mut kws: Vec<String> = vec!["crate::".into(), "super::".into(), "self::".into(), format!("{DSP_ROOT}::"), format!("{ENGINE_ROOT}::")];
    kws.extend(children.iter().map(|c| format!("{c}::")));
    for kw in &kws {
        let mut from = 0;
        while let Some(i) = code[from..].find(kw.as_str()) {
            let s = from + i;
            from = s + kw.len();
            if in_use(s) || (s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'_' || b[s - 1] == b':')) {
                continue;
            }
            let end = code[s..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .map_or(code.len(), |e| s + e);
            if code[end..].starts_with('{') {
                continue; // `crate::{…}` 묶음은 use 문에서만 나온다
            }
            out.push((s, code[s..end].trim_end_matches(':').to_string()));
        }
    }
    out
}

/// 파일 안의 인라인 모듈(`mod name { … }`) 범위: (시작, 끝, 이름 경로).
fn inline_modules(code: &str) -> Vec<(usize, usize, Vec<String>)> {
    let b = code.as_bytes();
    let mut ranges = Vec::new();
    let mut stack: Vec<(usize, usize, String)> = Vec::new(); // (여는 위치, 깊이, 이름)
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                if let Some(&(start, d, _)) = stack.last() {
                    if d == depth {
                        let names: Vec<String> = stack.iter().map(|s| s.2.clone()).collect();
                        ranges.push((start, i, names));
                        stack.pop();
                    }
                }
                depth = depth.saturating_sub(1);
            }
            b'm' if code[i..].starts_with("mod ") && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) => {
                let rest = &code[i + 4..];
                let name_len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
                let after = rest[name_len..].trim_start();
                if name_len > 0 && after.starts_with('{') {
                    let open = i + 4 + name_len + (rest[name_len..].len() - after.len());
                    stack.push((open, depth + 1, rest[..name_len].to_string()));
                    i = open; // 다음 반복에서 '{'를 센다
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    ranges
}

/// 위치 `at`의 모듈 경로(파일 모듈 + 가장 안쪽 인라인 모듈).
fn module_at(file_module: &[String], inline: &[(usize, usize, Vec<String>)], at: usize) -> Vec<String> {
    let inner = inline.iter().filter(|(s, e, _)| *s <= at && at <= *e).max_by_key(|(_, _, n)| n.len());
    let mut m = file_module.to_vec();
    if let Some((_, _, names)) = inner {
        m.extend(names.iter().cloned());
    }
    m
}

fn find_word(s: &str, w: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut from = 0;
    while let Some(i) = s[from..].find(w) {
        let p = from + i;
        if p == 0 || !(b[p - 1].is_ascii_alphanumeric() || b[p - 1] == b'_') {
            return Some(p);
        }
        from = p + w.len();
    }
    None
}

/// 경로를 이 파일의 모듈 기준으로 절대 모듈 경로로 바꾼다. 첫 세그먼트가 자식 모듈 이름(`children`)이면 `self::`로 본다.
/// `crate::`는 그 파일 크레이트의 뿌리(`band-dsp`면 [band_dsp], 앱이면 [band_lib]), `band_dsp::`는 `band-dsp` 노드로(⑩-1),
/// `band_engine::`은 이 크레이트 노드로(접두 없음, ⑩-2) 간다. 그 밖의 크레이트(std, 외부 크레이트, 실행 파일의 `band_lib::`)면 None.
fn resolve(path: &str, module: &[String], children: &BTreeSet<String>) -> Option<Vec<String>> {
    let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    let mut cur: Vec<String>;
    let mut it = segs.iter().peekable();
    match it.peek().copied() {
        Some(&"crate") => {
            it.next();
            cur = match module.first().map(String::as_str) {
                Some(r @ (DSP_ROOT | APP_ROOT)) => vec![r.to_string()],
                _ => Vec::new(),
            };
        }
        Some(&DSP_ROOT) => cur = Vec::new(),
        Some(&ENGINE_ROOT) => {
            it.next();
            cur = Vec::new();
        }
        Some(&"super") | Some(&"self") => {
            cur = module.to_vec();
            while let Some(&&s) = it.peek() {
                match s {
                    "super" => {
                        cur.pop();
                    }
                    "self" => {}
                    _ => break,
                }
                it.next();
            }
        }
        Some(first) if children.contains(*first) => cur = module.to_vec(),
        _ => return None,
    }
    cur.extend(it.map(|s| s.to_string()));
    Some(cur)
}

fn layer(node: &str) -> u8 {
    all_nodes().find(|(k, _)| *k == node).map(|(_, l)| *l).expect("NODES·DSP_NODES·APP_NODES에 있는 노드")
}

/// 세 크레이트(이 크레이트, `band-dsp`, 앱)의 소스 파일과 그 모듈 경로.
fn source_files() -> Vec<(PathBuf, Vec<String>)> {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    rs_files(&dsp_src_dir(), &mut files);
    rs_files(&app_src_dir(), &mut files);
    let attached = path_attr_modules(&files);
    files
        .into_iter()
        .map(|f| {
            let m = attached.get(&f).cloned().unwrap_or_else(|| module_of_file(&f));
            (f, m)
        })
        .collect()
}

/// 실제 간선(from → to, 노드 단위)과 그 간선이 나온 파일.
fn edges() -> BTreeMap<(&'static str, &'static str), BTreeSet<String>> {
    let mut edges: BTreeMap<(&str, &str), BTreeSet<String>> = BTreeMap::new();
    for (f, module) in source_files() {
        // 파일로 뗀 단위 테스트 모듈(`#[cfg(test)] mod tests;` → `tests.rs`)도 보지 않는다.
        if module.last().is_some_and(|m| m == "tests") {
            continue;
        }
        let from = node_of(&module).unwrap_or_else(|| panic!("{}: NODES에 없는 모듈 {:?} — 표에 계층을 정해 넣을 것", f.display(), module));
        let text = std::fs::read_to_string(&f).expect("읽기");
        let code = strip(without_unit_tests(&text));
        let inline = inline_modules(&code);
        let children = declared_children(&code);
        for (at, p) in raw_paths(&code, &children) {
            let Some(abs) = resolve(&p, &module_at(&module, &inline, at), &children) else { continue };
            let Some(to) = node_of(&abs) else { continue };
            if to != from {
                edges.entry((from, to)).or_default().insert(rel_of(&f));
            }
        }
    }
    edges
}

/// Tarjan 강한 연결 요소 탐색 상태.
struct Scc<'a> {
    adj: BTreeMap<&'a str, Vec<&'a str>>,
    index: BTreeMap<&'a str, usize>,
    low: BTreeMap<&'a str, usize>,
    stack: Vec<&'a str>,
    on: BTreeSet<&'a str>,
    next: usize,
    out: Vec<BTreeSet<&'a str>>,
}

impl<'a> Scc<'a> {
    fn visit(&mut self, v: &'a str) {
        self.index.insert(v, self.next);
        self.low.insert(v, self.next);
        self.next += 1;
        self.stack.push(v);
        self.on.insert(v);
        for w in self.adj.get(v).cloned().unwrap_or_default() {
            let cand = if !self.index.contains_key(w) {
                self.visit(w);
                Some(self.low[w])
            } else if self.on.contains(w) {
                Some(self.index[w])
            } else {
                None
            };
            if let Some(c) = cand {
                let lv = self.low.get_mut(v).expect("방문함");
                *lv = (*lv).min(c);
            }
        }
        if self.low[v] == self.index[v] {
            let mut scc = BTreeSet::new();
            while let Some(w) = self.stack.pop() {
                self.on.remove(w);
                scc.insert(w);
                if w == v {
                    break;
                }
            }
            if scc.len() > 1 {
                self.out.push(scc);
            }
        }
    }
}

/// 같은 계층 간선 중 예외(`skip`)가 아닌 것으로 만든 그래프의 강한 연결 요소(크기 2 이상) — 길이와 무관하게 순환을 찾는다.
/// 예외 간선은 "그 단계에서 끊을 간선"이므로 빼고 본다: 순환마다 끊을 간선 하나가 예외에 있으면 통과.
fn same_layer_cycles<'a>(edges: &BTreeMap<(&'a str, &'a str), BTreeSet<String>>, skip: &BTreeSet<(&str, &str)>) -> Vec<BTreeSet<&'a str>> {
    let mut t = Scc { adj: BTreeMap::new(), index: BTreeMap::new(), low: BTreeMap::new(), stack: Vec::new(), on: BTreeSet::new(), next: 0, out: Vec::new() };
    for &(a, b) in edges.keys() {
        if layer(a) == layer(b) && !skip.contains(&(a, b)) {
            t.adj.entry(a).or_default().push(b);
        }
    }
    let nodes: Vec<&str> = t.adj.keys().copied().collect();
    for v in nodes {
        if !t.index.contains_key(v) {
            t.visit(v);
        }
    }
    t.out
}

fn files_of(edges: &BTreeMap<(&str, &str), BTreeSet<String>>, k: (&str, &str)) -> String {
    edges.get(&k).map(|f| f.iter().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

#[test]
fn module_dependencies_follow_layers() {
    let edges = edges();
    for ((from, to), files) in &edges {
        println!("{from} → {to}  [{}]", files.iter().cloned().collect::<Vec<_>>().join(", "));
    }
    let exc: BTreeSet<(&str, &str)> = EXCEPTIONS.iter().map(|(a, b, _)| (*a, *b)).collect();
    let mut bad = Vec::new();
    for &(from, to) in edges.keys() {
        let (lf, lt) = (layer(from), layer(to));
        if exc.contains(&(from, to)) {
            continue;
        }
        if lt > lf {
            bad.push(format!("  {from} → {to}: 위 계층 사용({lf} → {lt}) [{}]", files_of(&edges, (from, to))));
        } else if lf == 6 && !ADAPTER_MAY_USE.contains(&to) {
            bad.push(format!("  {from} → {to}: 어댑터는 제어 평면·계약 DTO만(ADAPTER_MAY_USE) [{}]", files_of(&edges, (from, to))));
        }
    }
    // 같은 계층 순환: 예외 간선을 뺀 뒤에도 남는 순환(강한 연결 요소)은 위반.
    for scc in same_layer_cycles(&edges, &exc) {
        for &(a, b) in edges.keys() {
            if scc.contains(a) && scc.contains(b) && !exc.contains(&(a, b)) {
                bad.push(format!("  {a} → {b}: 같은 계층 순환 {scc:?} [{}]", files_of(&edges, (a, b))));
            }
        }
    }
    let stale: Vec<String> = EXCEPTIONS
        .iter()
        .filter(|(a, b, _)| !edges.contains_key(&(*a, *b)))
        .map(|(a, b, s)| format!("  {a} → {b} (단계 {s})"))
        .collect();
    assert!(
        bad.is_empty(),
        "의존 방향 위반(engine-architecture.md 5절) — 코드를 고칠 것. 예외 추가는 금지(이 파일 머리 주석):\n{}",
        bad.join("\n")
    );
    assert!(
        stale.is_empty(),
        "해소된 예외 — EXCEPTIONS와 문서 5.3절에서 지우고 MAX_EXCEPTIONS를 내릴 것:\n{}",
        stale.join("\n")
    );
}

#[test]
fn forbidden_edges_absent() {
    let edges = edges();
    let covers = |rule: &str, node: &str| node == rule || node.starts_with(&format!("{rule}::"));
    let mut bad = Vec::new();
    for &(from, to) in edges.keys() {
        for (rf, rt, stage) in FORBIDDEN {
            if covers(rf, from) && to == *rt {
                bad.push(format!("  {from} → {to}: 금지 간선(단계 {stage}) [{}]", files_of(&edges, (from, to))));
            }
        }
    }
    assert!(bad.is_empty(), "금지 간선(FORBIDDEN) — net은 미디어 상태를, 워커는 미디어 명령 큐의 제어 쪽 끝을 직접 쓰지 않는다:\n{}", bad.join("\n"));
}

/// 매니페스트의 `[dependencies]`·`[target.*.dependencies]` 이름(`-` → `_`). 개발·빌드 의존은 뺀다.
/// 점 표기 섹션(`[dependencies.parking_lot]`, `[target.'cfg(windows)'.dependencies.windows-sys]`)은 헤더에서 이름을 읽는다
/// (#160 리뷰 L3 — 이 꼴로 의존을 더해도 `dsp_crate_uses_only_dsp_deps`가 놓치지 않게).
fn extern_crates(manifest: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(manifest).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
    extern_crates_of(&text)
}

fn extern_crates_of(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_deps = false;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            let head = l.trim_start_matches('[').trim_end_matches(']').trim();
            in_deps = head == "dependencies" || (head.starts_with("target.") && head.ends_with(".dependencies"));
            // 점 표기: `dependencies.<이름>` 또는 `target.<…>.dependencies.<이름>`(`dev-dependencies.`·`build-dependencies.`는 아님).
            let dotted = head.strip_prefix("dependencies.").or_else(|| {
                head.starts_with("target.").then(|| head.rsplit_once(".dependencies.").map(|(_, n)| n)).flatten()
            });
            if let Some(name) = dotted {
                out.insert(name.trim_matches(|c| c == '"' || c == '\'').replace('-', "_"));
            }
            continue;
        }
        if in_deps && !l.starts_with('#') {
            if let Some((name, _)) = l.split_once('=') {
                let name = name.trim();
                if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
                    out.insert(name.replace('-', "_"));
                }
            }
        }
    }
    out
}

/// 코드(파일 끝 단위 시험을 지운 것, 문자열은 그대로 — 피처 이름을 읽어야 한다)의 시험 전용 항목: (이름, 종류).
/// - `support`: `test-support` 피처를 이름으로 든 `#[cfg(…)]`(`any(test, feature = "test-support")`, `feature = "test-support"`,
///   `all(…)` 등) 뒤의 비공개 아닌 항목. **모든 파일**에서 본다 — 하위 파일(`jitter/target.rs` 등)이 비공개 모듈이어도
///   그 안 `impl JitterBuffer`의 `pub fn`은 타입이 공개라 크레이트 밖에서 부를 수 있다(#160 리뷰 M1).
/// - `test`: `#[cfg(test)]` 뒤의 비공개 아닌 항목. 뿌리 파일(`root`)만(`band-dsp` 단위 시험에서만 보인다 — 참고용).
///
/// 줄 단위로 보고 `//`로 시작하는 주석 줄은 건너뛴다. ⑩-0에서는 `pub(crate)`·`pub(super)`도 경계 항목으로 봤으나
/// (`DSP_BOUNDARY`) 크레이트를 가른 뒤에는 컴파일러가 막는다.
fn test_only_items(code: &str, root: bool) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    let mut gate: Option<&'static str> = None;
    for line in code.lines().map(str::trim) {
        if line.starts_with("//") {
            continue;
        }
        let mut l = line;
        while l.starts_with("#[") {
            // 속성 끝: 괄호 짝이 맞는 `]`(피처 이름 문자열 안에 `]`는 없다).
            let mut depth = 0i32;
            let end = l.char_indices().skip(1).find(|&(_, c)| {
                match c {
                    '[' | '(' => depth += 1,
                    ']' | ')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            let attr = &l[..end.map_or(l.len(), |(e, _)| e + 1)];
            if attr == "#[cfg(test)]" {
                gate = Some("test");
            } else if attr.starts_with("#[cfg(") && attr.contains("\"test-support\"") {
                gate = Some("support");
            }
            match end {
                Some((e, _)) => l = l[e + 1..].trim_start(),
                None => break,
            }
        }
        if l.is_empty() || l.starts_with("#[") {
            continue;
        }
        let rest = ["pub(crate) ", "pub(super) ", "pub "].iter().find_map(|v| l.strip_prefix(v));
        let mut rest = rest.unwrap_or("");
        for q in ["const fn ", "unsafe fn ", "async fn "] {
            if rest.starts_with(q) {
                rest = &rest[q.len() - 3..];
            }
        }
        let item = ["fn ", "const ", "static ", "struct ", "enum ", "type ", "trait "].iter().find_map(|k| rest.strip_prefix(k));
        let was = gate.take();
        let (Some(item), Some(kind)) = (item, was) else { continue };
        if kind == "test" && !root {
            continue;
        }
        let name: String = item.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            out.push((name, kind));
        }
    }
    out
}

/// ⑩-1: `band-dsp`의 `test-support` 훅은 `DSP_TEST_SUPPORT`와 같다. 적힌 사용처는 `band-dsp` 밖이고 실제로 부른다.
/// 그리고 `band`의 `test-support`가 `band-dsp/test-support`를 켠다(안 켜면 훅이 이 크레이트 시험 빌드에서 사라진다 — 컴파일
/// 오류로도 드러나지만 피처 전달이 끊긴 이유를 바로 말해 준다).
#[test]
fn dsp_test_support_items_are_listed() {
    let mut found = BTreeSet::new();
    let mut roots = 0;
    for (f, module) in source_files() {
        if module.first().is_none_or(|m| m != DSP_ROOT) || module.last().is_some_and(|m| m == "tests") {
            continue;
        }
        let Some(node) = node_of(&module) else { continue };
        let root = module.join("::") == node;
        roots += usize::from(root);
        let rel = f.strip_prefix(dsp_src_dir()).unwrap_or(&f).display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&f).expect("읽기");
        for (name, kind) in test_only_items(without_unit_tests(&text), root) {
            if kind == "support" {
                found.insert((rel.clone(), name));
            }
        }
    }
    assert!(roots >= DSP_NODES.len() - 1, "band-dsp 뿌리 파일을 다 보지 못했다: {roots}");
    let listed: BTreeSet<(String, String)> = DSP_TEST_SUPPORT.iter().map(|(f, n, _)| (f.to_string(), n.to_string())).collect();
    let new: Vec<String> = found.difference(&listed).map(|(f, n)| format!("  {f}: {n}")).collect();
    let gone: Vec<String> = listed.difference(&found).map(|(f, n)| format!("  {f}: {n}")).collect();
    assert!(new.is_empty(), "band-dsp에 새 test-support 훅 — DSP_TEST_SUPPORT에 쓰는 곳을 적을 것:\n{}", new.join("\n"));
    assert!(gone.is_empty(), "DSP_TEST_SUPPORT의 훅이 없어졌다 — 목록에서 지울 것:\n{}", gone.join("\n"));
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (f, name, users) in DSP_TEST_SUPPORT {
        for u in users.split(", ") {
            assert!(!u.starts_with("crates/band-dsp/"), "{f}: {name} — 사용처 {u}는 band-dsp 안이다(cfg(test)로 충분)");
            let code = strip(&std::fs::read_to_string(manifest.join(u)).unwrap_or_else(|e| panic!("{u}: {e}")));
            let calls = [format!(".{name}("), format!("{name}("), format!("{name}::<")];
            assert!(calls.iter().any(|c| code.contains(c.as_str())), "{f}: {name} — 적힌 사용처 {u}가 부르지 않는다(목록 갱신)");
        }
    }
    let toml = std::fs::read_to_string(manifest.join("Cargo.toml")).expect("Cargo.toml");
    let line = toml.lines().map(str::trim).find(|l| l.starts_with("test-support =")).expect("band의 test-support 피처");
    assert!(line.contains("\"band-dsp/test-support\""), "band의 test-support가 band-dsp/test-support를 켜지 않는다: {line}");
}

/// 변이: `test-support` 게이트 항목은 형태(`any`·`all`·피처만)와 위치(뿌리·하위 파일, `impl` 블록 안)와 상관없이 잡고,
/// `cfg(test)`는 뿌리에서만, 보통 `pub`·비공개 시험 도우미·다른 피처(`asio`)·주석 줄은 잡지 않는다(#160 리뷰 M1).
#[test]
fn dsp_test_only_scanner_catches_variants() {
    let code = r#"pub(crate) fn a() {}
pub(super) const B: u8 = 0;
#[cfg(test)]
#[inline]
pub fn c() {}
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn d() {}
#[cfg(test)]
fn e() {}
pub fn f() {}
#[derive(Debug)]
pub(crate) struct G;
pub(crate) const fn h() {}
impl X {
    #[cfg(any(test, feature = "test-support"))]
    pub fn i(&self) {}
    #[cfg(feature = "test-support")]
    #[inline]
    pub fn j(&self) {}
    #[cfg(all(windows, feature = "test-support"))]
    pub fn k(&self) {}
    #[cfg(feature = "asio")]
    pub fn l(&self) {}
    // #[cfg(any(test, feature = "test-support"))]
    pub fn m(&self) {}
}
"#;
    let pick = |root| -> Vec<(String, &str)> { test_only_items(code, root) };
    let names = |v: &[(&str, &'static str)]| -> Vec<(String, &'static str)> { v.iter().map(|(n, k)| (n.to_string(), *k)).collect() };
    assert_eq!(pick(true), names(&[("c", "test"), ("d", "support"), ("i", "support"), ("j", "support"), ("k", "support")]));
    // 하위 파일(비공개 모듈 안의 `impl JitterBuffer` 등): test-support 게이트는 잡고 cfg(test)는 보지 않는다.
    assert_eq!(pick(false), names(&[("d", "support"), ("i", "support"), ("j", "support"), ("k", "support")]));
}

/// `band-dsp`로 옮기면 안 되는 노드(`DSP_EXCLUDED`)는 이 크레이트에 있고, 같은 이름의 모듈이 `band-dsp`에 생기지 않는다.
/// ⑩-0의 `dsp_planned_nodes_are_closed`(dsp 예정 노드에서 집합 밖으로 가는 간선 0)는 ⑩-1에서 컴파일러가 대신한다 —
/// `band-dsp`는 이 크레이트에 의존하지 않으므로 `sys`·`net::crypto` 등을 쓰면 컴파일이 안 된다.
#[test]
fn dsp_excluded_nodes_stay_in_band() {
    for n in DSP_EXCLUDED {
        assert!(NODES.iter().any(|(k, _)| k == n), "DSP_EXCLUDED의 노드가 NODES에 없음: {n}");
        let last = n.rsplit("::").next().unwrap_or(n);
        let moved = format!("{DSP_ROOT}::{last}");
        assert!(!DSP_NODES.iter().any(|(k, _)| *k == moved), "band-dsp에 넣으면 안 되는 노드가 생겼다(DSP_EXCLUDED): {moved}");
    }
}

/// ⑨b-1(6.3절): `band-dsp`의 공개 표면 = `band-dsp` 밖(이 크레이트·앱)이 쓰는 것. 여기서는 린트가 켜져 있는지와 모듈 단위를
/// 본다: 뿌리의 `pub mod`마다 `band-dsp` 밖 노드에서 들어오는 간선이 있다(아무도 안 쓰는 모듈은 `mod`로). 항목 단위는
/// `public_surface_is_fixed_and_used_outside`(⑨b-2, #165 리뷰 M1).
#[test]
fn dsp_public_surface_is_used_outside() {
    let lib = std::fs::read_to_string(dsp_src_dir().join("lib.rs")).expect("band-dsp lib.rs");
    assert!(lib.lines().any(|l| l.trim() == "#![warn(unreachable_pub)]"), "band-dsp lib.rs에 #![warn(unreachable_pub)]가 없다");
    let mods: Vec<&str> = lib.lines().filter_map(|l| l.trim().strip_prefix("pub mod ")?.strip_suffix(';')).collect();
    assert!(mods.len() >= DSP_NODES.len() / 2, "band-dsp pub mod 읽기: {mods:?}");
    let e = edges();
    let outside = |to: &str| e.keys().any(|(from, t)| !from.starts_with(DSP_ROOT) && *t == to);
    let unused: Vec<&str> = mods.iter().copied().filter(|m| !outside(&format!("{DSP_ROOT}::{m}"))).collect();
    assert!(unused.is_empty(), "band-dsp 밖이 쓰지 않는 pub mod — mod로 내릴 것: {unused:?}");
}

/// 공개 표면 목록 파일(⑨b-2). 크레이트마다 `tests/surface/<크레이트>.txt`, 한 줄에 `파일<TAB>종류<TAB>이름`.
fn surface_file(krate: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("surface").join(format!("{krate}.txt"))
}

/// 소스 디렉터리의 `pub` 선언(정확히 `pub` — `pub(crate)`·`pub(super)`는 뺀다): (파일, 종류, 이름). 종류는 `fn`·`struct`·
/// `enum`·`const`·`static`·`type`·`trait`·`mod`·`use`(다시 내보낸 이름, 글롭은 `경로::*`)·`field`(구조체 칸). 주석·문자열과
/// 인라인 시험 모듈(`#[cfg(test)] mod … { }`), 파일로 뗀 시험 모듈(`tests.rs`, `tests/`)은 보지 않는다. `skip`으로 시작하는
/// 상대 경로도 뺀다(`band-engine`의 `testing` — 그 목록은 `testing_surface_is_item_level`이 본다).
fn public_surface(dir: &Path, skip: &[&str]) -> BTreeSet<(String, String, String)> {
    public_surface_and_signatures(dir, skip).0
}

/// `public_surface`와, 그 `pub` 선언들의 시그니처(함수의 인자·반환·제약, 칸·상수·타입 별칭의 타입)에 나오는 식별자.
fn public_surface_and_signatures(dir: &Path, skip: &[&str]) -> (BTreeSet<(String, String, String)>, BTreeSet<String>) {
    let mut files = Vec::new();
    rs_files(dir, &mut files);
    let mut out = BTreeSet::new();
    let mut sig = BTreeSet::new();
    let mut add_sig = |text: &str| {
        for w in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if !w.is_empty() {
                sig.insert(w.to_string());
            }
        }
    };
    for f in files {
        let rel = f.strip_prefix(dir).unwrap().display().to_string().replace('\\', "/");
        if skip.iter().any(|s| rel.starts_with(s)) || rel.ends_with("/tests.rs") || rel.contains("/tests/") {
            continue;
        }
        let code = without_inline_test_mods(&std::fs::read_to_string(&f).unwrap());
        let mut lines = code.lines();
        while let Some(line) = lines.next() {
            let Some(rest) = line.trim().strip_prefix("pub ") else { continue };
            let mut rest = rest.to_string();
            for q in ["const fn ", "unsafe fn ", "async fn ", "unsafe impl "] {
                if let Some(r) = rest.strip_prefix(q) {
                    rest = format!("{}{r}", &q[q.len() - 3..]);
                }
            }
            let word = |s: &str| s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>();
            let mut push = |kind: &str, name: String| {
                assert!(!name.is_empty(), "{rel}: 이름 읽기 실패: {line}");
                out.insert((rel.clone(), kind.to_string(), name));
            };
            if let Some(tree) = rest.strip_prefix("use ") {
                let mut stmt = tree.to_string();
                while !stmt.contains(';') {
                    stmt.push(' ');
                    stmt.push_str(lines.next().expect("use 끝"));
                }
                for leaf in use_leaves(stmt.split(';').next().unwrap()) {
                    push("use", leaf);
                }
            } else if let Some(k) = ["fn ", "struct ", "enum ", "const ", "static ", "type ", "trait ", "mod ", "union "].iter().find(|k| rest.starts_with(**k)) {
                let name = word(&rest[k.len()..]);
                let after = &rest[k.len() + name.len()..];
                if *k == "fn " {
                    // 시그니처: 본문 `{`까지, 본문이 없으면 줄 끝 `;`까지(여러 줄). 배열 타입 `[u8; N]`의 `;`에서 끊지 않는다.
                    let mut s = after.to_string();
                    while !s.contains('{') && !s.trim_end().ends_with(';') {
                        let Some(n) = lines.next() else { break };
                        s.push(' ');
                        s.push_str(n);
                    }
                    add_sig(&without_param_names(s.split('{').next().unwrap()));
                } else if *k != "mod " {
                    add_sig(after);
                }
                // 들여 쓴 `fn`은 `impl`·`trait` 안의 메서드(인라인 모듈 안 자유 함수도 여기로 — 더 느슨한 쪽).
                let kind = if *k == "fn " && line.starts_with([' ', '\t']) { "method" } else { k.trim() };
                push(kind, name);
            } else if rest.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && word(&rest).len() < rest.len() && rest[word(&rest).len()..].trim_start().starts_with(':') {
                add_sig(&rest[word(&rest).len()..]);
                push("field", word(&rest));
            } else {
                panic!("{rel}: 읽지 못한 pub 선언(파서를 고칠 것): {line}");
            }
        }
    }
    (out, sig)
}

/// `use` 나무의 잎 이름(별칭이 있으면 별칭, 글롭은 `경로::*`, `self`는 앞 마디).
fn use_leaves(tree: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn go(t: &str, prefix: &str, out: &mut Vec<String>) {
        let t = t.trim();
        if let Some(open) = t.find('{') {
            let head = join(prefix, t[..open].trim().trim_end_matches("::"));
            let inner = &t[open + 1..t.rfind('}').unwrap_or(t.len())];
            let (mut depth, mut start) = (0, 0);
            for (i, c) in inner.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    ',' if depth == 0 => {
                        go(&inner[start..i], &head, out);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            go(&inner[start..], &head, out);
        } else if !t.is_empty() {
            let mut it = t.split(" as ");
            let path = join(prefix, it.next().unwrap().trim());
            let name = match it.next() {
                Some(a) => a.trim().to_string(),
                None if path.ends_with("::*") => path.clone(),
                None if path.ends_with("::self") => path.trim_end_matches("::self").rsplit("::").next().unwrap().to_string(),
                None => path.rsplit("::").next().unwrap().to_string(),
            };
            out.push(name);
        }
    }
    go(tree, "", &mut out);
    out
}

/// 함수 시그니처에서 인자 이름(`이름:` — `::`가 아닌 `:` 앞 단어)을 지운다. 타입만 남겨 `sig`에 인자 이름이 섞이지 않게
/// 한다(#169 리뷰 L2).
fn without_param_names(sig: &str) -> String {
    let b = sig.as_bytes();
    let mut out = String::with_capacity(sig.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphanumeric() || c == '_' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let mut j = i;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            let is_param = b.get(j) == Some(&b':') && b.get(j + 1) != Some(&b':') && (start == 0 || b[start - 1] != b':');
            if !is_param {
                out.push_str(&sig[start..i]);
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// 디렉터리들의 코드(주석·문자열 제외)에서 **경로·호출로** 쓰인 식별자: `::이름`, `이름(`(메서드 호출 `.이름(`·정의 `fn 이름(`
/// 제외), `use` 문 안의 이름. 지역 변수·인자처럼 같은 낱말이 우연히 나오는 것은 세지 않는다 — 최상위 `fn`의 "밖에서 씀"
/// 판정에 쓴다(#169 리뷰 L2).
fn path_or_call_identifiers(dirs: &[PathBuf], skip_file: &str) -> BTreeSet<String> {
    let mut files = Vec::new();
    for d in dirs {
        rs_files(d, &mut files);
    }
    let mut out = BTreeSet::new();
    for f in files.iter().filter(|f| !f.ends_with(skip_file)) {
        let code = strip(&std::fs::read_to_string(f).unwrap());
        let b = code.as_bytes();
        let mut in_use = false;
        let mut i = 0;
        while i < b.len() {
            let c = b[i] as char;
            if c == ';' {
                in_use = false;
            }
            if !(c.is_ascii_alphanumeric() || c == '_') {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let w = &code[start..i];
            if w == "use" {
                in_use = true;
                continue;
            }
            let before = code[..start].trim_end();
            let after = code[i..].trim_start();
            let by_path = before.ends_with("::");
            let by_call = after.starts_with('(') && !before.ends_with('.') && !before.ends_with("fn");
            if in_use || by_path || by_call {
                out.insert(w.to_string());
            }
        }
    }
    out
}

/// 디렉터리들의 코드(주석·문자열 제외)에 나오는 식별자 전부.
fn identifiers(dirs: &[PathBuf], skip_file: &str) -> BTreeSet<String> {
    let mut files = Vec::new();
    for d in dirs {
        rs_files(d, &mut files);
    }
    let mut out = BTreeSet::new();
    for f in files.iter().filter(|f| !f.ends_with(skip_file)) {
        let code = strip(&std::fs::read_to_string(f).unwrap());
        for w in code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if !w.is_empty() {
                out.insert(w.to_string());
            }
        }
    }
    out
}

/// 밖에서 이름을 부르지 않고 공개 시그니처에도 나오지 않지만 `pub`이어야 하는 항목: (크레이트, 이름, 이유). 공개 시그니처
/// (함수 인자·반환·제약, 칸·상수·별칭 타입)에 나오는 타입은 자동으로 통과한다 — 밖에서 그 값을 받거나 넘기므로 `pub(crate)`면
/// `private_interfaces`다. 열거형 변형의 값 타입이나 `pub(crate)` 타입 별칭(별칭은 `pub`이면 `unreachable_pub`)처럼 파서가
/// 시그니처로 읽지 않는 자리에만 나오는 것을 여기 적는다.
const SURFACE_SIG_ONLY: &[(&str, &str, &str)] = &[
    ("band-engine", "Ring", "`Timeline`(= `Ring<Sample>`, pub(crate) 별칭)으로 `testing::diag::timeline::to_jsonl`의 인자"),
    ("band-engine", "AutoPayload", "`AutoFn`(pub(crate) 별칭)의 반환으로 `NetShared::send_audio_timed`(시험 훅)의 인자"),
    // `tests/rt_alloc.rs`(#168)가 `MetroReport { applied_gen, running, ..Default::default() }`로 만든다 — 구조체 갱신 문법은 칸이
    // 모두 보여야 한다(E0451). 아래 셋은 그 문법 때문에만 `pub`이다.
    ("band-engine", "clock_rtt_ms", "`MetroReport` 칸 — `..Default::default()` 갱신 문법(tests/rt_alloc.rs)"),
    ("band-engine", "clock_extrapolation_ms", "`MetroReport` 칸 — `..Default::default()` 갱신 문법(tests/rt_alloc.rs)"),
    ("band-engine", "ref_clock_unsupported", "`MetroReport` 칸 — `..Default::default()` 갱신 문법(tests/rt_alloc.rs)"),
];

/// ⑨b-2(6.3절, #165 리뷰 M1): 두 크레이트의 공개 표면을 **항목 단위로** 고정한다.
/// 1. 실제 `pub` 선언 집합 = `tests/surface/<크레이트>.txt`(목록에 없는 `pub`을 더하거나 목록 항목을 내리면 실패 — 목록 diff가
///    리뷰에서 공개 표면 변화로 보인다). 다시 쓰기: `BAND_SURFACE_BLESS=1 cargo test -p band-engine --test architecture public_surface`.
/// 2. 목록의 이름마다 크레이트 밖(`band-dsp` → `band-engine`의 src·tests·examples와 앱 src, `band-engine` → 앱 src와 이 크레이트의
///    tests·examples)의 코드에 그 식별자가 한 번 이상 나온다(`SURFACE_SIG_ONLY` 제외). 밖에서 안 쓰는 `pub fn`을 더하면 1·2 둘 다로
///    실패한다. 최상위 `fn`은 경로·호출로 쓰여야 한다(`path_or_call_identifiers` — 같은 낱말의 지역 변수·인자로는 통과하지 않는다,
///    #169 리뷰 L2). 메서드·칸·타입은 이름 단위라 흔한 이름(`new`)은 2를 늘 통과한다 — 그 몫은 1(목록 diff를 리뷰가 본다)과 컴파일러다.
/// 3. 두 크레이트 모두 `unreachable_pub`가 켜져 있다(`band-dsp`는 늘, `band-engine`은 `test-support` 구성 — `lib.rs` 주석).
#[test]
fn public_surface_is_fixed_and_used_outside() {
    let engine_lib = std::fs::read_to_string(src_dir().join("lib.rs")).unwrap();
    assert!(
        engine_lib.lines().any(|l| l.trim() == "#![cfg_attr(feature = \"test-support\", warn(unreachable_pub))]"),
        "band-engine lib.rs에 unreachable_pub(test-support 구성)가 없다"
    );
    let me = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates: [(&str, PathBuf, &[&str], Vec<PathBuf>); 2] = [
        ("band-dsp", dsp_src_dir(), &[], vec![src_dir(), me.join("tests"), me.join("examples"), app_src_dir()]),
        ("band-engine", src_dir(), &["testing"], vec![me.join("tests"), me.join("examples"), app_src_dir()]),
    ];
    // 값이 정확히 `1`일 때만 다시 쓴다(셸에 남은 `=0`·빈 값으로 고정이 꺼지지 않게, #169 리뷰 L1).
    let bless = std::env::var("BAND_SURFACE_BLESS").as_deref() == Ok("1");
    let mut problems = Vec::new();
    for (krate, dir, skip, outside) in crates {
        let actual = public_surface(&dir, skip);
        let text: String = actual.iter().map(|(f, k, n)| format!("{f}\t{k}\t{n}\n")).collect();
        let path = surface_file(krate);
        if bless {
            let head = format!("# {krate} 공개 표면(⑨b-2). `tests/architecture.rs`의 `public_surface_is_fixed_and_used_outside`가 만든다 — 손으로 고치지 말 것.\n");
            let new = head + &text;
            let old = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
            if old != new {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &new).unwrap();
                // 다시 쓴 실행은 통과시키지 않는다 — diff를 보고 커밋한 뒤 BLESS 없이 다시 돌린다.
                problems.push(format!("{krate}: 목록을 다시 썼다 — `git diff {}`를 확인하고 커밋할 것", path.display()));
            }
        }
        let listed: BTreeSet<(String, String, String)> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let mut p = l.split('\t').map(str::to_string);
                (p.next().unwrap(), p.next().unwrap_or_default(), p.next().unwrap_or_default())
            })
            .collect();
        for e in actual.difference(&listed) {
            problems.push(format!("{krate}: 목록에 없는 pub(새 공개 표면 — 정말 밖에서 쓰면 목록을 다시 쓸 것): {e:?}"));
        }
        for e in listed.difference(&actual) {
            problems.push(format!("{krate}: 목록에만 있는 pub(좁혔으면 목록을 다시 쓸 것): {e:?}"));
        }
        let ids = identifiers(&outside, "architecture.rs");
        let calls = path_or_call_identifiers(&outside, "architecture.rs");
        let (_, sig) = public_surface_and_signatures(&dir, skip);
        for (f, k, n) in &actual {
            let name = n.rsplit("::").next().unwrap();
            // 최상위 `fn`은 경로·호출로 쓰여야 한다(같은 낱말의 지역 변수·인자로는 통과하지 않는다). 메서드·칸·타입은 이름 단위.
            let used = if k == "fn" { calls.contains(name) } else { ids.contains(name) };
            if name == "*" || used || sig.contains(name) || SURFACE_SIG_ONLY.iter().any(|(c, s, _)| *c == krate && s == n) {
                continue;
            }
            problems.push(format!("{krate}: 밖에서 이름을 부르지 않는 pub — pub(crate)로 내릴 것: {f} {k} {n}"));
        }
    }
    for (c, n, _) in SURFACE_SIG_ONLY {
        let dir = if *c == "band-dsp" { dsp_src_dir() } else { src_dir() };
        assert!(public_surface(&dir, &["testing"]).iter().any(|(_, _, x)| x == n), "SURFACE_SIG_ONLY의 {c} {n}이 공개 표면에 없다 — 지울 것");
    }
    assert!(problems.is_empty(), "공개 표면 래칫:\n{}", problems.join("\n"));
}

/// 파서 변이 확인: `pub` 종류마다 읽고, `pub(crate)`·시험 모듈·주석은 뺀다.
#[test]
fn public_surface_scanner_catches_variants() {
    let dir = std::env::temp_dir().join(format!("band-surface-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.rs"),
        "pub mod a;\npub(crate) fn hidden() {}\n// pub fn in_comment() {}\npub const fn k() {}\npub use x::{y as z, w::{self, v}, g::*};\n\
         pub struct S {\n    pub field_a: u8,\n    pub(crate) field_b: u8,\n}\n#[cfg(test)]\nmod tests {\n    pub fn in_test() {}\n}\npub trait T {}\n",
    )
    .unwrap();
    let got: Vec<String> = public_surface(&dir, &[]).into_iter().map(|(_, k, n)| format!("{k} {n}")).collect();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(got, ["field field_a", "fn k", "mod a", "struct S", "trait T", "use v", "use w", "use x::g::*", "use z"]);
}

/// ⑩-1: `band-dsp`의 외부 의존은 `DSP_EXTERN_ALLOWED`와 같다(7.2절). ⑩-0의 `dsp_planned_nodes_use_only_dsp_crates`(소스에서
/// `krate::` 사용을 찾던 검사)는 컴파일러가 대신한다 — 매니페스트에 없는 크레이트는 쓸 수 없다. 그래서 매니페스트를 고정한다:
/// 의존을 더하면(예: `parking_lot`·`rand`) 여기서 실패하고, 7.2절과 ADR 제안 3("스레드·I/O·OS API 없음")을 다시 봐야 한다.
/// 빌드 스크립트도 두지 않는다.
#[test]
fn dsp_crate_uses_only_dsp_deps() {
    let deps = extern_crates(&dsp_dir().join("Cargo.toml"));
    let allowed: BTreeSet<String> = DSP_EXTERN_ALLOWED.iter().map(|s| s.to_string()).collect();
    assert_eq!(deps, allowed, "band-dsp [dependencies]가 7.2절 목록(DSP_EXTERN_ALLOWED)과 다르다");
    assert!(!dsp_dir().join("build.rs").exists(), "band-dsp에 빌드 스크립트를 두지 않는다");
    // 읽기 확인: 이 크레이트의 의존에는 암호·OS 크레이트와 band-dsp가 있다.
    let band = extern_crates(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    for k in ["sha2", "rand", "parking_lot", "windows_sys", "x25519_dalek", "band_dsp"] {
        assert!(band.contains(k), "Cargo.toml 의존 읽기: {k} 없음 {band:?}");
    }
    // 점 표기 섹션도 읽는다(#160 리뷰 L3).
    let dotted = "[dependencies]\nlog = \"0.4\"\n[dependencies.parking_lot]\nversion = \"0.12\"\n\
                  [target.'cfg(windows)'.dependencies.windows-sys]\nversion = \"0.59\"\n[dev-dependencies.rand]\nversion = \"0.8\"\n\
                  [target.'cfg(windows)'.dev-dependencies.sha2]\nversion = \"0.10\"\n[build-dependencies]\ncc = \"1\"\n";
    let got = extern_crates_of(dotted);
    let want: BTreeSet<String> = ["log", "parking_lot", "windows_sys"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want, "점 표기 의존");
}

/// 앱(`band`)의 `[dependencies]`(7.2절 그래프, ⑩-2): Tauri와 플러그인, 직렬화·로그, 아래 두 크레이트뿐. 장치·네트워크·암호
/// 크레이트(`cpal`·`windows`·`socket2`…)가 앱에 다시 생기면 엔진 코드가 앱으로 새고 있다는 뜻이다.
const APP_EXTERN_ALLOWED: &[&str] = &[
    "tauri",
    "tauri_plugin_shell",
    "tauri_plugin_log",
    "tauri_plugin_process",
    "tauri_plugin_dialog",
    "tauri_plugin_updater",
    "serde",
    "serde_json",
    "log",
    "band_engine",
    "band_dsp",
];

/// 장치 백엔드 피처 하나(7.4절).
struct Backend {
    /// 피처 이름.
    feature: &'static str,
    /// 그 피처가 켜는 선택 의존(크레이트 경로 첫 마디와 같다).
    dep: &'static str,
    /// 그 크레이트 경로를 쓸 수 있는 파일(`src` 기준 접두).
    allow: &'static str,
    /// 모듈 선언 파일과 그 안의 선언들 — 바로 앞 줄이 `gate`여야 한다.
    decl_file: &'static str,
    decls: &'static [&'static str],
    gate: &'static str,
}

const BACKENDS: &[Backend] = &[
    Backend {
        feature: "backend-cpal",
        dep: "cpal",
        allow: "engine/device/cpal.rs",
        decl_file: "engine/device/mod.rs",
        decls: &["pub(crate) mod cpal;", "pub mod select;"],
        gate: "#[cfg(feature = \"backend-cpal\")]",
    },
    Backend {
        feature: "backend-wasapi",
        dep: "windows",
        allow: "engine/wasapi/",
        decl_file: "engine/mod.rs",
        decls: &["pub(crate) mod wasapi;"],
        gate: "#[cfg(all(windows, feature = \"backend-wasapi\"))]",
    },
];

/// `krate::` 경로(앞이 식별자·`:`가 아닌 것 — `std::os::windows::`·`super::cpal::`·`windows_sys::`는 아니다)를 쓰는지.
fn uses_crate_path(code: &str, krate: &str) -> bool {
    let kw = format!("{krate}::");
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(i) = code[from..].find(kw.as_str()) {
        let s = from + i;
        from = s + kw.len();
        if s == 0 || !(b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'_' || b[s - 1] == b':') {
            return true;
        }
    }
    false
}

/// ⑩-2: 크레이트 사이 외부 의존 배치(7.2절 표)와 백엔드 피처(7.4절).
/// - 앱 `[dependencies]` = `APP_EXTERN_ALLOWED`, 앱에는 빌드 스크립트(`tauri-build`)가 있다.
/// - `band-engine`은 tauri·플러그인에 의존하지 않고 빌드 스크립트가 없다(`tauri-build`의 프런트 `dist` 요구가 하네스·시험에 오지 않게).
/// - `cpal`·`windows`는 `band-engine`의 선택 의존이고 각각 `backend-cpal`·`backend-wasapi`가 켠다. 기본 피처는 둘 다.
/// - 그 크레이트 경로는 백엔드 파일에서만 쓰고, 그 모듈 선언은 피처 게이트 뒤에 있다. 게이트가 빠지면 CI `Clippy (band-engine,
///   백엔드 없음)`이 컴파일 오류로도 잡지만, 여기서는 어느 줄인지 바로 말해 준다.
#[test]
fn engine_crate_deps_follow_graph() {
    let app = extern_crates(&ws_dir().join("Cargo.toml"));
    let allowed: BTreeSet<String> = APP_EXTERN_ALLOWED.iter().map(|s| s.to_string()).collect();
    assert_eq!(app, allowed, "앱 [dependencies]가 7.2절(APP_EXTERN_ALLOWED)과 다르다 — 엔진 의존은 band-engine으로");
    assert!(ws_dir().join("build.rs").exists(), "앱 빌드 스크립트(tauri-build)");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let engine = extern_crates(&manifest);
    assert!(!engine.iter().any(|d| d.starts_with("tauri")), "band-engine이 tauri에 의존한다: {engine:?}");
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs").exists(), "band-engine에 빌드 스크립트를 두지 않는다");
    let toml = std::fs::read_to_string(&manifest).expect("Cargo.toml");
    let line = |key: &str| toml.lines().map(str::trim).find(|l| l.starts_with(&format!("{key} ="))).unwrap_or_else(|| panic!("band-engine Cargo.toml: {key} 없음"));
    let default = line("default");
    for Backend { feature, dep, allow, decl_file, decls, gate } in BACKENDS {
        assert!(engine.contains(*dep), "band-engine 의존에 {dep} 없음");
        assert!(line(dep).contains("optional = true"), "{dep}는 선택 의존이어야 한다(백엔드 피처 뒤): {}", line(dep));
        assert!(line(feature).contains(&format!("\"dep:{dep}\"")), "{feature}가 dep:{dep}를 켜지 않는다: {}", line(feature));
        assert!(default.contains(&format!("\"{feature}\"")), "기본 피처에 {feature} 없음(앱은 기본으로 켠다): {default}");
        assert!(!app.contains(*dep), "앱이 {dep}에 직접 의존한다");
        // 크레이트 경로를 쓰는 파일
        let mut files = Vec::new();
        rs_files(&src_dir(), &mut files);
        let users: Vec<String> = files
            .iter()
            .filter(|f| uses_crate_path(&strip(&std::fs::read_to_string(f).expect("읽기")), dep))
            .map(|f| rel_of(f))
            .collect();
        assert!(!users.is_empty(), "{dep}:: 사용처를 찾지 못했다(검사기 확인)");
        let outside: Vec<&String> = users.iter().filter(|r| !r.starts_with(allow)).collect();
        assert!(outside.is_empty(), "{dep}::를 백엔드 파일({allow}) 밖에서 쓴다 — {feature} 없이 컴파일되지 않는다: {outside:?}");
        // 모듈 선언의 게이트(바로 앞 비어 있지 않은 줄)
        let text = std::fs::read_to_string(src_file(decl_file)).expect("읽기");
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for d in *decls {
            let i = lines.iter().position(|l| l == d).unwrap_or_else(|| panic!("{decl_file}: `{d}` 없음"));
            let prev = lines[..i].iter().rev().find(|p| !p.is_empty()).copied().unwrap_or("");
            assert_eq!(prev, *gate, "{decl_file}: `{d}`는 {feature} 게이트 뒤에 있어야 한다");
        }
    }
    assert!(line("asio").contains("\"backend-cpal\"") && line("asio").contains("\"band-dsp/asio\""), "asio는 cpal 백엔드와 band-dsp/asio를 켠다");
    // 앱은 asio를 엔진으로 넘긴다
    let app_toml = std::fs::read_to_string(ws_dir().join("Cargo.toml")).expect("앱 Cargo.toml");
    let app_asio = app_toml.lines().map(str::trim).find(|l| l.starts_with("asio =")).expect("앱 asio 피처");
    assert!(app_asio.contains("\"band-engine/asio\""), "앱 asio가 band-engine/asio를 켜지 않는다: {app_asio}");
    // 변이: 크레이트 경로만 잡는다.
    assert!(uses_crate_path("use cpal::traits::DeviceTrait;", "cpal"));
    assert!(uses_crate_path("fn f() { windows::Win32::Foundation::HANDLE(0); }", "windows"));
    assert!(!uses_crate_path("use super::cpal::CpalBackend;", "cpal"));
    assert!(!uses_crate_path("use std::os::windows::io::AsRawSocket;", "windows"));
    assert!(!uses_crate_path("use windows_sys::Win32::Foundation;", "windows"));
}

/// `band-dsp` 정의(ADR 제안 3)의 std 쪽: 스레드를 만들지 않고, 파일·소켓·프로세스·환경을 쓰지 않고, 벽시계를 읽지 않는다
/// (#160 리뷰 L1 — 외부 크레이트·`band` 모듈은 컴파일러가 막지만 std는 막지 않는다). RT 검사의 `시스템콜` 문자열 목록에
/// `Instant::now`를 더해 비시험 코드(파일 끝 단위 시험·`tests.rs` 제외, 주석·문자열 제외)에서 찾는다. `Instant` 타입 자체는
/// 허용한다(`relay_codec`이 인자로 받기만 한다). 예외는 `DSP_STD_ALLOWED`뿐.
const DSP_STD_ALLOWED: &[(&str, &str, &str)] = &[(
    "stats.rs",
    "std::thread::yield_now()",
    "SeqCell::read — 비RT 독자의 seqlock 재시도 양보. 스레드 생성·I/O·시계가 아니다(7.3절)",
)];

fn dsp_std_violations(rel: &str, code: &str) -> Vec<String> {
    let mut flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    for (f, ok, _) in DSP_STD_ALLOWED {
        if *f == rel {
            flat = flat.replace(ok, "");
        }
    }
    let syscalls = RT_FORBIDDEN.iter().find(|(k, _)| *k == "시스템콜").map(|(_, p)| *p).expect("RT_FORBIDDEN 시스템콜");
    syscalls.iter().copied().chain(["Instant::now"]).filter(|p| flat.contains(p)).map(|p| format!("  {rel}: {p}")).collect()
}

#[test]
fn dsp_crate_has_no_thread_io_or_clock() {
    let mut bad = Vec::new();
    let mut seen = 0;
    let mut used_allow = BTreeSet::new();
    for (f, module) in source_files() {
        if module.first().is_none_or(|m| m != DSP_ROOT) || module.last().is_some_and(|m| m == "tests") {
            continue;
        }
        seen += 1;
        let rel = f.strip_prefix(dsp_src_dir()).unwrap_or(&f).display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&f).expect("읽기");
        let code = strip(without_unit_tests(&text));
        for (af, ok, _) in DSP_STD_ALLOWED {
            if *af == rel && code.chars().filter(|c| !c.is_whitespace()).collect::<String>().contains(ok) {
                used_allow.insert(*ok);
            }
        }
        bad.extend(dsp_std_violations(&rel, &code));
    }
    assert!(seen > DSP_NODES.len(), "band-dsp 파일을 다 보지 못했다: {seen}");
    assert!(bad.is_empty(), "band-dsp가 스레드·I/O·벽시계를 쓴다(ADR 제안 3 — band로 옮길 것):\n{}", bad.join("\n"));
    for (f, ok, _) in DSP_STD_ALLOWED {
        assert!(used_allow.contains(ok), "DSP_STD_ALLOWED의 {f}: {ok}가 없어졌다 — 목록에서 지울 것");
    }
    // 변이: 스레드 생성·벽시계·파일·네트워크는 잡고, 허용 예외·`Instant` 타입 인자는 잡지 않는다.
    assert_eq!(dsp_std_violations("x.rs", "fn a() { std::thread::spawn(|| {}); }"), ["  x.rs: thread::"]);
    assert_eq!(dsp_std_violations("x.rs", "fn a() -> u64 { let t = Instant::now(); 0 }"), ["  x.rs: Instant::now"]);
    assert_eq!(dsp_std_violations("x.rs", "fn a() { let _ = SystemTime::UNIX_EPOCH; }"), ["  x.rs: SystemTime"]);
    assert_eq!(dsp_std_violations("x.rs", "fn a() { let _ = std::fs::File::open(p); }"), ["  x.rs: std::fs", "  x.rs: File::"]);
    assert_eq!(dsp_std_violations("x.rs", "use std::net::UdpSocket;"), ["  x.rs: std::net"]);
    assert_eq!(dsp_std_violations("stats.rs", "fn r() { std::thread::yield_now(); }"), Vec::<String>::new());
    assert_eq!(dsp_std_violations("mixer.rs", "fn r() { std::thread::yield_now(); }"), ["  mixer.rs: thread::"], "허용은 그 파일에서만");
    assert!(dsp_std_violations("x.rs", "fn update(now: Instant) {}").is_empty());
}

/// 워커 단독 소유 미디어 상태 타입(엔진 ③). 락 안에 들어가면 안 된다. `PeerMediaShared`(원자값만)는 다른 이름이라 해당 없음.
const WORKER_OWNED: &[&str] = &["JitterBuffer", "PeerMedia"];
/// 락 타입 이름(`parking_lot`·`std` 모두 같은 이름).
const LOCK_TYPES: &[&str] = &["Mutex", "RwLock", "ReentrantMutex", "FairMutex"];
/// SPSC 링 끝점: 그 안의 값은 공유가 아니라 소유 이전이다(예: 제어 평면의 반환 큐 `Mutex<HeapCons<Box<PeerMedia>>>` — 워커는
/// 생산 끝만 갖고 소비 끝을 잡지 않는다). 락 인자가 이것을 거쳐 이름에 닿으면 세지 않는다.
const QUEUE_ENDPOINTS: &[&str] = &["HeapCons", "HeapProd", "CachingCons", "CachingProd"];

/// 식별자와 구두점 한 글자씩으로 자른다(공백 무시). 경로(`a::b::C`)는 `a` `:` `:` `b` … 로 나온다.
fn tokens(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in code.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
            continue;
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !ch.is_whitespace() {
            out.push(ch.to_string());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `names`를 가리키는 별칭을 `names`에 더한다: `type X = …name…;`, `use …::name as X;`. 한 번에 한 단계씩, 더 늘지 않을 때까지.
fn add_aliases(files: &[Vec<String>], names: &mut BTreeSet<String>) {
    loop {
        let before = names.len();
        for t in files {
            for i in 0..t.len() {
                if t[i] == "type" && i + 2 < t.len() {
                    let alias = &t[i + 1];
                    if let Some(end) = t[i..].iter().position(|x| x == ";") {
                        let rhs = &t[i + 2..i + end];
                        if rhs.iter().any(|x| names.contains(x)) {
                            names.insert(alias.clone());
                        }
                    }
                }
                if t[i] == "as" && i > 0 && i + 1 < t.len() && names.contains(&t[i - 1]) {
                    names.insert(t[i + 1].clone());
                }
            }
        }
        if names.len() == before {
            return;
        }
    }
}

/// 락 타입 인자(`Mutex<…>`·`Mutex::<…>`, 경로 한정 포함)에 `names`가 나오는 곳.
fn locked_names(t: &[String], locks: &BTreeSet<String>, names: &BTreeSet<String>) -> Vec<String> {
    // 꺾쇠 짝 찾기: `->`의 `>`는 세지 않는다(`fn() -> T`, #124 리뷰 L-3-4).
    let closes = |k: usize| t[k] == ">" && !(k > 0 && t[k - 1] == "-");
    let mut hits = Vec::new();
    for i in 0..t.len() {
        if !locks.contains(&t[i]) {
            continue;
        }
        let mut j = i + 1;
        if t.get(j).map(String::as_str) == Some(":") && t.get(j + 1).map(String::as_str) == Some(":") {
            j += 2;
        }
        if t.get(j).map(String::as_str) != Some("<") {
            continue;
        }
        let mut depth = 0;
        let mut k = j;
        while k < t.len() {
            match t[k].as_str() {
                "<" => depth += 1,
                ">" if closes(k) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                ";" | "{" => break,
                // 링 끝점은 그 꺾쇠 구간만 건너뛴다(나머지 인자는 계속 본다, #124 리뷰 L-3-2)
                x if QUEUE_ENDPOINTS.contains(&x) && t.get(k + 1).map(String::as_str) == Some("<") => {
                    let mut d = 0;
                    k += 1;
                    while k < t.len() {
                        if t[k] == "<" {
                            d += 1;
                        } else if closes(k) {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        k += 1;
                    }
                }
                x if names.contains(x) => hits.push(format!("{}<…{x}…>", t[i])),
                _ => {}
            }
            k += 1;
        }
    }
    hits
}

fn worker_owned_names() -> BTreeSet<String> {
    WORKER_OWNED.iter().map(|s| s.to_string()).collect()
}

/// 락 타입 이름과 그 별칭(`use parking_lot::Mutex as PlMutex`, `type Guarded<T> = Mutex<T>`, #124 리뷰 L-3-1).
fn lock_names(files: &[Vec<String>]) -> BTreeSet<String> {
    let mut locks: BTreeSet<String> = LOCK_TYPES.iter().map(|s| s.to_string()).collect();
    add_aliases(files, &mut locks);
    locks
}

/// 지터 버퍼·피어 미디어를 락으로 감싸지 않는다(엔진 ③-a3): `JitterBuffer`·`PeerMedia`를 만지는 스레드는 워커뿐이다
/// (생성·해제는 제어 평면). 누가 다시 넣으면 기본 우선순위 스레드와 워커의 우선순위 역전 경로가 돌아온다.
/// 경로 한정(`band_dsp::jitter::JitterBuffer`), 별칭(`type`·`use … as`, 파일을 넘어서도), 중첩(`Arc<Mutex<Box<PeerMedia>>>`)을
/// 잡는다(#118 리뷰 L-1), 락 이름의 별칭(`use … Mutex as X`, `type G<T> = Mutex<T>`)과 링 끝점 뒤 인자도(#124 리뷰 L-3).
/// 한계: 이름이 락 인자에 **직접** 나와야 한다 — 감싼 구조체(`struct H { jb: JitterBuffer }` + `Mutex<H>`)·명령 열거형
/// (`Mutex<Vec<MediaCmd>>`)은 못 잡는다. 또 `#[cfg(test)] mod tests` 뒤의 코드는 보지 않는다(테스트 모듈은 파일 끝이 관례).
/// 주석·문자열·단위 테스트는 보지 않는다. 검사기 자체의 음성 확인은 `locked_media_scanner_catches_variants`.
#[test]
fn no_locked_jitter_buffer() {
    let files: Vec<(PathBuf, Vec<String>)> = source_files()
        .into_iter()
        .map(|(f, _)| {
            let src = std::fs::read_to_string(&f).unwrap();
            let t = tokens(&strip(without_unit_tests(&src)));
            (f, t)
        })
        .collect();
    let all: Vec<Vec<String>> = files.iter().map(|(_, t)| t.clone()).collect();
    let mut names = worker_owned_names();
    add_aliases(&all, &mut names);
    let locks = lock_names(&all);
    let mut bad = Vec::new();
    for (file, t) in &files {
        for h in locked_names(t, &locks, &names) {
            bad.push(format!("  {}: {h}", file.display()));
        }
    }
    assert!(bad.is_empty(), "워커 단독 소유 미디어 상태를 락으로 감쌌다(엔진 ③, engine-stage3-design.md 7.1절):\n{}", bad.join("\n"));
}

/// 검사기 음성 확인(#118 리뷰 L-1의 표): 잡아야 할 것은 잡고, 원자값 공유 상태(`PeerMediaShared`)는 통과시킨다.
#[test]
fn locked_media_scanner_catches_variants() {
    let scan = |code: &str| {
        let t = tokens(&strip(code));
        let mut names = worker_owned_names();
        add_aliases(std::slice::from_ref(&t), &mut names);
        locked_names(&t, &lock_names(std::slice::from_ref(&t)), &names)
    };
    for bad in [
        "struct A { j: parking_lot::Mutex<JitterBuffer> }",
        "struct A { j: parking_lot::Mutex<band_dsp::jitter::JitterBuffer> }",
        "struct A { m: Arc<parking_lot::Mutex<PeerMedia>> }",
        "struct A { m: Mutex<Vec<Box<PeerMedia>>> }",
        "type Jb = JitterBuffer; struct A { j: Mutex<Jb> }",
        "type Jb = jitter::JitterBuffer; type Jb2 = Jb; struct A { j: RwLock<Option<Jb2>> }",
        "use band_dsp::jitter::JitterBuffer as Jb; struct A { j: std::sync::RwLock<Jb> }",
        "fn f() { let m = Mutex::<PeerMedia>::new(x); }",
        // #124 리뷰 L-3: 락 이름 별칭, 끝점 뒤의 인자, `->`
        "use parking_lot::Mutex as PlMutex; struct A { j: PlMutex<Option<JitterBuffer>> }",
        "type Guarded<T> = Mutex<T>; struct A { m: Guarded<PeerMedia> }",
        "struct A { m: Mutex<(HeapCons<u8>, Box<PeerMedia>)> }",
        "struct A { m: Mutex<(fn() -> u8, JitterBuffer)> }",
        "struct A { m: tokio::sync::Mutex<JitterBuffer> }",
    ] {
        assert!(!scan(bad).is_empty(), "못 잡음: {bad}");
    }
    for ok in [
        "struct E { media: Mutex<Vec<Arc<PeerMediaShared>>> }",
        "struct W { media: Vec<Box<PeerMedia>>, jb: JitterBuffer }",
        "// Mutex<JitterBuffer>\nstruct A;",
        "fn f() { log(\"Mutex<PeerMedia>\"); }",
        "struct A { m: Mutex<u32> } fn g(p: &PeerMedia) {}",
        "struct P { ret: Mutex<HeapCons<Box<PeerMedia>>> }",
    ] {
        assert!(scan(ok).is_empty(), "잘못 잡음: {ok} → {:?}", scan(ok));
    }
}

/// 함수 본문들(이름, 공백을 지운 본문). 주석·문자열을 지운 코드에서 `fn 이름 … {` 뒤 중괄호 짝까지. 안쪽 함수는 따로도 나온다.
fn fn_bodies(code: &str) -> Vec<(String, String)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = code[i..].find("fn ") {
        let at = i + off;
        i = at + 3;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        let name: String = code[i..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if name.is_empty() {
            continue;
        }
        // 본문 시작: 괄호 밖의 다음 `{`(그 전에 괄호 밖 `;`가 오면 선언뿐 — `[u8; N]` 같은 인자 타입의 `;`는 건너뛴다)
        let mut paren = 0i32;
        let Some(open) = (i..b.len()).find(|&k| {
            match b[k] {
                b'(' | b'[' => paren += 1,
                b')' | b']' => paren -= 1,
                _ => {}
            }
            paren == 0 && (b[k] == b'{' || b[k] == b';')
        }) else {
            break;
        };
        if b[open] == b';' {
            continue;
        }
        let mut depth = 0;
        let mut end = open;
        for (k, &c) in b.iter().enumerate().skip(open) {
            if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = k;
                    break;
                }
            }
        }
        out.push((name, code[open..=end].chars().filter(|c| !c.is_whitespace()).collect()));
    }
    out
}

/// `Peer`의 `conn`·`echo` 락을 직접 잡는 함수(가드 생성자와 읽기 전용만). 상태를 바꾸는 곳은 모두 `conn()`·`echo()` 가드를
/// 거쳐야 송신 경로 스냅샷이 발행된다(엔진 ③c). 여기에 없는 함수가 직접 잡으면 실패한다 — 새로 넣으려면 읽기 전용인지
/// 확인하고 이 목록에 더한다.
const PEER_DIRECT_LOCK_ALLOWED: &[&str] = &["conn", "echo", "send_route", "net_stats", "hello_targets"];
const PEER_DIRECT_LOCKS: &[&str] = &[".conn.lock(", ".conn.try_lock(", ".echo.lock(", ".echo.try_lock("];

fn peer_direct_lock_violations(code: &str) -> Vec<String> {
    fn_bodies(code)
        .into_iter()
        .filter(|(name, body)| !PEER_DIRECT_LOCK_ALLOWED.contains(&name.as_str()) && PEER_DIRECT_LOCKS.iter().any(|l| body.contains(l)))
        .map(|(name, _)| name)
        .collect()
}

/// 워커 송신 경로 함수(엔진 ③c): 본문에 락 획득이 없다(`refresh_copy`의 `try_read`만 — 기다리지 않음).
const SEND_PATH_FNS: &[&str] = &["send_audio_timed", "send_audio_to", "send_sealed", "refresh_copy", "relay_audio_load"];
const LOCK_CALLS: &[&str] = &[".lock(", ".read(", ".write(", ".try_lock(", ".try_write("];

fn send_path_lock_violations(code: &str) -> Vec<String> {
    let bodies = fn_bodies(code);
    let mut bad = Vec::new();
    for f in SEND_PATH_FNS {
        // 여러 파일을 이어 보므로(⑥-6) 같은 이름이 둘이면 어느 본문을 봤는지 모른다 — 정확히 하나여야 한다(#153 리뷰 L1).
        let found: Vec<&String> = bodies.iter().filter(|(n, _)| n == f).map(|(_, b)| b).collect();
        let [body] = found[..] else {
            bad.push(if found.is_empty() {
                format!("{f}: 함수를 못 찾음(이름이 바뀌었으면 SEND_PATH_FNS를 고칠 것)")
            } else {
                format!("{f}: 같은 이름 함수가 {}개(검사 대상이 모호하다 — 이름을 바꿀 것)", found.len())
            });
            continue;
        };
        for l in LOCK_CALLS {
            if body.contains(l) {
                bad.push(format!("{f}: {l}"));
            }
        }
    }
    bad
}

/// 엔진 ③c(#126 리뷰 M-3): (1) `net/peer/`에서 `conn`·`echo` 락을 직접 잡는 함수는 허용 목록뿐(상태 변경은 스냅샷을
/// 발행하는 가드로만), (2) 워커 송신 경로 함수에 락 획득이 없다. 단위 시험 코드는 보지 않는다.
#[test]
fn send_path_takes_no_locks() {
    let read = |rel: &str| {
        let src = std::fs::read_to_string(src_dir().join(rel)).unwrap();
        strip(without_unit_tests(&src))
    };
    // `Peer`와 가드는 `net/peer/mod.rs`, 상태기계·에코 상태는 형제 파일에 있다(엔진 R-1 ⑥-7). 디렉터리의 모든 파일을 이어 본다 —
    // 가드 생성자(`conn`·`echo`)를 못 찾으면 실패하므로 파일을 옮겨도 검사가 빠지지 않는다.
    let mut files = Vec::new();
    rs_files(&src_dir().join("net/peer"), &mut files);
    files.sort();
    let code: String = files.iter().map(|f| strip(without_unit_tests(&std::fs::read_to_string(f).unwrap())) + "\n").collect();
    let names: Vec<String> = fn_bodies(&code).into_iter().map(|(n, _)| n).collect();
    // 허용 목록의 이름은 모두 정확히 하나여야 한다(#155 리뷰 L1): 형제 파일에 같은 이름의 함수가 생기면 그 함수의 직접 락이
    // 이름 허용으로 통과하기 때문이다. 가드 생성자(`conn`·`echo`)를 못 찾으면 실패하는 것도 여기서 함께 지킨다.
    for g in PEER_DIRECT_LOCK_ALLOWED {
        assert_eq!(names.iter().filter(|n| n == g).count(), 1, "net/peer/에서 허용 목록 함수 `{g}`를 정확히 하나 찾아야 한다");
    }
    let peer = peer_direct_lock_violations(&code);
    assert!(peer.is_empty(), "net/peer/에서 가드 없이 conn·echo 락을 잡는 함수(스냅샷이 발행되지 않는다): {peer:?}");
    // 송신 경로 함수는 `net/send.rs`, 피어 표 복사본(`refresh_copy`)은 `net/mod.rs`에 있다(엔진 R-1 ⑥-6). 둘을 이어 본다 —
    // 함수를 못 찾으면 실패하므로 파일을 옮겨도 검사가 빠지지 않는다.
    let send = send_path_lock_violations(&(read("net/mod.rs") + "\n" + &read("net/send.rs")));
    assert!(send.is_empty(), "워커 송신 경로에 락 획득이 있다: {send:?}");
}

/// 위 검사기의 음성 확인.
#[test]
fn send_path_scanner_catches_variants() {
    let peer = "impl Peer { fn conn(&self) -> G { G { g: self.conn.lock() } } \
                fn bad(&self) { self.conn.lock().state = X; } \
                fn net_stats(&self) { let c = self.conn.lock(); } \
                fn worse(&self) { let mut e = self.echo.try_lock(); } }";
    assert_eq!(peer_direct_lock_violations(&strip(peer)), ["bad", "worse"]);
    let send = "fn send_audio_timed(&self) { let p = self.peers.read(); } fn send_audio_to(&self) { } \
                fn send_sealed(&self) { } fn refresh_copy(&self) { let p = self.peers.try_read(); } \
                fn relay_audio_load(&self) { self.relay.lock(); }";
    assert_eq!(send_path_lock_violations(&strip(send)), ["send_audio_timed: .read(", "relay_audio_load: .lock("]);
    assert_eq!(send_path_lock_violations("fn x() {}").len(), SEND_PATH_FNS.len(), "함수를 못 찾으면 실패");
    // 같은 이름이 둘(앞 것은 깨끗, 뒤 것이 락)이면 앞 것만 보고 통과하지 않는다(#153 리뷰 L1)
    let twice = format!("{send} fn send_sealed(&self) {{ self.relay.lock(); }}");
    assert!(send_path_lock_violations(&strip(&twice)).iter().any(|v| v.starts_with("send_sealed: 같은 이름 함수가 2개")));
}

/// 워커가 **기다리는** 락 호출(엔진 ②b-6b, 설계 7.1절, #143 리뷰 M-1). 공백을 지운 본문에서 찾는 문자열이다.
/// - 메서드 꼴: `.lock(`·`.read(`·`.write(`와 parking_lot의 다른 기다리는 입구(`lock_arc`, `read_recursive`,
///   `upgradable_read`, `read_arc`·`write_arc`, `Condvar`의 `wait`·`wait_while`).
/// - 시한 대기: `_for(`·`_until(`로 끝나는 이름(`try_lock_for`, `try_read_until`, `wait_for` …) — 이름은 try여도 시한만큼 기다린다.
/// - UFCS: `::lock(`·`::read(`·`::write(`(`Mutex::lock(&m)`, `RwLock::read(&l)`).
///
/// `try_lock(`·`try_read(`·`try_write(`는 기다리지 않으므로 걸리지 않는다(`.try_lock(`에는 `.lock(`이 없다).
/// `.read(`는 seqlock 칸의 `SeqCell::read`·`MediaStatsCell::read`도 잡는다 — 거짓 양성이 아니다: 그 둘은 재시도하며
/// `yield_now`를 부르는 **비RT 독자 전용**이고 워커는 그 칸의 작성자다(읽어야 하면 `try_read`). 진단 래퍼
/// `LockWait::lock(..)`도 같은 문자열이라 함께 걸린다.
const WAITING_LOCK_CALLS: &[&str] = &[
    ".lock(",
    ".read(",
    ".write(",
    ".lock_arc(",
    ".read_recursive(",
    ".upgradable_read(",
    ".read_arc(",
    ".write_arc(",
    ".wait(",
    ".wait_while(",
    "_for(",
    "_until(",
    "::lock(",
    "::read(",
    "::write(",
];

/// 주석·문자열을 지운 본문에서 기다리는 락 호출을 찾는다(공백 무시).
fn waiting_lock_calls(code: &str) -> Vec<&'static str> {
    let flat: String = strip(code).chars().filter(|c| !c.is_whitespace()).collect();
    WAITING_LOCK_CALLS.iter().copied().filter(|l| flat.contains(l)).collect()
}

/// 엔진 ②b 완료 조건(설계 7.1절·8절): `src/engine/worker/`(tests.rs 제외) 안에 기다리는 락 호출이 없다. 예외 0.
/// 단위 테스트 모듈은 보지 않으므로, 그 **뒤**에 코드를 숨길 수 없게 테스트 모듈이 파일 끝임도 확인한다.
/// **재발 방지용**이다 — 워커 파일이 직접 쓰는 락만 본다. 워커가 부르는 메서드 안의 락(`Peer.clock` 등)은 폭주 시험의
/// 락 대기 계측(`send_path_storm_measure`)과 `engine-architecture.md` 4.1절 표가 맡는다.
#[test]
fn worker_dir_waits_for_no_lock() {
    let dir = src_dir().join("engine/worker");
    let mut files = Vec::new();
    rs_files(&dir, &mut files);
    assert!(files.len() >= 10, "워커 파일을 못 찾음: {files:?}");
    let mut bad = Vec::new();
    for f in files.iter().filter(|f| f.file_name().is_some_and(|n| n != "tests.rs")) {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(f).unwrap();
        if !unit_tests_at_end(&src) {
            bad.push(format!("{name}: 단위 테스트 모듈 뒤에 코드가 있다(검사가 보지 못한다)"));
        }
        for l in waiting_lock_calls(without_unit_tests(&src)) {
            bad.push(format!("{name}: {l}"));
        }
    }
    assert!(bad.is_empty(), "워커 디렉터리에 기다리는 락 호출이 있다(원자값·큐·try_lock으로 바꿀 것): {bad:?}");
}

/// 위 검사기의 음성 확인.
#[test]
fn worker_lock_scanner_catches_variants() {
    let hit = |code: &str| waiting_lock_calls(&format!("fn a(&self) {{ {code} }}"));
    assert_eq!(hit("let p = self.ctl.plan.lock();"), [".lock("]);
    assert_eq!(hit("let p = self.net.peers\n    .read();"), [".read("]);
    assert_eq!(hit("*self.x . write() = 1; let s = cell.read();"), [".read(", ".write("]);
    assert_eq!(hit("let g = w.lock(&self.m, site);"), [".lock("], "진단 래퍼 LockWait::lock");
    // parking_lot의 다른 기다리는 입구(#143 리뷰 M-1)
    assert_eq!(hit("let g = parking_lot::Mutex::lock(&stats.m);"), ["::lock("]);
    assert_eq!(hit("let g = RwLock::read(&net.peers); let h = RwLock :: write(&net.peers);"), ["::read(", "::write("]);
    assert_eq!(hit("let g = stats.m.try_lock_for(Duration::from_secs(1));"), ["_for("]);
    assert_eq!(hit("let g = net.peers.try_read_until(deadline);"), ["_until("]);
    assert_eq!(hit("let g = net.peers.try_write_for(d);"), ["_for("]);
    assert_eq!(hit("let g = net.peers.read_recursive();"), [".read_recursive("]);
    assert_eq!(hit("let g = net.peers.upgradable_read();"), [".upgradable_read("]);
    assert_eq!(hit("let g = stats.m.lock_arc();"), [".lock_arc("]);
    assert_eq!(hit("let g = peers.read_arc(); let h = peers.write_arc();"), [".read_arc(", ".write_arc("]);
    assert_eq!(hit("cv.wait(&mut g);"), [".wait("]);
    assert_eq!(hit("cv.wait_for(&mut g, d); cv.wait_while(&mut g, |x| *x);"), [".wait_while(", "_for("]);
    // 기다리지 않는 것, 주석·문자열 안의 것은 걸리지 않는다
    let ok = "if let Some(p) = self.plan.try_lock() {} let _ = t.try_read(); let _ = t.try_write(); \
              /* x.lock() */ log::warn!(\".lock( .read( Mutex::lock( try_lock_for(\"); // m.write()";
    assert!(hit(ok).is_empty());
    // 테스트 모듈 뒤에 숨긴 코드는 "끝이 아님"으로 잡힌다
    assert!(unit_tests_at_end("fn a() {}\n#[cfg(test)]\nmod tests {\n fn t() {}\n}\n"));
    assert!(!unit_tests_at_end("fn a() {}\n#[cfg(test)]\nmod tests {\n}\nfn hidden() { M.lock(); }\n"));
}

/// `Peer.clock`을 잡는 워커 쪽 입구. 부르는 곳은 워커 디렉터리뿐이어야 한다(#143 리뷰 L-2).
const PEER_CLOCK_CALLS: &[&str] = &[".clock_estimate(", ".clock_unsupported("];

/// 주석·문자열을 지운 코드에서 인라인 시험 모듈(`#[cfg(test)] mod 이름 { .. }` — 이름이 `tests`가 아닌 `copy_tests`도)을
/// **그 모듈만** 들어낸다. 모듈 뒤에 이어지는 코드는 남는다(뒤에 숨길 수 없다). 선언만 있는 `mod tests;`는 건드리지 않는다.
fn without_inline_test_mods(src: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let code = strip(src);
    let mut out = String::new();
    let mut rest = code.as_str();
    while let Some(i) = rest.find(ATTR) {
        let after = &rest[i + ATTR.len()..];
        let body = after.trim_start().strip_prefix("mod ").and_then(|m| {
            let open = m.find(['{', ';'])?;
            m[open..].starts_with('{').then(|| &m[open..])
        });
        let Some(body) = body else {
            out.push_str(&rest[..i + ATTR.len()]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..i]);
        let mut depth = 0usize;
        let mut end = body.len();
        for (k, c) in body.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &body[end..];
    }
    out.push_str(rest);
    out
}

/// 엔진 ②b-6b(ADR D1 "`Peer.clock`은 워커와 `band-net-recv`만 잡는다 — 기본 우선순위 스레드가 잡지 않는 한"):
/// `Peer::clock_estimate`·`clock_unsupported`는 `pub(crate)`라 가시성으로는 "워커 전용"을 못 적는다(`net`의 메서드를
/// `engine::worker`에만 보이게 할 수 없다). 그래서 부르는 파일이 `engine/worker/`뿐임을 여기서 고정한다 — 통계·커맨드가
/// 부르게 되면 ②b가 없앤 우선순위 역전이 돌아온다. 시험 모듈 안의 호출은 보지 않는다.
#[test]
fn peer_clock_is_read_only_by_worker() {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    let worker = src_dir().join("engine/worker");
    let mut bad = Vec::new();
    let mut seen = 0;
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let flat: String = without_inline_test_mods(&src).chars().filter(|c| !c.is_whitespace()).collect();
        for c in PEER_CLOCK_CALLS.iter().filter(|c| flat.contains(*c)) {
            seen += 1;
            if !f.starts_with(&worker) {
                bad.push(format!("{}: {c}", f.strip_prefix(src_dir()).unwrap().display()));
            }
        }
    }
    assert!(seen >= 2, "호출을 못 찾음(이름이 바뀌었으면 PEER_CLOCK_CALLS를 고칠 것)");
    assert!(bad.is_empty(), "워커 밖에서 Peer.clock을 잡는 호출(기본 우선순위 스레드면 워커가 그 뒤에서 기다린다): {bad:?}");
    // 검사기 음성 확인: 시험 모듈만 들어내고 그 뒤 코드는 남긴다
    let cut = without_inline_test_mods("fn a() {}\n#[cfg(test)]\nmod copy_tests {\n fn t() { if x { p.clock_estimate(1); } }\n}\nfn b() { p.clock_unsupported(); }\n");
    assert!(!cut.contains("clock_estimate") && cut.contains("fn a()") && cut.contains("fn b() { p.clock_unsupported(); }"), "{cut}");
    let decl = "#[cfg(test)]\nmod tests;\nfn a() { p.clock_estimate(1); }\n";
    assert_eq!(without_inline_test_mods(decl), decl, "선언만 있는 모듈은 자르지 않는다");
}

#[test]
fn exception_list_is_a_ratchet() {
    assert_eq!(
        EXCEPTIONS.len(),
        MAX_EXCEPTIONS,
        "예외 수가 상한과 다르다. 지웠으면 MAX_EXCEPTIONS를 내리고, 늘리지 말 것(새 예외 추가 금지)"
    );
    for (a, b, s) in EXCEPTIONS {
        assert!(OPEN_STAGES.contains(s), "{a} → {b}: 풀 단계 {s}가 열린 단계(OPEN_STAGES)가 아니다");
    }
}

#[test]
fn every_node_is_listed_once() {
    let mut seen = BTreeSet::new();
    for (k, _) in all_nodes() {
        assert!(seen.insert(*k), "NODES·DSP_NODES 중복: {k}");
    }
    for (k, _) in DSP_NODES {
        assert!(*k == DSP_ROOT || k.starts_with(&format!("{DSP_ROOT}::")), "DSP_NODES는 {DSP_ROOT}::로 시작: {k}");
    }
    for (k, _) in NODES {
        assert!(!k.starts_with(DSP_ROOT) && !k.starts_with(APP_ROOT), "NODES에 band-dsp·앱 노드: {k}");
    }
    for (k, l) in APP_NODES {
        assert!(*k == APP_ROOT || k.starts_with(&format!("{APP_ROOT}::")), "APP_NODES는 {APP_ROOT}::로 시작: {k}");
        assert_eq!(*l, 6, "앱 노드는 어댑터(계층 6)뿐: {k}");
    }
    assert!(!NODES.iter().chain(DSP_NODES).any(|(_, l)| *l == 6), "계층 6(어댑터)은 앱 크레이트에만 있다(⑩-2)");
    for (a, b, _) in EXCEPTIONS {
        assert!(seen.contains(a) && seen.contains(b), "EXCEPTIONS의 노드가 NODES에 없음: {a} → {b}");
    }
    for (a, b, _) in FORBIDDEN {
        assert!(seen.contains(a) && seen.contains(b), "FORBIDDEN의 노드가 NODES에 없음: {a} → {b}");
    }
    for a in ADAPTER_MAY_USE {
        assert!(seen.contains(a), "ADAPTER_MAY_USE의 노드가 노드 표에 없음: {a}");
    }
}

#[test]
fn parser_assumptions_hold() {
    for (f, _) in source_files() {
        let text = std::fs::read_to_string(&f).expect("읽기");
        assert!(unit_tests_at_end(&text), "{}: `#[cfg(test)] mod tests`는 파일 끝에 둘 것(검사기 전제)", f.display());
        // `edges()`는 `tests`라는 모듈 파일을 단위 테스트로 보고 건너뛴다. 그러니 `mod tests;` 선언은 바로 앞 줄이
        // `#[cfg(test)]`여야 한다(이름만 tests인 실제 모듈이 검사에서 조용히 빠지지 않게, #81 리뷰 L1).
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (i, l) in lines.iter().enumerate() {
            if matches!(*l, "mod tests;" | "pub mod tests;" | "pub(crate) mod tests;") {
                let prev = lines[..i].iter().rev().find(|p| !p.is_empty()).copied().unwrap_or("");
                assert_eq!(prev, "#[cfg(test)]", "{}: `mod tests;` 앞에는 `#[cfg(test)]`가 있어야 한다(검사기 전제)", f.display());
            }
        }
    }
}

#[test]
fn parser_resolves_paths() {
    let none = BTreeSet::new();
    let m: Vec<String> = vec!["engine".into(), "jitter".into(), "target".into()];
    assert_eq!(resolve("super::State", &m, &none), Some(vec!["engine".into(), "jitter".into(), "State".into()]));
    assert_eq!(resolve("super::super::config::X", &m, &none), Some(vec!["engine".into(), "config".into(), "X".into()]));
    assert_eq!(resolve("crate::net::peer::Peer", &m, &none), Some(vec!["net".into(), "peer".into(), "Peer".into()]));
    assert_eq!(resolve("std::sync::Arc", &m, &none), None);
    let mut out = Vec::new();
    expand_use("super::{codec::Decoder, config::{A, B as C}}", "", &mut out);
    assert_eq!(out, vec!["super::codec::Decoder", "super::config::A", "super::config::B"]);
    assert_eq!(node_of(&["band_dsp".into(), "jitter".into(), "target".into()]), Some("band_dsp::jitter"));
    // 두 크레이트(⑩-1): `band-dsp` 안의 `crate::`는 그 뿌리, 이 크레이트의 `band_dsp::`는 `band-dsp` 노드, 거꾸로는 없다.
    let dsp: Vec<String> = vec!["band_dsp".into(), "jitter".into(), "conceal".into()];
    assert_eq!(resolve("crate::codec::Decoder", &dsp, &none), Some(vec!["band_dsp".into(), "codec".into(), "Decoder".into()]));
    assert_eq!(resolve("super::stats::RateRing", &dsp, &none), Some(vec!["band_dsp".into(), "jitter".into(), "stats".into(), "RateRing".into()]));
    let net: Vec<String> = vec!["net".into(), "recv_loop".into()];
    let to = resolve("band_dsp::jitter::JitterBuffer", &net, &none);
    assert_eq!(to.as_deref().and_then(node_of), Some("band_dsp::jitter"));
    assert_eq!(resolve("crate::engine::rt::X", &net, &none).as_deref().and_then(node_of), Some("engine::rt"));
    let body: Vec<String> = raw_paths("fn f() { let j = band_dsp::jitter::JitterBuffer::new(); }", &none).into_iter().map(|(_, p)| p).collect();
    assert_eq!(body, ["band_dsp::jitter::JitterBuffer::new"], "본문의 band_dsp:: 경로도 간선이다");
    assert_eq!(module_of_file(&dsp_src_dir().join("lib.rs")), vec!["band_dsp".to_string()]);
    assert_eq!(module_of_file(&dsp_src_dir().join("jitter").join("mod.rs")), vec!["band_dsp".to_string(), "jitter".to_string()]);
    assert_eq!(node_of(&["engine".into(), "Engine".into()]), Some("engine"));
    // 앱(⑩-2): 앱 안의 `crate::`는 [band_lib], 앱의 `band_engine::…`은 이 크레이트 노드(접두 없음), 본문 경로도 간선이다.
    let app: Vec<String> = vec![APP_ROOT.into(), "commands".into()];
    assert_eq!(resolve("crate::commands::EngineState", &[APP_ROOT.to_string()], &none).as_deref().and_then(node_of), Some("band_lib::commands"));
    assert_eq!(resolve("band_engine::manager::EngineManager", &app, &none).as_deref().and_then(node_of), Some("manager"));
    assert_eq!(resolve("band_engine::engine::rt::OutputRt", &app, &none).as_deref().and_then(node_of), Some("engine::rt"));
    assert_eq!(resolve("band_engine::Foo", &app, &none), Some(vec!["Foo".to_string()]), "뿌리 항목(지금은 없다)은 접두 없는 경로");
    assert_eq!(resolve("band_lib::run", &[APP_ROOT.to_string(), "main".into()], &none), None, "실행 파일의 band_lib::는 외부 크레이트 경로");
    let body: Vec<String> = raw_paths("fn f() { let n = band_engine::sys::time::unix_ms(); }", &none).into_iter().map(|(_, p)| p).collect();
    assert_eq!(body, ["band_engine::sys::time::unix_ms"]);
    assert_eq!(module_of_file(&app_src_dir().join("commands.rs")), vec![APP_ROOT.to_string(), "commands".to_string()]);
    assert_eq!(module_of_file(&app_src_dir().join("lib.rs")), vec![APP_ROOT.to_string()]);
    assert_eq!(module_of_file(&src_dir().join("lib.rs")), Vec::<String>::new());
    assert_eq!(rel_of(&app_src_dir().join("commands.rs")), "src/commands.rs");
    assert_eq!(rel_of(&dsp_src_dir().join("jitter").join("mod.rs")), "crates/band-dsp/src/jitter/mod.rs");
    assert_eq!(rel_of(&src_dir().join("engine").join("rt.rs")), "engine/rt.rs");
    assert!(src_file("src/commands.rs").exists() && src_file("crates/band-dsp/src/lib.rs").exists() && src_file("engine/rt.rs").exists());
    // 인라인 모듈 안의 super는 그 인라인 모듈 기준이다(device.rs의 `pub mod cpal_backend { use super::*; }`).
    let code = "pub mod a { use super::x; fn f() {} }\nuse super::y;";
    let inline = inline_modules(code);
    let file: Vec<String> = vec!["engine".into(), "device".into()];
    let ps = raw_paths(code, &none);
    let resolved: Vec<Vec<String>> = ps.iter().filter_map(|(at, p)| resolve(p, &module_at(&file, &inline, *at), &none)).collect();
    assert!(resolved.contains(&vec!["engine".into(), "device".into(), "x".into()]));
    assert!(resolved.contains(&vec!["engine".into(), "y".into()]));
    // 부모가 자식을 상대 경로로 쓴다(net/mod.rs의 `use peer::…`, 본문 `relay::f()`) — 부모↔자식 순환을 잡으려면 필요(#76 N1).
    let code = "pub mod peer;\nmod relay;\nuse peer::{Peer, LocalDescriptor};\nfn f() { relay::resolve(); other::g(); }";
    let children = declared_children(code);
    assert_eq!(children, ["peer".to_string(), "relay".to_string()].into_iter().collect());
    let net: Vec<String> = vec!["net".into()];
    let resolved: Vec<Vec<String>> = raw_paths(code, &children).iter().filter_map(|(_, p)| resolve(p, &net, &children)).collect();
    assert!(resolved.contains(&vec!["net".into(), "peer".into(), "Peer".into()]));
    assert!(resolved.contains(&vec!["net".into(), "relay".into(), "resolve".into()]));
    assert!(!resolved.iter().any(|p| p.first().is_some_and(|s| s == "other")));
    // `use crate::{a, b}` 묶음의 `crate::`를 본문 경로로 다시 잡지 않는다(#76 N3).
    let ps = raw_paths("use crate::{engine::config, net};", &none);
    let ps: Vec<&str> = ps.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(ps, vec!["crate::engine::config", "crate::net"]);
    assert_eq!(strip("let s = \"crate::x\"; // crate::y\nuse crate::z;"), "let s = \"\"; \nuse crate::z;");
    assert_eq!(strip("let r = r#\"a \" crate::x\"#; use crate::y;"), "let r = \"\"; use crate::y;");
    assert!(unit_tests_at_end("fn a() {}\n#[cfg(test)]\nmod tests {\n fn t() { let x = \"}\"; }\n}\n"));
    assert!(!unit_tests_at_end("fn a() {}\n#[cfg(test)]\nmod tests {\n}\nfn after() {}\n"));
    // 파일 모듈 선언(`mod tests;`)에서는 자르지 않는다 — jitter/mod.rs처럼 선언이 파일 앞에 있어도 뒤 코드를 본다.
    let decl = "#[cfg(test)]\nmod tests;\nuse crate::x;\n";
    assert_eq!(without_unit_tests(decl), decl);
}

// ============================================================================
// 워커 디렉터리 밖의 워커·RT 스레드 코드(#151 리뷰 L-1, #152 리뷰 L3)
// ============================================================================

/// 주석·문자열을 지운 코드에서 타입 `ty`의 `impl` 블록 전부(고유 `impl ty {`, 제네릭 `impl<T> ty<T> {`, 트레이트
/// `impl Trait for ty {`)를 이어 붙인다(#154 리뷰 L-1 — 둘째 블록·트레이트 impl도 본다). 하나도 없으면 `None`.
fn impl_block(code: &str, ty: &str) -> Option<String> {
    let b = code.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::new();
    let mut from = 0;
    while let Some(off) = code[from..].find("impl") {
        let at = from + off;
        from = at + 4;
        if (at > 0 && ident(b[at - 1])) || b.get(at + 4).is_some_and(|&c| ident(c)) {
            continue;
        }
        let Some(open) = code[at..].find(['{', ';']).map(|k| at + k) else { break };
        if b[open] == b';' {
            continue;
        }
        let head: String = code[at + 4..open].split_whitespace().collect::<Vec<_>>().join(" ");
        // 앞 제네릭 `<…>`를 건너뛴 뒤, `for` 뒤(트레이트 impl) 또는 전체가 구현 대상 타입.
        let mut h = head.as_str();
        if h.starts_with('<') {
            let mut depth = 0;
            let end = h.char_indices().find(|&(_, c)| {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            h = end.map(|(k, _)| h[k + 1..].trim_start()).unwrap_or("");
        }
        let target = h.rsplit(" for ").next().unwrap_or("");
        let name: String = target.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if name != ty {
            continue;
        }
        let mut depth = 0;
        for (k, c) in code[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push_str(&code[open..=open + k]);
                        out.push('\n');
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// 워커 스레드에서 도는데 `engine/worker/` 밖에 있는 코드. (파일, `impl` 블록 — `None`이면 파일 전체).
/// - `metronome/click.rs`: 출력 콜백(RT) 쪽 클릭. 락이 하나도 없어야 한다(RT 검사도 본다).
/// - `metronome/sched.rs`의 `impl MetronomeRt`(블록 전부): 워커 쪽 끝(`refresh_plan`은 `try_lock`만). 같은 파일의
///   `impl MetronomeCtl`(`set_plan`·`stats`)은 커맨드 스레드라 `.lock()`이 정당하므로 블록만 자른다.
/// - `band-dsp`의 `jitter/`: 워커 소유 `JitterBuffer`(`WORKER_OWNED`). 지터 상태는 워커만 만진다(⑩-1에서 크레이트만 옮김).
const WORKER_THREAD_CODE: &[(&str, Option<&str>)] = &[
    ("engine/metronome/click.rs", None),
    ("engine/metronome/sched.rs", Some("MetronomeRt")),
    ("crates/band-dsp/src/jitter/conceal.rs", None),
    ("crates/band-dsp/src/jitter/correction.rs", None),
    ("crates/band-dsp/src/jitter/delay.rs", None),
    ("crates/band-dsp/src/jitter/late_budget.rs", None),
    ("crates/band-dsp/src/jitter/mod.rs", None),
    ("crates/band-dsp/src/jitter/playout.rs", None),
    ("crates/band-dsp/src/jitter/stats.rs", None),
    ("crates/band-dsp/src/jitter/target.rs", None),
    ("crates/band-dsp/src/jitter/tuning.rs", None),
];

/// 기다리지 않는데 `WAITING_LOCK_CALLS` 문자열에 걸리는 호출: (파일, 공백을 지운 꼴, 근거). 그 파일에서만 지운다(#154 리뷰 L-4).
const WORKER_THREAD_NOT_WAITING: &[(&str, &str, &str)] = &[(
    "engine/metronome/sched.rs",
    "self.report.write(",
    "MetronomeRt::publish — SeqCell 작성 끝 `SeqWriter::write`(seq 올리고 쓰기, 기다리지 않음). 작성 끝은 Clone 아님·&mut self 하나(#160 L2)",
)];

fn worker_thread_lock_violations(rel: &str, src: &str, block: Option<&str>) -> Vec<String> {
    let code = strip(without_unit_tests(src));
    let part = match block {
        None => code,
        Some(ty) => match impl_block(&code, ty) {
            Some(b) => b,
            None => return vec![format!("{rel}: `impl {ty}` 블록을 못 찾음(이름이 바뀌었으면 WORKER_THREAD_CODE를 고칠 것)")],
        },
    };
    let mut flat: String = part.chars().filter(|c| !c.is_whitespace()).collect();
    for (file, ok, _) in WORKER_THREAD_NOT_WAITING {
        if *file == rel {
            flat = flat.replace(ok, "");
        }
    }
    let at = block.map(|b| format!(" impl {b}")).unwrap_or_default();
    waiting_lock_calls(&flat).into_iter().map(|l| format!("{rel}{at}: {l}")).collect()
}

/// 워커 디렉터리 밖이지만 워커·RT 스레드에서 도는 코드에도 기다리는 락이 없다(#151 리뷰 L-1).
/// `worker_dir_waits_for_no_lock`과 같은 문자열 검사. 지터 디렉터리는 새 파일이 생기면 목록에 넣어야 한다(파일 수 확인).
#[test]
fn worker_thread_code_outside_worker_dir_waits_for_no_lock() {
    let mut jitter = Vec::new();
    rs_files(&dsp_src_dir().join("jitter"), &mut jitter);
    let listed = WORKER_THREAD_CODE.iter().filter(|(f, _)| f.starts_with("crates/band-dsp/src/jitter/")).count();
    assert_eq!(jitter.len(), listed + 1, "band-dsp jitter/ 파일이 늘거나 줄었다(tests.rs 빼고 WORKER_THREAD_CODE에 맞출 것): {jitter:?}");
    let mut bad = Vec::new();
    for (rel, block) in WORKER_THREAD_CODE {
        let src = std::fs::read_to_string(src_file(rel)).unwrap();
        if !unit_tests_at_end(&src) {
            bad.push(format!("{rel}: 단위 테스트 모듈 뒤에 코드가 있다(검사가 보지 못한다)"));
        }
        bad.extend(worker_thread_lock_violations(rel, &src, *block));
    }
    assert!(bad.is_empty(), "워커·RT 스레드 코드에 기다리는 락 호출이 있다(원자값·try_lock으로 바꿀 것): {bad:?}");
}

/// 위 검사기의 음성 확인.
#[test]
fn worker_thread_scanner_catches_variants() {
    let rel = "engine/metronome/sched.rs";
    let sched = "impl MetronomeCtl { fn stats(&self) { let s = self.stats.lock(); } }\n\
                 impl MetronomeRt {\n fn refresh_plan(&self) { if let Some(p) = self.ctl.plan.try_lock() {} }\n \
                 fn publish(&mut self) { self.report.write(&x); }\n}\n";
    assert!(worker_thread_lock_violations(rel, sched, Some("MetronomeRt")).is_empty(), "MetronomeCtl의 lock은 보지 않는다");
    let bad = sched.replace("self.ctl.plan.try_lock()", "Some(self.ctl.plan.lock())");
    assert_eq!(worker_thread_lock_violations(rel, &bad, Some("MetronomeRt")), [format!("{rel} impl MetronomeRt: .lock(")]);
    let bad = sched.replace("self.report.write(&x)", "self.ctl.other.write()");
    assert_eq!(worker_thread_lock_violations(rel, &bad, Some("MetronomeRt")), [format!("{rel} impl MetronomeRt: .write(")], "허용은 그 호출 하나뿐");
    assert_eq!(worker_thread_lock_violations("other.rs", sched, Some("MetronomeRt")), ["other.rs impl MetronomeRt: .write("], "허용은 그 파일에서만");
    // 둘째 블록·트레이트 impl도 본다(#154 리뷰 L-1)
    let second = format!("{sched}impl MetronomeRt {{ fn peek_plan(&self) {{ self.ctl.plan.lock(); }} }}\n");
    assert_eq!(worker_thread_lock_violations(rel, &second, Some("MetronomeRt")), [format!("{rel} impl MetronomeRt: .lock(")]);
    let tr = format!("{sched}impl Drop for MetronomeRt {{ fn drop(&mut self) {{ self.ctl.plan.lock(); }} }}\n");
    assert_eq!(worker_thread_lock_violations(rel, &tr, Some("MetronomeRt")), [format!("{rel} impl MetronomeRt: .lock(")]);
    assert_eq!(worker_thread_lock_violations(rel, sched, Some("Gone")).len(), 1, "블록을 못 찾으면 실패");
    assert_eq!(worker_thread_lock_violations("c", "fn begin(&mut self) { let g = m.lock(); }", None), ["c: .lock("]);
    assert_eq!(impl_block("impl<T> A for B { } impl A { fn f() { {} } } x", "A").as_deref(), Some("{ fn f() { {} } }\n"));
    assert_eq!(impl_block("impl<T: X<u8>> Tr<T> for A<T> { a } impl AB { b } fn implx() {}", "A").as_deref(), Some("{ a }\n"));
}

/// RT 경로(오디오 콜백·WASAPI 이벤트 루프)에서 금지하는 것(CLAUDE.md "rt.rs와 cpal 콜백 경로: 할당·락·시스템콜·로그·panic
/// 금지"). 주석·문자열을 지우고 공백을 지운 함수 본문에서 찾는 문자열이다. **재발 방지용 문자열 검사**다 — 다른 파일·크레이트의
/// 함수 안(ringbuf `push_slice`, `AtomicF32` 등)과 배열 인덱스 panic은 보지 않는다. 런타임 무할당은 `tests/rt_alloc.rs`가 본다.
/// `Instant::now()`는 허용한다(Windows `QueryPerformanceCounter`, 사용자 모드 읽기 — 콜백 시간 계측 `rt_timing`).
const RT_FORBIDDEN: &[(&str, &[&str])] = &[
    (
        "할당",
        &[
            "Vec::",
            "vec!",
            "Box::",
            "String::",
            "format!",
            ".to_string(",
            ".to_owned(",
            ".to_vec(",
            ".collect",
            ".into_boxed_slice(",
            "Arc::new",
            "Rc::new",
            ".clone(",
            ".push(",
            ".push_str(",
            ".insert(",
            ".extend(",
            ".reserve(",
            ".resize(",
            ".append(",
            ".join(",
            ".concat(",
            ".repeat(",
            ".entry(",
            "HashMap",
            "BTreeMap",
        ],
    ),
    ("락", &["Mutex", "RwLock", "Condvar", "OnceLock", ".get_or_init(", ".try_lock(", ".try_read(", ".try_write("]),
    // 채널: std `mpsc`·crossbeam의 송수신은 기다리거나(동기·유한 채널, recv) 할당한다(무한 채널 send). RT ↔ 다른 스레드는 SPSC 링만(#154 리뷰 M-3).
    ("채널", &["mpsc", "channel(", "channel::<", "crossbeam", ".send(", ".recv(", ".recv_timeout(", ".try_recv(", ".try_send("]),
    ("로그", &["log::", "println!", "eprintln!", "print!", "dbg!", "info!(", "warn!(", "error!(", "debug!(", "trace!("]),
    ("panic", &["panic!", ".unwrap()", ".expect(", "unreachable!", "todo!", "unimplemented!", "assert!", "assert_eq!", "assert_ne!"]),
    ("시스템콜", &["thread::", "sleep(", "std::fs", "File::", "SystemTime", "std::process", "std::env", "std::net", "std::io", "Command::"]),
];

/// RT 함수 본문에서 대문자로 시작하는 이름의 호출(`Xxx(`, `.Xxx(`, `Xxx::<T>(`) 중 허용하는 것 — 열거형·`Option`·`Result` 생성자.
const RT_CONSTRUCTOR_CALLS: &[&str] = &["Some", "Ok", "Err", "F32", "I32", "I16", "Ready", "DeviceLost", "Failed"];

/// WASAPI 이벤트 루프가 정당하게 부르는 Win32·COM 호출(대문자 이름): (이름, 허용 함수 — `None`이면 WASAPI 파일의 RT 함수 어디서나,
/// 근거). `engine/wasapi/` 밖에서는 하나도 허용하지 않는다(#154 리뷰 L-2). 여기 없는 대문자 호출(`Sleep`, `CreateEventW`,
/// `SetEvent`, `CoCreateInstance`, `GetService::<T>` …)은 실패한다(#152 리뷰 L3).
const WASAPI_LOOP_SYSCALLS: &[(&str, Option<&[&str]>, &str)] = &[
    ("WaitForMultipleObjects", Some(&["wait"]), "이벤트 대기 — 이벤트 구동 모드에서 루프의 박자(주기마다 커널 대기가 본질)"),
    ("GetNextPacketSize", Some(&["service_capture"]), "캡처 패킷 크기 — 캡처 버퍼 교환"),
    ("GetBuffer", Some(&["service_capture", "service_render", "prefill"]), "캡처·렌더 버퍼 받기 — WASAPI의 유일한 데이터 경로"),
    ("ReleaseBuffer", Some(&["service_capture", "service_render", "prefill"]), "캡처·렌더 버퍼 돌려주기 — 위와 짝"),
    ("GetCurrentPadding", Some(&["service_render", "check"]), "공유 모드 빈 칸 계산(렌더), 이벤트가 멈췄을 때 장치 무효화 확인(`check`)"),
    ("Stop", Some(&["restart"]), "일시 오류가 RESTART_AFTER번 이어졌을 때의 복구(`DirStream::restart`: Stop → Reset → prefill → Start). 실패 경로에서만"),
    ("Reset", Some(&["restart"]), "위 복구"),
    ("Start", Some(&["restart"]), "위 복구"),
];

/// 금지 문자열에 걸리지만 정당한 RT 호출: (파일, 함수, 공백을 지운 꼴, 근거). 그 함수 본문에서만 지운다(#154 리뷰 L-4).
const RT_ALLOWED_CALLS: &[(&str, &str, &str, &str)] = &[(
    "engine/wasapi/stream.rs",
    "drive_loop",
    "io.wait(WAIT_TIMEOUT_MS)",
    "LoopIo::wait — 락이 아니라 WaitForMultipleObjects 이벤트 대기(WASAPI_LOOP_SYSCALLS)",
)];

enum RtPick {
    /// 파일의 모든 함수(새 함수는 기본 RT). 목록은 제어 평면에서 도는 함수(근거는 `RT_CODE` 주석).
    AllExcept(&'static [&'static str]),
    /// 이 함수들만(같은 이름이 여럿이면 모두). 못 찾으면 실패. 목록 밖 함수를 RT 본문이 부르면 실패(`rt_uncovered_callees`).
    Only(&'static [&'static str]),
}

/// (파일, 고르는 방식, RT 콜백을 인자로 받는 호출 — 그 호출의 인자(클로저)도 RT 본문으로 본다).
const RT_CODE: &[(&str, RtPick, &[&str])] = &[
    ("engine/rt.rs", RtPick::AllExcept(&["measure_path", "input_ring", "output_path", "new", "attach_measure", "attach_click", "attach_record"]), &[]),
    ("engine/metronome/click.rs", RtPick::AllExcept(&["click_wave", "new", "default"]), &[]),
    // 이벤트 루프(`run_loop` → `drive_loop` → `WasapiIo::{wait, service, check, restart}` → `service_*` → `feed_input`·`fill_output`)와
    // `open_engine_streams`가 `run_loop`에 넘기는 `on_capture`·`on_render` 클로저(#154 리뷰 M-1). 새 함수는 기본 RT(#154 리뷰 M-2).
    // 제외 — 루프 밖(오디오 스레드의 열기·종료 또는 다른 스레드): `open`·`open_exclusive`·`open_shared3`(장치 열기·초기화),
    // `start`(첫 시작), `period_ms`·`stream_latency_ms`(열린 뒤 정보), `open_engine_streams`(스레드 준비·종료 로그 — 클로저는 따로 본다),
    // `watch_default`(기본 장치 감시 스레드), `drop`(`DirStream`·`WasapiGuard` 정리).
    (
        "engine/wasapi/stream.rs",
        RtPick::AllExcept(&["open", "open_exclusive", "open_shared3", "start", "period_ms", "stream_latency_ms", "open_engine_streams", "watch_default", "drop"]),
        &["run_loop"],
    ),
    // 포맷 협상(열기)이 대부분이라 RT 함수만 고른다: 24비트 변환과 루프의 `on_error`가 부르는 `classify`(#154 리뷰 M-2).
    ("engine/wasapi/format.rs", RtPick::Only(&["i24_to_f32", "f32_to_i24", "classify"]), &[]),
    // 녹음 탭의 RT 끝점(`rt.rs`가 `rec.poll()`·`rec.active()` → `RecordRt::{begin, source, finish}`을 부른다). 나머지(`RecordPortCtl`의
    // `attach`·`reclaim`, 링 할당 `alloc_rings`, `InFlightGuard::drop`의 락)는 제어 평면·쓰기 스레드다(#162 리뷰 L-1 후속에서 찾음).
    ("engine/record/rt_ring.rs", RtPick::Only(&["poll", "active", "begin", "source", "finish"]), &[]),
    // `band-dsp`에서 RT 본문(위 파일들)이 부르는 함수(#161 리뷰 L-2, ⑩-3). 경로가 `crates/`로 시작하면 워크스페이스 루트 기준이다.
    // 출력 콜백의 소프트 클립(`rt.rs` `OutputRt::process`), 레벨·게인 원자값(`rt.rs`·`click.rs`), 콜백 실행시간 기록
    // (`counters.rt_timing.record_{in,out}` → `nanos`·`histo::bucket_of`). 같은 파일의 나머지(믹스 합산·RMS 등)는 워커·제어 평면 쪽이다 —
    // 워커 경로의 할당은 하네스(`tests/sim`)의 계수 단언과 `rt_alloc`이 동적으로 본다. RT에서 새 `band_dsp` 함수를 부르면 여기에 넣는다 — 안 넣으면 `rt_external_items`(경로로 쓰는 RT_CODE 밖 함수)나 `rt_uncovered_callees`(이 파일들의 목록 밖 함수)가 실패한다(#162 리뷰 L-1).
    ("crates/band-dsp/src/mixer.rs", RtPick::Only(&["soft_clip"]), &[]),
    ("crates/band-dsp/src/stats.rs", RtPick::Only(&["load", "store"]), &[]),
    ("crates/band-dsp/src/rt_timing.rs", RtPick::Only(&["record_out", "record_in", "nanos"]), &[]),
    ("crates/band-dsp/src/histo.rs", RtPick::Only(&["bucket_of"]), &[]),
];

/// `RT_CODE`의 파일 경로 → 실제 경로: `crates/`로 시작하면 워크스페이스 루트 기준(`band-dsp`), 아니면 이 크레이트의 `src` 기준.
fn rt_code_path(rel: &str) -> PathBuf {
    if rel.starts_with("crates/") {
        ws_dir().join(rel)
    } else {
        src_dir().join(rel)
    }
}

/// RT로 보지 않는 함수와 이름만 같은 호출: (파일, 함수, 공백을 지운 호출 꼴, 근거). 그 함수 본문에서만 그 꼴을 지우고
/// `rt_uncovered_callees`가 본다(#162 리뷰 L-2 — 이름 단위 면제 `new`·`default`·`drop`·`take`를 호출 단위로 좁힘). 그래서
/// 다른 함수나 다른 수신자의 같은 이름(`counters.rt_timing.take(…)` — `band-dsp` `RtTiming::take`, 진단 스레드 전용)은 실패한다.
/// 꼴이 그 함수 본문에 없으면 실패한다(낡은 면제를 남기지 않게).
const RT_SAME_NAME_CALLS: &[(&str, &str, &str, &str)] = &[
    ("engine/wasapi/stream.rs", "drive_loop", ".enumerate().take(", "`Iterator::take`(방향 수만큼) — `RtTiming::take`가 아님"),
    ("engine/wasapi/stream.rs", "drive_loop", "FailState::default(", "이 파일의 `#[derive(Default, Clone, Copy)]` 두 필드 구조체 — 할당 없음"),
    ("engine/wasapi/stream.rs", "run_loop", "HANDLE::default(", "windows `HANDLE`(null 핸들 값) — 할당·시스템콜 없음"),
    ("engine/record/rt_ring.rs", "poll", "self.active.take(", "`Option<RecordRt>::take`(필드 `active`) — `RtTiming::take`가 아님"),
];

/// RT_CODE 파일이 경로(`use`·본문 경로)로 쓰는 RT_CODE 밖의 타입(같은 크레이트·`band-dsp`, #162 리뷰 L-1): (파일, 타입, 수신자, 근거).
/// 그 타입의 메서드(고유·트레이트 impl)는 RT로 보지 않는다 — RT 본문이 부르면 실패한다. 수신자가 `None`이면 메서드 이름으로
/// 보고(`rt_uncovered_callees`), `Some(필드)`면 이름이 원자값 등과 겹쳐 `.필드.메서드(`·`타입::메서드(` 꼴로만 본다.
/// 여기 없는 타입을 RT_CODE 파일이 새로 쓰면 실패한다. 상수(정의 파일의 `const`·`static`, #166 리뷰 L-2)와 열거형 변형은 데이터라 보지 않는다. 함수는 목록 없이
/// 이름으로 본다(`band_dsp::config::frame_code_from_samples(…)`를 RT 본문이 부르면 실패).
const RT_EXTERNAL_TYPES: &[(&str, &str, Option<&[&str]>, &str)] = &[
    ("crates/band-dsp/src/config.rs", "EngineConfig", None, "`stream.rs` 장치 열기(`open_engine_streams`)의 인자. RT 본문은 메서드를 부르지 않는다"),
    ("crates/band-dsp/src/prio_slot.rs", "ThreadPriority", None, "`open_engine_streams` 열기 실패 경로가 `Unmanaged` 변형을 적는다(RT 밖)"),
    (
        "crates/band-dsp/src/prio_slot.rs",
        "PrioSlot",
        Some(&["audio_thread"]),
        "`AudioCounters::audio_thread` 필드 타입. 적는 곳은 오디오 스레드의 열기 경로, 읽는 곳은 진단 — RT 본문은 부르지 않는다",
    ),
    ("engine/device/types.rs", "BackendKind", None, "`format.rs`의 포맷 협상(열기) 결과 표시"),
    ("engine/device/types.rs", "StreamInfo", None, "`open_engine_streams`가 열린 스트림 정보를 돌려준다(RT 밖)"),
    ("engine/wasapi/enumerate.rs", "ComGuard", None, "오디오 스레드 COM 초기화 가드(`open_engine_streams`·`watch_default`) — 루프 밖"),
];

/// 공백을 지운 본문에서 이름 앞에 붙을 수 있는 키워드(`rt_uncovered_callees`의 경계).
const RT_KEYWORDS: &[&str] = &["match", "return", "if", "while", "in", "else", "mut", "move", "unsafe", "let", "for"];

/// 대문자로 시작하는 이름의 호출(공백을 지운 본문): `Xxx(`, `.Xxx(`, turbofish `Xxx::<T>(`(#154 리뷰 M-3).
fn capitalized_calls(flat: &str) -> BTreeSet<String> {
    let b = flat.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = BTreeSet::new();
    for (k, _) in flat.match_indices('(') {
        let mut end = k;
        if end > 0 && b[end - 1] == b'>' {
            // `::<…>(` — 짝이 맞는 `<`까지 되돌아가 그 앞 `::` 앞의 이름
            let mut depth = 0;
            let Some(lt) = (0..end).rev().find(|&j| {
                match b[j] {
                    b'>' => depth += 1,
                    b'<' => depth -= 1,
                    _ => {}
                }
                depth == 0
            }) else {
                continue;
            };
            if lt < 2 || &b[lt - 2..lt] != b"::" {
                continue;
            }
            end = lt - 2;
        }
        if let Some(s) = (0..end).rev().take_while(|&j| ident(b[j])).last() {
            if b[s].is_ascii_uppercase() {
                out.insert(flat[s..end].to_string());
            }
        }
    }
    out
}

/// `callee(…)` 호출의 인자(괄호 안, 정의 `fn callee(`는 뺀다). 주석·문자열을 지운 코드.
fn call_args(code: &str, callee: &str) -> Vec<String> {
    let b = code.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let pat = format!("{callee}(");
    let mut out = Vec::new();
    for (at, _) in code.match_indices(&pat) {
        if (at > 0 && ident(b[at - 1])) || code[..at].trim_end().ends_with("fn") {
            continue;
        }
        let open = at + callee.len();
        let mut depth = 0;
        for (k, c) in code[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(code[open + 1..open + k].to_string());
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// 파일에서 RT로 보는 본문들(이름, 공백을 지운 본문)과, RT로 보지 않는 함수 이름들. 함수를 못 찾으면 `bad`에 적는다.
fn rt_bodies(rel: &str, src: &str, pick: &RtPick, callbacks: &[&str], bad: &mut Vec<String>) -> (Vec<(String, String)>, BTreeSet<String>) {
    let code = strip(without_unit_tests(src));
    let bodies = fn_bodies(&code);
    let is_rt = |n: &str| match pick {
        RtPick::AllExcept(skip) => !skip.contains(&n),
        RtPick::Only(names) => names.contains(&n),
    };
    if let RtPick::Only(names) = pick {
        for n in *names {
            if !bodies.iter().any(|(b, _)| b == n) {
                bad.push(format!("{rel}: {n} 함수를 못 찾음(이름이 바뀌었으면 RT_CODE를 고칠 것)"));
            }
        }
    }
    let others: BTreeSet<String> = bodies.iter().filter(|(n, _)| !is_rt(n)).map(|(n, _)| n.clone()).collect();
    let mut chosen: Vec<(String, String)> = bodies.into_iter().filter(|(n, _)| is_rt(n)).collect();
    for cb in callbacks {
        let args = call_args(&code, cb);
        if args.is_empty() {
            bad.push(format!("{rel}: {cb}(…) 호출을 못 찾음(콜백을 넘기는 자리가 바뀌었으면 RT_CODE를 고칠 것)"));
        }
        for a in args {
            chosen.push((format!("{cb} 인자(콜백)"), a.chars().filter(|c| !c.is_whitespace()).collect()));
        }
    }
    (chosen, others)
}

fn rt_body_violations(rel: &str, name: &str, body: &str) -> Vec<String> {
    let mut body = body.to_string();
    for (file, f, ok, _) in RT_ALLOWED_CALLS {
        if *file == rel && *f == name {
            body = body.replace(ok, "");
        }
    }
    let mut bad = Vec::new();
    for (kind, pats) in RT_FORBIDDEN {
        for p in *pats {
            if body.contains(p) {
                bad.push(format!("{rel} {name}: {kind} {p}"));
            }
        }
    }
    for l in waiting_lock_calls(&body) {
        bad.push(format!("{rel} {name}: 락 {l}"));
    }
    let wasapi = rel.starts_with("engine/wasapi/");
    for c in capitalized_calls(&body) {
        let sys = WASAPI_LOOP_SYSCALLS.iter().any(|(s, fns, _)| *s == c && wasapi && fns.is_none_or(|f| f.contains(&name)));
        if !RT_CONSTRUCTOR_CALLS.contains(&c.as_str()) && !sys {
            bad.push(format!("{rel} {name}: 허용 목록 밖 호출 {c}("));
        }
    }
    bad
}

fn rt_violations(rel: &str, src: &str, pick: &RtPick) -> Vec<String> {
    let mut bad = Vec::new();
    let (chosen, _) = rt_bodies(rel, src, pick, &[], &mut bad);
    for (name, body) in &chosen {
        bad.extend(rt_body_violations(rel, name, body));
    }
    bad
}

/// RT 본문이 RT로 보지 않는 함수(제외·목록 밖)를 부르는지: `이름(` 앞이 `.`·`::`·식별자 아닌 문자. 이름이 같은 다른 타입의
/// 메서드도 걸릴 수 있어 보수적이다(#154 리뷰 M-2 — 목록이 닫혀 있어도 루프가 부르는 새 함수를 놓치지 않게).
/// 이름만 같은 호출(`RT_SAME_NAME_CALLS`의 그 파일·함수 꼴)은 지우고 본다(#162 리뷰 L-2).
fn rt_uncovered_callees(rt: &[(String, String, String)], others: &[(String, BTreeSet<String>)]) -> Vec<String> {
    let mut bad = Vec::new();
    for (rel, name, body) in rt {
        let mut body = body.clone();
        for (file, f, form, _) in RT_SAME_NAME_CALLS {
            if file == rel && f == name {
                body = remove_bounded(&body, form);
            }
        }
        let body = &body;
        for (orel, names) in others {
            for n in names {
                let b = body.as_bytes();
                let pat = format!("{n}(");
                // 공백을 지운 본문이라 `match classify(`가 `matchclassify(`가 된다 — 앞이 키워드면 경계로 본다.
                let called = body.match_indices(&pat).any(|(at, _)| {
                    at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') || RT_KEYWORDS.iter().any(|k| body[..at].ends_with(k))
                });
                if called && !body.contains(&format!("fn{n}(")) {
                    bad.push(format!("{rel} {name}: RT로 보지 않는 {orel}의 {n}( 를 부른다(RT_CODE 목록에 넣거나 제외 근거를 다시 볼 것)"));
                }
            }
        }
    }
    bad
}

/// `form`을 지운다. 단 `form`이 식별자 문자로 시작하면 앞 글자가 식별자 문자가 아닌 자리만(#166 리뷰 L-4 —
/// `FailState::default(` 면제가 `MyFailState::default(`의 뒷부분을 지우지 않게).
fn remove_bounded(body: &str, form: &str) -> String {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let check = form.as_bytes().first().is_some_and(|&c| ident(c));
    let b = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut last = 0;
    for (at, _) in body.match_indices(form) {
        if at < last || (check && at > 0 && ident(b[at - 1])) {
            continue;
        }
        out.push_str(&body[last..at]);
        last = at + form.len();
    }
    out.push_str(&body[last..]);
    out
}

/// 상수 이름 꼴(대문자·숫자·`_`만, 첫 글자 대문자).
fn is_const_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase()) && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// 정의 파일(주석·문자열을 지운 코드)에 `const 이름:`·`static 이름:`이 있는 상수 — 데이터라 RT 호출 검사에서 보지 않는다.
/// 이름 꼴만으로 고르면 대문자 약어 타입(`PCM`)이 빠져나간다(#166 리뷰 L-2).
fn is_defined_const(item: &str, def_code: &str) -> bool {
    let one = def_code.split_whitespace().collect::<Vec<_>>().join(" ").replace(" :", ":");
    is_const_name(item) && ["const", "static", "static mut"].iter().any(|k| find_word(&one, &format!("{k} {item}:")).is_some())
}

/// `use` 문의 항목: (경로, 이 파일에서 부르는 이름 — `as` 별칭이면 별칭, 아니면 마지막 마디). `use m::{self, x}`의 `self`는
/// 모듈 `m`으로 본다. `as _`는 이름이 없다(빈 문자열).
fn use_items(code: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut base = 0;
    while let Some(i) = find_word(&code[base..], "use ") {
        let at = base + i;
        let after = &code[at + 4..];
        let end = after.find(';').unwrap_or(after.len());
        let mut v = Vec::new();
        // `expand_use`는 ` as ` 뒤를 버린다 — 별칭을 마디로 바꿔 넘긴 뒤 다시 가른다(#166 리뷰 L-1)
        expand_use(&after[..end].replace(" as ", "::__as__::"), "", &mut v);
        for p in v {
            let (path, alias) = match p.split_once("::__as__::") {
                Some((a, b)) => (a.to_string(), Some(b.trim().to_string())),
                None => (p, None),
            };
            let path = path.strip_suffix("::self").map(str::to_string).unwrap_or(path);
            let local = alias.unwrap_or_else(|| path.rsplit("::").next().unwrap_or("").to_string());
            out.push((path, if local == "_" { String::new() } else { local }));
        }
        base = at + 4 + end;
    }
    out
}

/// RT 본문이 함수를 값으로 넘기는지(`and_then(f)`, `map(m::f)`, `g(a, f)`): 이름 앞이 `(`·`,`·`::`이고 뒤가 `)`·`,`(#166 리뷰 L-1).
/// RT_CODE 밖 함수 이름에만 쓴다(지역 변수 이름과 겹치는 대상 파일 함수 이름에는 쓰지 않는다).
fn rt_fn_values(rt: &[(String, String, String)], fns: &[(String, BTreeSet<String>)]) -> Vec<String> {
    let mut bad = Vec::new();
    for (rel, name, body) in rt {
        for (orel, names) in fns {
            for n in names {
                let hit = body.match_indices(n.as_str()).any(|(at, _)| {
                    let (pre, post) = (&body[..at], &body[at + n.len()..]);
                    (pre.ends_with('(') || pre.ends_with(',') || pre.ends_with("::")) && (post.starts_with(')') || post.starts_with(','))
                });
                if hit {
                    bad.push(format!("{rel} {name}: RT로 보지 않는 {orel}의 {n} 를 값으로 넘긴다"));
                }
            }
        }
    }
    bad
}

/// RT_CODE 밖 항목의 RT 검사 재료(#162 리뷰 L-1). `files`: (RT_CODE 파일, 원문), `defs`: 모듈 경로 → 그 모듈 파일(`rel_of` 꼴),
/// `type_src`: 정의 파일 → 원문(없으면 디스크에서 읽는다). RT_CODE 파일이 경로(`use`·본문 — `crate::`·`super::`·`band_dsp::`)로 쓰는 항목을
/// 그 정의 파일로 해석해, 정의 파일이 RT_CODE 밖이면
/// - 함수(소문자 이름)는 "RT로 보지 않는 이름"이 된다(`band_dsp::config::frame_code_from_samples`),
/// - 타입은 `RT_EXTERNAL_TYPES`에 있어야 하고 그 메서드도 RT로 보지 않는다(수신자 `None`이면 이름, `Some`이면 `.필드.메서드(`·`타입::메서드(` 꼴),
/// - 상수·열거형 변형은 넘긴다.
///
/// 돌려주는 것: (RT로 보지 않는 이름들 — `rt_uncovered_callees`의 `others` 꼴, 수신자 꼴들 — (설명, 공백 없는 꼴), 위반).
#[allow(clippy::type_complexity)]
fn rt_external_items(
    files: &[(String, String)],
    defs: &BTreeMap<Vec<String>, String>,
    type_src: &BTreeMap<String, String>,
) -> (Vec<(String, BTreeSet<String>)>, Vec<(String, BTreeSet<String>)>, Vec<(String, String)>, Vec<String>) {
    let listed: BTreeSet<&str> = RT_CODE.iter().map(|(r, _, _)| *r).collect();
    let (mut fns, mut bad) = (BTreeMap::<String, BTreeSet<String>>::new(), Vec::new());
    let mut used = BTreeSet::new();
    let mut def_code: BTreeMap<String, String> = BTreeMap::new();
    for (rel, src) in files {
        let code = strip(without_unit_tests(src));
        let children = declared_children(&code);
        let module = module_of_file(&rt_code_path(rel));
        // (절대 경로, 이 파일에서 부르는 다른 이름 — `use … as 별칭`)
        let mut refs: Vec<(Vec<String>, Option<String>)> = Vec::new();
        refs.extend(raw_paths(&code, &children).into_iter().filter_map(|(_, p)| resolve(&p, &module, &children)).map(|a| (a, None)));
        let b = code.as_bytes();
        for (path, local) in use_items(&code) {
            let Some(abs) = resolve(&path, &module, &children) else { continue };
            if !defs.contains_key(&abs) {
                refs.push((abs, Some(local)));
                continue;
            }
            // 모듈을 가져와 `모듈::함수(…)`로 부르는 꼴(#166 리뷰 M-1): 본문의 `별칭::…` 경로를 그 모듈 기준으로 푼다
            if local.is_empty() {
                continue;
            }
            let pat = format!("{local}::");
            for (at, _) in code.match_indices(&pat) {
                if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_' || b[at - 1] == b':') {
                    continue;
                }
                let rest: String = code[at + pat.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':').collect();
                let mut a = abs.clone();
                a.extend(rest.split("::").filter(|s| !s.is_empty()).map(str::to_string));
                refs.push((a, None));
            }
        }
        for (abs, alias) in refs {
            if defs.contains_key(&abs) {
                continue; // 모듈 자체(`use band_dsp::config;`) — 위에서 그 모듈 경로로 풀었다
            }
            let Some(k) = (1..abs.len()).rev().find(|&k| defs.contains_key(&abs[..k])) else { continue };
            let def = &defs[&abs[..k]];
            let item = abs[k].as_str();
            if listed.contains(def.as_str()) {
                continue;
            }
            let dc = def_code.entry(def.clone()).or_insert_with(|| {
                strip(without_unit_tests(&type_src.get(def).cloned().unwrap_or_else(|| std::fs::read_to_string(rt_code_path(def)).unwrap_or_default())))
            });
            if is_defined_const(item, dc) {
                continue;
            }
            if item.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                let e = fns.entry(def.clone()).or_default();
                e.insert(item.to_string());
                e.extend(alias.filter(|a| !a.is_empty()));
            } else if RT_EXTERNAL_TYPES.iter().any(|(f, t, _, _)| f == def && *t == item) {
                used.insert((def.clone(), item.to_string()));
            } else {
                bad.push(format!("{rel}: RT_CODE 밖 {def}의 타입 {item}을 쓴다(RT_EXTERNAL_TYPES에 근거와 함께 넣을 것)"));
            }
        }
    }
    let fns: Vec<(String, BTreeSet<String>)> = fns.into_iter().collect();
    let mut others = fns.clone();
    let mut forms = Vec::new();
    for (f, ty, receivers, _) in RT_EXTERNAL_TYPES {
        if !used.contains(&(f.to_string(), ty.to_string())) {
            bad.push(format!("RT_EXTERNAL_TYPES의 {f} {ty}를 RT_CODE 파일이 쓰지 않는다(목록에서 뺄 것)"));
        }
        let code = strip(without_unit_tests(type_src.get(*f).map_or("", String::as_str)));
        let methods: BTreeSet<String> = impl_block(&code, ty).map(|b| fn_bodies(&b).into_iter().map(|(n, _)| n).collect()).unwrap_or_default();
        match receivers {
            None => others.push((format!("{f} {ty}"), methods)),
            Some(fields) => {
                for m in &methods {
                    forms.push((format!("{f} {ty}::{m}"), format!("{ty}::{m}(")));
                    forms.extend(fields.iter().map(|fld| (format!("{f} {ty}::{m}"), format!(".{fld}.{m}("))));
                }
            }
        }
    }
    bad.sort();
    bad.dedup();
    (others, fns, forms, bad)
}

/// RT 경로 정적 검사: `rt.rs`, 출력 콜백의 클릭(`metronome/click.rs`), 녹음 탭 끝점(`record/rt_ring.rs`), RT가 부르는 `band-dsp` 함수(⑩-3),
/// 자체 WASAPI 이벤트 루프(`wasapi/stream.rs` — 루프 함수와 `run_loop`에 넘기는 콜백 클로저 — 와 `format.rs`의 변환·`classify`)에
/// 할당·락·채널·로그·panic·시스템콜 문자열이 없다. WASAPI 루프의 Win32·COM 호출은 `WASAPI_LOOP_SYSCALLS`의 (이름, 함수)만
/// (#152 리뷰 L3, #154 리뷰). RT 본문이 RT로 보지 않는 함수 — 대상 파일의 제외·목록 밖 함수, 대상 파일이 경로로 쓰는 RT_CODE 밖
/// 함수와 타입 메서드(같은 크레이트·`band-dsp`, #162 리뷰 L-1) — 를 부르면 실패한다.
#[test]
fn rt_paths_have_no_alloc_lock_log_panic_syscall() {
    let mut bad = Vec::new();
    let (mut rt, mut others, mut files) = (Vec::new(), Vec::new(), Vec::new());
    for (rel, pick, callbacks) in RT_CODE {
        let src = std::fs::read_to_string(rt_code_path(rel)).unwrap();
        if !unit_tests_at_end(&src) {
            bad.push(format!("{rel}: 단위 테스트 모듈 뒤에 코드가 있다(검사가 보지 못한다)"));
        }
        let (chosen, other) = rt_bodies(rel, &src, pick, callbacks, &mut bad);
        for (name, body) in chosen {
            bad.extend(rt_body_violations(rel, &name, &body));
            rt.push((rel.to_string(), name, body));
        }
        others.push((rel.to_string(), other));
        files.push((rel.to_string(), src));
    }
    // RT_CODE 밖 항목(#162 리뷰 L-1)
    let defs: BTreeMap<Vec<String>, String> = source_files().into_iter().map(|(f, m)| (m, rel_of(&f))).collect();
    let type_src: BTreeMap<String, String> =
        RT_EXTERNAL_TYPES.iter().map(|(f, ..)| (f.to_string(), std::fs::read_to_string(rt_code_path(f)).unwrap_or_default())).collect();
    let (ext, ext_fns, forms, ext_bad) = rt_external_items(&files, &defs, &type_src);
    bad.extend(ext_bad);
    others.extend(ext);
    bad.extend(rt_fn_values(&rt, &ext_fns));
    for (rel, name, body) in &rt {
        for (what, form) in &forms {
            if body.contains(form.as_str()) {
                bad.push(format!("{rel} {name}: RT로 보지 않는 {what}를 부른다({form})"));
            }
        }
    }
    // 이름만 같은 호출 면제는 그 함수 본문에 그 꼴이 있을 때만(#162 리뷰 L-2)
    for (file, f, form, _) in RT_SAME_NAME_CALLS {
        if !rt.iter().any(|(r, n, b)| r == file && n == f && b.contains(form)) {
            bad.push(format!("RT_SAME_NAME_CALLS: {file} {f}에 {form} 가 없다(면제를 뺄 것)"));
        }
    }
    bad.extend(rt_uncovered_callees(&rt, &others));
    assert!(bad.is_empty(), "RT 경로에 금지 호출이 있다(CLAUDE.md: 할당·락·시스템콜·로그·panic 금지): {bad:?}");
}

/// 위 검사기의 음성 확인.
#[test]
fn rt_scanner_catches_variants() {
    let all = RtPick::AllExcept(&["new"]);
    let hit = |body: &str| rt_violations("r", &format!("fn new() {{ let v = Vec::new(); }}\nfn process(&mut self) {{ {body} }}"), &all);
    assert!(hit("let x = self.a.load(Ordering::Relaxed); let t0 = Instant::now(); if let Some(v) = o { Ok(()) } else { Err(e) }").is_empty());
    assert_eq!(hit("let v: Vec<f32> = Vec :: with_capacity(4);"), ["r process: 할당 Vec::"]);
    assert_eq!(hit("let s = format!(\"{}\", 1);"), ["r process: 할당 format!"]);
    assert_eq!(hit("s.push_str(x); v.resize(8, 0.0);"), ["r process: 할당 .push_str(", "r process: 할당 .resize("]);
    assert_eq!(hit("log::warn!(\"x\");"), ["r process: 로그 log::", "r process: 로그 warn!("]);
    assert_eq!(hit("let a = b.get(0).unwrap();"), ["r process: panic .unwrap()"]);
    assert_eq!(hit("std::thread::sleep(d);"), ["r process: 시스템콜 thread::", "r process: 시스템콜 sleep("]);
    assert_eq!(hit("let g = self.m.lock();"), ["r process: 락 .lock("]);
    assert_eq!(hit("if let Some(g) = self.m.try_lock() {}"), ["r process: 락 .try_lock("]);
    assert_eq!(hit("let v = CELL.get_or_init(f);"), ["r process: 락 .get_or_init("]);
    // 채널(#154 리뷰 M-3, 변이 V4 꼴)
    assert_eq!(
        hit("let (tx, rx) = std::sync::mpsc::sync_channel::<u8>(1); let _ = tx.send(1); let _ = rx.recv();"),
        ["r process: 채널 mpsc", "r process: 채널 channel::<", "r process: 채널 .send(", "r process: 채널 .recv("]
    );
    assert_eq!(hit("unsafe { Sleep(1) };"), ["r process: 허용 목록 밖 호출 Sleep("]);
    assert_eq!(hit("let e = unsafe { CreateEventW(None, false, false, None) };"), ["r process: 허용 목록 밖 호출 CreateEventW("]);
    // turbofish COM 호출(#154 리뷰 M-3, 변이 V2 꼴)
    assert_eq!(hit("let r = unsafe { s.client.GetService::<IAudioRenderClient>() };"), ["r process: 허용 목록 밖 호출 GetService("]);
    assert_eq!(hit("let e = CoCreateInstance::<_, IMMDeviceEnumerator>(&X, None, Y);"), ["r process: 허용 목록 밖 호출 CoCreateInstance("]);
    // Win32 허용은 WASAPI 파일의 그 함수에서만(#154 리뷰 L-2)
    assert_eq!(hit("unsafe { cap.GetBuffer(&mut p)?; }"), ["r process: 허용 목록 밖 호출 GetBuffer("], "WASAPI 밖");
    let w = |name: &str, body: &str| rt_body_violations("engine/wasapi/stream.rs", name, &body.chars().filter(|c| !c.is_whitespace()).collect::<String>());
    assert!(w("service_capture", "cap.GetBuffer(&mut p)?; cap.ReleaseBuffer(n)?; cap.GetNextPacketSize()?;").is_empty());
    assert!(w("restart", "let _ = self.client.Stop(); self.client.Reset()?; self.client.Start()").is_empty());
    assert_eq!(w("service_render", "s.client.Start()?;"), ["engine/wasapi/stream.rs service_render: 허용 목록 밖 호출 Start("], "변이 V9 꼴");
    assert!(w("wait", "WaitForMultipleObjects(&h, false, 2)").is_empty());
    // 허용 호출은 그 파일·함수에서만(#154 리뷰 L-4)
    assert!(w("drive_loop", "match io.wait(WAIT_TIMEOUT_MS) {}").is_empty(), "허용 목록의 이벤트 대기");
    assert_eq!(w("service_render", "io.wait(WAIT_TIMEOUT_MS)"), ["engine/wasapi/stream.rs service_render: 락 .wait("]);
    assert_eq!(hit("cv.wait(&mut g);"), ["r process: 락 .wait("]);
    // 주석·문자열 안, 제외한 함수, 단위 테스트 모듈은 보지 않는다
    assert!(hit("/* Vec::new() */ let s = \"format! .lock(\"; // log::x").is_empty());
    let only = RtPick::Only(&["service", "gone"]);
    let src = "fn open() { log::info!(\"\"); }\nfn service(&self) { }\n#[cfg(test)]\nmod tests {\n fn service() { panic!(); }\n}\n";
    assert_eq!(rt_violations("w", src, &only), ["w: gone 함수를 못 찾음(이름이 바뀌었으면 RT_CODE를 고칠 것)"]);
    let calls = capitalized_calls("a.GetBuffer(1);Some(x);x.get(1);T::from_f32(v);a.Get::<A<B>>();Vec<u8>(");
    assert_eq!(calls, ["Get".to_string(), "GetBuffer".to_string(), "Some".to_string()].into_iter().collect());
    // 콜백 인자(#154 리뷰 M-1, 변이 V1 꼴): 제외 함수 안의 `run_loop(…)` 인자를 RT 본문으로 본다
    let cb = "fn run_loop(a: u8, f: impl FnMut()) {}\nfn open_all() { log::info!(\"\"); let e = run_loop(1, |b| { fill(b); log::warn!(\"\"); }); }\n";
    let mut bad = Vec::new();
    let (chosen, others) = rt_bodies("s", cb, &RtPick::AllExcept(&["open_all"]), &["run_loop"], &mut bad);
    assert!(bad.is_empty() && others.contains("open_all"));
    let v: Vec<String> = chosen.iter().flat_map(|(n, b)| rt_body_violations("s", n, b)).collect();
    assert_eq!(v, ["s run_loop 인자(콜백): 로그 log::", "s run_loop 인자(콜백): 로그 warn!("]);
    rt_bodies("s", "fn run_loop() {}", &RtPick::AllExcept(&[]), &["run_loop"], &mut bad);
    assert_eq!(bad.len(), 1, "콜백 호출을 못 찾으면 실패");
    // RT 본문이 RT로 보지 않는 함수를 부르면 실패(#154 리뷰 M-2, 변이 V5·V8 꼴)
    let rt = vec![("f.rs".to_string(), "on_error".to_string(), "matchclassify(e.code()){}x.note(1);".to_string())];
    let others = vec![("g.rs".to_string(), ["classify".to_string(), "note".to_string(), "new".to_string(), "fallback".to_string()].into_iter().collect())];
    assert_eq!(
        rt_uncovered_callees(&rt, &others),
        [
            "f.rs on_error: RT로 보지 않는 g.rs의 classify( 를 부른다(RT_CODE 목록에 넣거나 제외 근거를 다시 볼 것)",
            "f.rs on_error: RT로 보지 않는 g.rs의 note( 를 부른다(RT_CODE 목록에 넣거나 제외 근거를 다시 볼 것)"
        ]
    );
    // 이름만 같은 호출은 (파일, 함수, 꼴) 단위로만 면제(#162 리뷰 L-2): `drive_loop`의 `.enumerate().take(`는 넘기고,
    // 같은 함수의 `.rt_timing.take(`와 다른 함수의 같은 꼴은 잡는다.
    let timing = vec![("rt_timing.rs".to_string(), ["take".to_string()].into_iter().collect())];
    let take = |f: &str, body: &str| rt_uncovered_callees(&[("engine/wasapi/stream.rs".to_string(), f.to_string(), body.to_string())], &timing);
    assert!(take("drive_loop", "for(i,s)inst.iter_mut().enumerate().take(n){}").is_empty());
    assert_eq!(take("drive_loop", "for(i,s)inst.iter_mut().enumerate().take(n){}letw=counters.rt_timing.take(d);").len(), 1);
    assert_eq!(take("service", "for(i,s)inst.iter_mut().enumerate().take(n){}").len(), 1, "다른 함수");
    // RT_CODE 밖 항목(#162 리뷰 L-1): 함수는 이름, 목록 타입은 메서드(수신자), 목록 밖 타입은 실패, 상수·변형은 넘긴다
    let defs: BTreeMap<Vec<String>, String> = [
        (vec!["band_dsp", "config"], "crates/band-dsp/src/config.rs"),
        (vec!["band_dsp", "prio_slot"], "crates/band-dsp/src/prio_slot.rs"),
        (vec!["band_dsp", "jitter"], "crates/band-dsp/src/jitter/mod.rs"),
        (vec!["band_dsp", "mixer"], "crates/band-dsp/src/mixer.rs"),
    ]
    .into_iter()
    .map(|(m, f)| (m.into_iter().map(String::from).collect(), f.to_string()))
    .collect();
    let type_src: BTreeMap<String, String> = [
        (
            "crates/band-dsp/src/config.rs",
            "pub const MAX_SOURCES: usize = 8;\npub struct PCM;\n\
             impl EngineConfig { pub fn validate(&self) {} }\nimpl Default for EngineConfig { fn default() -> Self { x } }",
        ),
        ("crates/band-dsp/src/prio_slot.rs", "impl PrioSlot { pub fn load(&self) {} }"),
    ]
    .into_iter()
    .map(|(f, s)| (f.to_string(), s.to_string()))
    .collect();
    let src = "use band_dsp::config::{frame_code_from_samples, EngineConfig, MAX_SOURCES};\nuse band_dsp::jitter::JitterBuffer;\n\
               use band_dsp::mixer::{mix_into, MixControls};\n\
               fn f() { let _ = band_dsp::prio_slot::ThreadPriority::Unmanaged; let _ = band_dsp::config::frame_samples_from_code(1); }\n";
    let (names, _, forms, bad) = rt_external_items(&[("engine/rt.rs".to_string(), src.to_string())], &defs, &type_src);
    let has = |file: &str, n: &str| names.iter().any(|(f, ns)| f == file && ns.contains(n));
    assert!(has("crates/band-dsp/src/config.rs", "frame_code_from_samples") && has("crates/band-dsp/src/config.rs", "frame_samples_from_code"));
    assert!(has("crates/band-dsp/src/config.rs EngineConfig", "validate") && has("crates/band-dsp/src/config.rs EngineConfig", "default"));
    assert!(!names.iter().any(|(f, _)| f.contains("mixer")), "RT_CODE 파일(mixer.rs)의 항목은 그 파일의 RT 고르기가 본다");
    assert!(forms.contains(&("crates/band-dsp/src/prio_slot.rs PrioSlot::load".to_string(), ".audio_thread.load(".to_string())));
    let not_stale: Vec<&String> = bad.iter().filter(|b| !b.starts_with("RT_EXTERNAL_TYPES의")).collect();
    assert_eq!(not_stale, ["engine/rt.rs: RT_CODE 밖 crates/band-dsp/src/jitter/mod.rs의 타입 JitterBuffer을 쓴다(RT_EXTERNAL_TYPES에 근거와 함께 넣을 것)"]);
    assert!(bad.iter().any(|b| b.contains("PrioSlot를 RT_CODE 파일이 쓰지 않는다")), "쓰지 않는 목록 항목");
    assert!(!bad.iter().any(|b| b.contains("EngineConfig를") || b.contains("ThreadPriority를")), "쓰는 목록 항목");
    assert!(is_const_name("MAX_SOURCES") && is_const_name("I24") && !is_const_name("EngineConfig") && !is_const_name("load"));
    // #166 리뷰: 모듈 가져오기(M-1), `as` 별칭·함수 값(L-1), 대문자 약어 타입(L-2)
    let src = "use band_dsp::config;\nuse band_dsp::config::frame_code_from_samples as fcs;\nuse band_dsp::config::PCM;\n\
               fn f() { let _ = config::frame_samples_from_code(1); let _ = config::MAX_SOURCES; }\n";
    let (names, fns, _, bad) = rt_external_items(&[("engine/rt.rs".to_string(), src.to_string())], &defs, &type_src);
    let has = |n: &str| names.iter().any(|(f, ns)| f == "crates/band-dsp/src/config.rs" && ns.contains(n));
    assert!(has("frame_samples_from_code") && has("fcs") && has("frame_code_from_samples"), "{names:?}");
    assert!(!has("config") && !has("MAX_SOURCES"), "모듈 이름·상수는 함수가 아니다: {names:?}");
    assert!(bad.iter().any(|b| b.contains("의 타입 PCM을 쓴다")), "대문자 약어 타입은 상수가 아니다: {bad:?}");
    let rt = |body: &str| vec![("engine/rt.rs".to_string(), "process".to_string(), body.to_string())];
    assert_eq!(rt_fn_values(&rt("letx=Some(n).and_then(band_dsp::config::frame_samples_from_code);"), &fns).len(), 1);
    assert_eq!(rt_fn_values(&rt("letx=o.map(fcs).unwrap_or(0);g(a,fcs,b);"), &fns).len(), 1, "이름당 하나");
    assert!(rt_fn_values(&rt("letfcs_n=fcs2(1);letx=y.fcs;"), &fns).is_empty());
    assert_eq!(use_items("use a::{self, b as c, d};\nuse e::f as _;"), [
        ("a".to_string(), "a".to_string()),
        ("a::b".to_string(), "c".to_string()),
        ("a::d".to_string(), "d".to_string()),
        ("e::f".to_string(), String::new())
    ]);
    // 면제 꼴은 식별자 경계에서만 지운다(#166 리뷰 L-4)
    assert_eq!(remove_bounded("a=FailState::default();b=MyFailState::default();", "FailState::default("), "a=);b=MyFailState::default();");
    assert_eq!(remove_bounded("x.enumerate().take(n)", ".enumerate().take("), "xn)");
}

/// 엔진 R-1 ⑨a: 통합 테스트·예제는 내부를 `band_engine::testing::…`으로만 쓴다(engine-stage5-10-scope.md 6.2절).
/// 원래 경로(`band_engine::engine::…` 등)를 직접 쓰면 `testing`이 "테스트가 기대는 표면"의 목록 구실을 못 한다.
#[test]
fn tests_use_only_testing_surface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("tests"), &mut files);
    rs_files(&root.join("examples"), &mut files);
    assert!(files.len() > 10, "tests/·examples/ 파일을 찾지 못했다: {files:?}");
    let mut bad = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        for (i, line) in src.lines().enumerate() {
            // `band_engine`이 나오는 모든 곳의 바로 뒤가 `::testing`이고 그 뒤가 `::`·`;`·` as `여야 한다. 크레이트 별칭
            // (`use band_engine as x;`, `extern crate band_engine as x;`)·`band_engine::{…}`·`band_engine;`로 우회하는 것은 실패로 친다
            // (#156 리뷰 L1). `testing` 모듈 자체의 별칭(`use band_engine::testing as t;`)은 그 안의 표면만 쓰므로 허용(#158 리뷰 L2).
            let mut rest = line;
            while let Some(p) = rest.find("band_engine") {
                rest = &rest[p + "band_engine".len()..];
                let ok = rest.strip_prefix("::testing").is_some_and(|t| t.starts_with("::") || t.starts_with(';') || t.starts_with(" as "));
                if !ok {
                    bad.push(format!("{}:{}", f.strip_prefix(root).unwrap_or(f).display(), i + 1));
                }
            }
        }
    }
    // 이 파일은 검사 문자열("band_engine")을 담고 있으므로 뺀다.
    bad.retain(|loc| !loc.starts_with(&format!("tests{}architecture.rs", std::path::MAIN_SEPARATOR)));
    assert!(bad.is_empty(), "tests/·examples/에서 `band_engine::testing::` 밖의 경로를 쓴다: {bad:?}");
}

/// ⑨b-2(#156 리뷰 L3): `testing`은 **항목 단위로** 다시 내보낸다. `src/testing.rs`의 `pub use crate::…` 잎마다
/// (1) 모듈이 아니고(`crate::engine::rt`처럼 모듈을 통째로 내보내면 그 모듈의 `pub` 전부가 시험 표면이 된다 — 하위 모듈은
/// `pub mod rt { pub use …::{항목}; }`처럼 같은 이름의 모듈로 둔다), 글롭이 아니며, (2) 그 이름을 tests/·examples/가 부른다
/// (안 쓰는 재노출은 지운다).
#[test]
fn testing_surface_is_item_level() {
    let text = strip(&std::fs::read_to_string(src_dir().join("testing.rs")).unwrap());
    let mut leaves = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("pub use ") {
        let stmt = &rest[i + "pub use ".len()..];
        let end = stmt.find(';').expect("use 끝");
        let mut paths = Vec::new();
        expand_use(&stmt[..end], "", &mut paths);
        leaves.extend(paths);
        rest = &stmt[end..];
    }
    assert!(leaves.len() > 40, "testing.rs 재노출 읽기: {leaves:?}");
    let me = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ids = identifiers(&[me.join("tests"), me.join("examples")], "architecture.rs");
    let mut bad = Vec::new();
    for p in &leaves {
        let segs: Vec<&str> = p.split("::").collect();
        if segs.first() != Some(&"crate") || p.ends_with('*') {
            bad.push(format!("{p}: crate:: 경로의 항목만(글롭 금지)"));
            continue;
        }
        let rel = segs[1..].join("/");
        if src_dir().join(format!("{rel}.rs")).exists() || src_dir().join(&rel).join("mod.rs").exists() {
            bad.push(format!("{p}: 모듈을 통째로 내보낸다 — 항목 단위로"));
        }
        if !ids.contains(*segs.last().unwrap()) {
            bad.push(format!("{p}: tests/·examples/가 쓰지 않는다 — 지울 것"));
        }
    }
    assert!(bad.is_empty(), "testing 재노출:\n{}", bad.join("\n"));
}
