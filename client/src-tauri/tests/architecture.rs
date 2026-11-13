//! 모듈 의존 방향 검사(`docs/overhaul/engine-architecture.md` 5절).
//!
//! `src/**/*.rs`의 `use`·경로(`crate::…`, `super::…`, `self::…`, 자식 모듈 이름으로 시작하는 상대 경로)를 읽어
//! 모듈 노드 사이 간선을 만들고 규칙을 본다.
//! 1. 계층: 노드는 자기 계층 이하만 쓴다. 같은 계층은 허용하되 순환(강한 연결 요소, 길이 무관)은 금지.
//!    어댑터(6)는 `ADAPTER_MAY_USE`만.
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
    // 0 공용 기초
    ("engine::config", 0),
    ("net::packet", 0),
    ("sys", 0),
    ("sys::prio", 0),
    ("sys::cpu", 0),
    ("sys::time", 0),
    // 1 알고리즘·상태(스레드·소켓 없음)
    ("engine::codec", 1),
    ("engine::resample", 1),
    ("engine::wav", 1),
    ("engine::flac", 1),
    ("engine::jitter", 1),
    ("engine::relay_codec", 1),
    ("engine::mixer", 1),
    ("engine::peer_media", 1),
    ("engine::stats", 1),
    ("engine::rt_timing", 1),
    ("net::clock", 1),
    ("net::crypto", 1),
    ("net::media_link", 1),
    ("net::txlat", 1),
    ("net::types", 1),
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
    ("engine::sim", 4),
    ("engine::measure", 4),
    // 5 B 제어 평면
    ("engine", 5),
    ("engine::cpuprof", 5),
    ("engine::report", 5),
    ("manager", 5),
    ("diag", 5),
    ("diag::timeline", 5),
    ("diag::export", 5),
    // 6 A 어댑터
    ("commands", 6),
    ("crate", 6),
    ("main", 6),
];

/// 현재 어긋남(고정): (from, to, 풀 단계). 위 계층으로 가는 간선, 같은 계층 순환을 끊을 간선, 어댑터의 직접 의존.
/// 문서 5.3절 표와 같다. 단계를 끝내면 여기서 지운다(남아 있으면 `module_dependencies_follow_layers`가 실패).
const EXCEPTIONS: &[(&str, &str, &str)] = &[
    // 믹서가 메트로놈 기본 게인을 안다(metronome.rs가 RT 클릭·스케줄러·계획을 한 파일에) → ⑥ metronome 계층별 분리.
    ("engine::mixer", "engine::metronome", "⑥"),
    // 장치 선택(device) ↔ 자체 WASAPI(wasapi) 순환 → ⑥ device를 선택·cpal·가짜로, 공용 타입(StreamInfo 등)을 아래로.
    ("engine::device", "engine::wasapi", "⑥"),
    ("engine::wasapi", "engine::device", "⑥"),
    // RT 콜백(rt) ↔ 녹음(record: RT 링 + 쓰기 스레드 + 사이드카) 순환 → ⑥ record를 RT 링과 쓰기 쪽으로.
    ("engine::rt", "engine::record", "⑥"),
    ("engine::record", "engine::rt", "⑥"),
    // 어댑터의 로직: 릴레이 주소 DNS 해석, 게인 범위 자르기 → ⑦ manager 쪽으로.
    ("commands", "net::relay", "⑦"),
    ("commands", "engine::mixer", "⑦"),
];

/// 예외 수 상한(래칫). 단계를 끝내 예외를 지우면 이 값도 내린다. 올리는 것은 금지(위 주석).
const MAX_EXCEPTIONS: usize = 7;

/// 아직 끝나지 않은 단계(문서 9절). 단계를 끝내면 지운다.
const OPEN_STAGES: &[&str] = &["③", "⑥", "⑦"];

/// 계층 규칙상 허용(아래로 가는 간선)이지만 금지하는 간선(엔진 ③, `engine-stage3-design.md` 7.1절):
/// net은 미디어 상태(지터 버퍼·믹스 제어)를 모른다. 미디어 쪽과는 `net::media_link`로만 잇는다.
/// `from`이 `"net"`이면 `net`과 `net::*` 모든 노드에 적용한다.
const FORBIDDEN: &[(&str, &str, &str)] = &[
    ("net", "engine::jitter", "③-b"),
    ("net", "engine::mixer", "③-b"),
    // net은 수신 큐(`net::media_link`)에 넣기만 한다. 피어 미디어 상태는 워커 소유(③-a3).
    ("net", "engine::peer_media", "③-a3"),
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
    "engine::report",
    "net",
    "net::peer",
    // 벽시계(진단 내보내기 파일 이름 시각). 로직이 아닌 계층 0 함수(④-1에서 diag::now_ms를 옮김).
    "sys::time",
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
    let mut kws: Vec<String> = vec!["crate::".into(), "super::".into(), "self::".into()];
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
/// 크레이트 밖(std, 외부 크레이트)이면 None.
fn resolve(path: &str, module: &[String], children: &BTreeSet<String>) -> Option<Vec<String>> {
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
        Some(first) if children.contains(*first) => cur = module.to_vec(),
        _ => return None,
    }
    cur.extend(it.map(|s| s.to_string()));
    Some(cur)
}

fn layer(node: &str) -> u8 {
    NODES.iter().find(|(k, _)| *k == node).map(|(_, l)| *l).expect("NODES에 있는 노드")
}

/// 크레이트 소스 파일과 그 모듈 경로.
fn source_files() -> Vec<(PathBuf, Vec<String>)> {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
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
                let rel = f.strip_prefix(src_dir()).unwrap_or(&f).display().to_string().replace('\\', "/");
                edges.entry((from, to)).or_default().insert(rel);
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
    assert!(bad.is_empty(), "금지 간선(FORBIDDEN) — net은 미디어 상태를 직접 쓰지 않는다:\n{}", bad.join("\n"));
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
fn locked_names(t: &[String], names: &BTreeSet<String>) -> Vec<String> {
    let mut hits = Vec::new();
    for i in 0..t.len() {
        if !LOCK_TYPES.contains(&t[i].as_str()) {
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
        for k in j..t.len() {
            match t[k].as_str() {
                "<" => depth += 1,
                ">" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                ";" | "{" => break,
                x if QUEUE_ENDPOINTS.contains(&x) => break,
                x if names.contains(x) => hits.push(format!("{}<…{x}…>", t[i])),
                _ => {}
            }
        }
    }
    hits
}

fn worker_owned_names() -> BTreeSet<String> {
    WORKER_OWNED.iter().map(|s| s.to_string()).collect()
}

/// 지터 버퍼·피어 미디어를 락으로 감싸지 않는다(엔진 ③-a3): `JitterBuffer`·`PeerMedia`를 만지는 스레드는 워커뿐이다
/// (생성·해제는 제어 평면). 누가 다시 넣으면 기본 우선순위 스레드와 워커의 우선순위 역전 경로가 돌아온다.
/// 경로 한정(`crate::engine::jitter::JitterBuffer`), 별칭(`type`·`use … as`, 파일을 넘어서도), 중첩(`Arc<Mutex<Box<PeerMedia>>>`)을
/// 잡는다(#118 리뷰 L-1). 주석·문자열·단위 테스트는 보지 않는다. 검사기 자체의 음성 확인은 `locked_media_scanner_catches_variants`.
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
    let mut names = worker_owned_names();
    add_aliases(&files.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(), &mut names);
    let mut bad = Vec::new();
    for (file, t) in &files {
        for h in locked_names(t, &names) {
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
        locked_names(&t, &names)
    };
    for bad in [
        "struct A { j: parking_lot::Mutex<JitterBuffer> }",
        "struct A { j: parking_lot::Mutex<crate::engine::jitter::JitterBuffer> }",
        "struct A { m: Arc<parking_lot::Mutex<PeerMedia>> }",
        "struct A { m: Mutex<Vec<Box<PeerMedia>>> }",
        "type Jb = JitterBuffer; struct A { j: Mutex<Jb> }",
        "type Jb = jitter::JitterBuffer; type Jb2 = Jb; struct A { j: RwLock<Option<Jb2>> }",
        "use crate::engine::jitter::JitterBuffer as Jb; struct A { j: std::sync::RwLock<Jb> }",
        "fn f() { let m = Mutex::<PeerMedia>::new(x); }",
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
    for (k, _) in NODES {
        assert!(seen.insert(*k), "NODES 중복: {k}");
    }
    for (a, b, _) in EXCEPTIONS {
        assert!(seen.contains(a) && seen.contains(b), "EXCEPTIONS의 노드가 NODES에 없음: {a} → {b}");
    }
    for (a, b, _) in FORBIDDEN {
        assert!(seen.contains(a) && seen.contains(b), "FORBIDDEN의 노드가 NODES에 없음: {a} → {b}");
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
    assert_eq!(node_of(&["engine".into(), "jitter".into(), "target".into()]), Some("engine::jitter"));
    assert_eq!(node_of(&["engine".into(), "Engine".into()]), Some("engine"));
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
