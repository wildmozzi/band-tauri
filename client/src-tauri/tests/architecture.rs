//! 모듈 의존 방향 검사(`docs/overhaul/engine-architecture.md` 5절).
//!
//! `src/**/*.rs`의 `use`·경로(`crate::…`, `super::…`)를 읽어 모듈 노드 사이 간선을 만들고 두 규칙을 본다.
//! 1. 계층: 노드는 자기 계층 이하만 쓴다(같은 계층 허용, 단 서로 쓰는 2-순환은 금지). 어댑터(6)는 `ADAPTER_MAY_USE`만.
//! 2. 예외: 지금 어긋난 간선은 `EXCEPTIONS`에 "풀 단계"와 함께 적는다. 단계를 끝내 간선이 사라졌는데 예외가 남아
//!    있으면 실패한다(목록이 늘 실제와 같게).
//!
//! 단위 테스트 모듈(`#[cfg(test)] mod tests { … }` 이후)은 보지 않는다. 테스트 코드는 계층을 건너 도우미를 쓸 수 있다.
//! 간선 목록 보기: `cargo test --test architecture -- --nocapture`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 노드 → (계층, 이름). 계층 번호가 작을수록 아래(기초)다. 문서 5절 표와 같다.
const NODES: &[(&str, u8)] = &[
    // 0 공용 기초
    ("engine::config", 0),
    ("net::packet", 0),
    ("engine::prio", 0),
    // 1 알고리즘·상태(스레드·소켓 없음)
    ("engine::codec", 1),
    ("engine::resample", 1),
    ("engine::wav", 1),
    ("engine::flac", 1),
    ("engine::jitter", 1),
    ("engine::relay_codec", 1),
    ("engine::mixer", 1),
    ("engine::stats", 1),
    ("net::clock", 1),
    ("net::crypto", 1),
    ("net::txlat", 1),
    ("diag::mask", 1),
    // 2 E 장치 I/O·RT
    ("engine::rt", 2),
    ("engine::device", 2),
    ("engine::wasapi", 2),
    ("engine::record", 2),
    ("engine::metronome", 2),
    // 3 D 네트워크
    ("net", 3),
    ("net::peer", 3),
    ("net::relay", 3),
    ("net::stun", 3),
    ("net::qos", 3),
    // 4 C 워커 파이프라인
    ("engine::worker", 4),
    ("engine::loss_codec_sim", 4),
    ("engine::measure", 4),
    // 5 B 제어 평면
    ("engine", 5),
    ("engine::cpuprof", 5),
    ("manager", 5),
    ("diag", 5),
    ("diag::timeline", 5),
    ("diag::export", 5),
    // 6 A 어댑터
    ("commands", 6),
    ("crate", 6),
    ("main", 6),
];

/// 현재 어긋남: (from, to, 풀 단계). 위 계층으로 가는 간선, 같은 계층 2-순환(양쪽 다 적는다), 어댑터의 직접 의존.
/// 문서 5.3절 표와 같다. 단계를 끝내면 여기서 지운다(남아 있으면 `module_dependencies_follow_layers`가 실패).
const EXCEPTIONS: &[(&str, &str, &str)] = &[
    // 보고 DTO(stats)가 위 계층 타입을 모은다 → ④ stats를 원자 카운터와 보고 DTO로 나눈다.
    ("engine::stats", "engine::cpuprof", "④"),
    ("engine::stats", "engine::metronome", "④"),
    ("engine::stats", "engine::record", "④"),
    ("engine::stats", "net::qos", "④"),
    ("engine::stats", "net::relay", "④"),
    // relay_codec → net::relay(릴레이 상태 타입), net → relay_codec(CodecReason) 순환 → ④ 공용 타입을 아래로.
    ("engine::relay_codec", "net::relay", "④"),
    // 믹서가 메트로놈 기본 게인을 안다(metronome.rs가 RT 클릭·스케줄러·계획을 한 파일에) → ⑥ metronome 계층별 분리.
    ("engine::mixer", "engine::metronome", "⑥"),
    // 장치 선택(device) ↔ 자체 WASAPI(wasapi) 순환 → ⑥ device를 선택·cpal·가짜로, 공용 타입(StreamInfo 등)을 아래로.
    ("engine::device", "engine::wasapi", "⑥"),
    ("engine::wasapi", "engine::device", "⑥"),
    // RT 콜백(rt) ↔ 녹음(record: RT 링 + 쓰기 스레드 + 사이드카) 순환 → ⑥ record를 RT 링과 쓰기 쪽으로.
    ("engine::rt", "engine::record", "⑥"),
    ("engine::record", "engine::rt", "⑥"),
    // manager ↔ diag 순환 → ⑤ diag는 스냅샷 함수를 받는다.
    ("diag", "manager", "⑤"),
    ("diag::export", "manager", "⑤"),
    ("manager", "diag", "⑤"),
    // 어댑터의 로직: 릴레이 주소 DNS 해석, 게인 범위 자르기 → ⑦ manager 쪽으로.
    ("commands", "net::relay", "⑦"),
    ("commands", "engine::mixer", "⑦"),
];

/// 어댑터(계층 6)가 쓸 수 있는 노드: 제어 평면과 계약 DTO(설정·보고·디스크립터)를 가진 모듈만. 로직은 제어 평면에서 부른다.
const ADAPTER_MAY_USE: &[&str] = &[
    "manager",
    "diag",
    "diag::export",
    "engine::config",
    "engine::device",
    "engine::measure",
    "engine::metronome",
    "engine::record",
    "engine::stats",
    "net",
    "net::peer",
    "commands",
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
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

/// 파일 → 모듈 경로(세그먼트). `src/engine/mod.rs` → [engine], `src/engine/jitter/target.rs` → [engine, jitter, target].
fn module_of_file(file: &Path) -> Vec<String> {
    let rel = file.strip_prefix(src_dir()).expect("src 아래");
    let mut segs: Vec<String> = rel.iter().map(|s| s.to_string_lossy().into_owned()).collect();
    let last = segs.pop().unwrap_or_default();
    let stem = last.trim_end_matches(".rs");
    if stem != "mod" && stem != "lib" {
        segs.push(stem.to_string());
    }
    segs
}

/// 모듈 경로 → 노드: 가장 긴 접두가 `NODES`에 있는 것. 최상위 `lib.rs`는 "crate".
fn node_of(path: &[String]) -> Option<&'static str> {
    if path.is_empty() {
        return Some("crate");
    }
    (1..=path.len()).rev().find_map(|n| {
        let key = path[..n].join("::");
        NODES.iter().find(|(k, _)| *k == key).map(|(k, _)| *k)
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

/// 단위 테스트 모듈 앞까지만 남긴다.
fn without_unit_tests(src: &str) -> &str {
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        if line.trim() == "#[cfg(test)]" && src[at + line.len()..].trim_start().starts_with("mod tests") {
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

/// 파일 안의 `crate::`/`super::`/`self::` 경로들과 그 위치(모듈 경로로 해석 전).
fn raw_paths(code: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    // use 문
    let mut base = 0;
    while let Some(i) = find_word(&code[base..], "use ") {
        let at = base + i;
        let after = &code[at + 4..];
        let end = after.find(';').unwrap_or(after.len());
        let mut v = Vec::new();
        expand_use(&after[..end], "", &mut v);
        out.extend(v.into_iter().map(|p| (at, p)));
        base = at + 4 + end;
    }
    // 본문의 경로(타입·함수 경로)
    let b = code.as_bytes();
    for kw in ["crate::", "super::"] {
        let mut from = 0;
        while let Some(i) = code[from..].find(kw) {
            let s = from + i;
            from = s + kw.len();
            if s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'_' || b[s - 1] == b':') {
                continue;
            }
            let end = code[s..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .map_or(code.len(), |e| s + e);
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

/// 경로를 이 파일의 모듈 기준으로 절대 모듈 경로로 바꾼다. 크레이트 밖(std, 외부 크레이트)이면 None.
fn resolve(path: &str, module: &[String]) -> Option<Vec<String>> {
    let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    let mut cur: Vec<String>;
    let mut it = segs.iter().peekable();
    match it.peek().copied() {
        Some(&"crate") => {
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
        _ => return None,
    }
    cur.extend(it.map(|s| s.to_string()));
    Some(cur)
}

fn layer(node: &str) -> u8 {
    NODES.iter().find(|(k, _)| *k == node).map(|(_, l)| *l).expect("NODES에 있는 노드")
}

/// 실제 간선(from → to, 노드 단위).
fn edges() -> BTreeMap<(&'static str, &'static str), BTreeSet<String>> {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    let attached = path_attr_modules(&files);
    let mut edges: BTreeMap<(&str, &str), BTreeSet<String>> = BTreeMap::new();
    for f in files {
        let module = attached.get(&f).cloned().unwrap_or_else(|| module_of_file(&f));
        let from = node_of(&module).unwrap_or_else(|| panic!("{}: NODES에 없는 모듈 {:?} — 표에 계층을 정해 넣을 것", f.display(), module));
        let text = std::fs::read_to_string(&f).expect("읽기");
        let code = strip(without_unit_tests(&text));
        let inline = inline_modules(&code);
        for (at, p) in raw_paths(&code) {
            let Some(abs) = resolve(&p, &module_at(&module, &inline, at)) else { continue };
            let Some(to) = node_of(&abs) else { continue };
            if to != from {
                let rel = f.strip_prefix(src_dir()).unwrap_or(&f).display().to_string().replace('\\', "/");
                edges.entry((from, to)).or_default().insert(rel);
            }
        }
    }
    edges
}

#[test]
fn module_dependencies_follow_layers() {
    let edges = edges();
    let exc: BTreeSet<(&str, &str)> = EXCEPTIONS.iter().map(|(a, b, _)| (*a, *b)).collect();
    let mut bad = Vec::new();
    for ((from, to), files) in &edges {
        let (lf, lt) = (layer(from), layer(to));
        let upward = lt > lf;
        let cycle = lt == lf && edges.contains_key(&(*to, *from));
        let adapter = lf == 6 && !ADAPTER_MAY_USE.contains(to);
        if (upward || cycle || adapter) && !exc.contains(&(*from, *to)) {
            let why = if upward {
                format!("위 계층 사용({lf} → {lt})")
            } else if cycle {
                "같은 계층 순환".to_string()
            } else {
                "어댑터는 제어 평면·계약 DTO만(ADAPTER_MAY_USE)".to_string()
            };
            bad.push(format!("  {from} → {to}: {why} [{}]", files.iter().cloned().collect::<Vec<_>>().join(", ")));
        }
    }
    let stale: Vec<String> = EXCEPTIONS
        .iter()
        .filter(|(a, b, _)| !edges.contains_key(&(*a, *b)))
        .map(|(a, b, s)| format!("  {a} → {b} (단계 {s})"))
        .collect();
    for ((from, to), files) in &edges {
        println!("{from} → {to}  [{}]", files.iter().cloned().collect::<Vec<_>>().join(", "));
    }
    assert!(
        bad.is_empty(),
        "의존 방향 위반(engine-architecture.md 5절). 고치거나, 정말 필요하면 문서와 EXCEPTIONS에 풀 단계와 함께 적을 것:\n{}",
        bad.join("\n")
    );
    assert!(stale.is_empty(), "이미 사라진 예외 — EXCEPTIONS와 문서에서 지울 것:\n{}", stale.join("\n"));
}

#[test]
fn every_node_is_listed_once() {
    let mut seen = BTreeSet::new();
    for (k, _) in NODES {
        assert!(seen.insert(*k), "NODES 중복: {k}");
    }
    for (a, b, _) in EXCEPTIONS {
        assert!(seen.contains(a) && seen.contains(b), "EXCEPTIONS의 노드가 NODES에 없음: {a} → {b}");
    }
}

#[test]
fn parser_resolves_paths() {
    let m: Vec<String> = vec!["engine".into(), "jitter".into(), "target".into()];
    assert_eq!(resolve("super::State", &m), Some(vec!["engine".into(), "jitter".into(), "State".into()]));
    assert_eq!(resolve("super::super::config::X", &m), Some(vec!["engine".into(), "config".into(), "X".into()]));
    assert_eq!(resolve("crate::net::peer::Peer", &m), Some(vec!["net".into(), "peer".into(), "Peer".into()]));
    assert_eq!(resolve("std::sync::Arc", &m), None);
    let mut out = Vec::new();
    expand_use("super::{codec::Decoder, config::{A, B as C}}", "", &mut out);
    assert_eq!(out, vec!["super::codec::Decoder", "super::config::A", "super::config::B"]);
    assert_eq!(node_of(&["engine".into(), "jitter".into(), "target".into()]), Some("engine::jitter"));
    assert_eq!(node_of(&["engine".into(), "Engine".into()]), Some("engine"));
    // 인라인 모듈 안의 super는 그 인라인 모듈 기준이다(device.rs의 `pub mod cpal_backend { use super::*; }`).
    let code = "pub mod a { use super::x; fn f() {} }\nuse super::y;";
    let inline = inline_modules(code);
    let file: Vec<String> = vec!["engine".into(), "device".into()];
    let ps = raw_paths(code);
    let resolved: Vec<Vec<String>> = ps.iter().filter_map(|(at, p)| resolve(p, &module_at(&file, &inline, *at))).collect();
    assert!(resolved.contains(&vec!["engine".into(), "device".into(), "x".into()]));
    assert!(resolved.contains(&vec!["engine".into(), "y".into()]));
    assert_eq!(strip("let s = \"crate::x\"; // crate::y\nuse crate::z;"), "let s = \"\"; \nuse crate::z;");
}
