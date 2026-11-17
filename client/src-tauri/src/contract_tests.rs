//! 프론트 계약 대조(계약 감사): 코드와 설계 문서(`docs/overhaul/native-engine-design.md` 4절)가 어긋나지 않게 소스 텍스트로 본다.
//!
//! - 커맨드: `lib.rs`의 `generate_handler!` 목록 = 프론트(`client/src`)가 `invoke('…')`로 부르는 이름 = 문서 4절 커맨드 표.
//! - 계약 타입: Rust 구조체(serde camelCase, `#[serde(skip)]` 제외)의 필드 = `client/src/native/engineApi.ts`의 같은 interface 필드
//!   = (문서에 `interface X { … }` 발췌가 있으면) 그 발췌의 필드.
//!
//! 새 커맨드·필드를 더하면 세 곳(또는 두 곳)을 같이 고쳐야 이 시험이 통과한다. 파서는 이 저장소의 서식(한 줄 필드, 4칸/2칸 들여쓰기)만 가정한다.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} 읽기 실패: {e}", p.display())).replace('\r', "")
}

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, exts, out);
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)) {
            out.push(p);
        }
    }
}

/// `lib.rs` `generate_handler![commands::a, commands::b, …]`의 이름.
fn registered_commands() -> BTreeSet<String> {
    let src = read("client/src-tauri/src/lib.rs");
    let start = src.find("generate_handler![").expect("generate_handler! 없음");
    let end = start + src[start..].find(']').expect("generate_handler! 닫힘 없음");
    src[start..end].split("commands::").skip(1).map(|s| s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or("").to_string()).collect()
}

/// 프론트 소스(시험 제외)의 `invoke('name'` · `invoke<T>('name'` 리터럴 이름.
fn invoked_commands() -> BTreeSet<String> {
    let mut files = Vec::new();
    walk(&root().join("client").join("src"), &["ts", "tsx"], &mut files);
    let mut out = BTreeSet::new();
    for f in files {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains(".test.") {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        for (at, _) in src.match_indices("invoke") {
            let mut rest = &src[at + "invoke".len()..];
            if let Some(r) = rest.strip_prefix('<') {
                // 타입 인자를 건너뛴다(`invoke<string | null>(`)
                let mut depth = 1;
                let mut cut = r.len();
                for (i, c) in r.char_indices() {
                    match c {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                cut = i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                rest = &r[cut..];
            }
            if let Some(r) = rest.strip_prefix("('") {
                if let Some(end) = r.find('\'') {
                    out.insert(r[..end].to_string());
                }
            }
        }
    }
    out
}

/// 설계 문서 4절 커맨드 표(`| \`engine_x\` | …`)의 이름.
fn documented_commands() -> BTreeSet<String> {
    let doc = read("docs/overhaul/native-engine-design.md");
    let start = doc.find("## 4. Tauri 커맨드").expect("4절 없음");
    let end = start + doc[start..].find("\n## 5.").unwrap_or(doc.len() - start);
    doc[start..end]
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .filter(|n| n.starts_with("engine_") || n.starts_with("diag_"))
        .map(str::to_string)
        .collect()
}

#[test]
fn commands_match_frontend_and_design_doc() {
    let reg = registered_commands();
    let inv = invoked_commands();
    let doc = documented_commands();
    assert!(reg.len() >= 30, "generate_handler 파싱 결과가 너무 적다({}) — 시험을 고칠 것", reg.len());
    let diff = |a: &BTreeSet<String>, b: &BTreeSet<String>| a.difference(b).cloned().collect::<Vec<_>>();
    assert!(diff(&reg, &inv).is_empty(), "등록됐지만 프론트가 부르지 않는 커맨드: {:?}", diff(&reg, &inv));
    assert!(diff(&inv, &reg).is_empty(), "프론트가 부르지만 등록되지 않은 커맨드(실행 시 오류): {:?}", diff(&inv, &reg));
    assert!(diff(&reg, &doc).is_empty(), "설계 문서 4절 표에 없는 커맨드: {:?}", diff(&reg, &doc));
    assert!(diff(&doc, &reg).is_empty(), "설계 문서에만 있는(없어진) 커맨드: {:?}", diff(&doc, &reg));
}

fn camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// 모든 Rust 소스를 한 번 읽어 둔다(엔진·DSP·앱).
fn rust_sources() -> Vec<String> {
    let mut files = Vec::new();
    let base = root().join("client").join("src-tauri");
    walk(&base.join("src"), &["rs"], &mut files);
    walk(&base.join("crates"), &["rs"], &mut files);
    files.into_iter().filter(|p| !p.components().any(|c| c.as_os_str() == "tests")).map(|p| std::fs::read_to_string(p).unwrap_or_default().replace('\r', "")).collect()
}

/// Rust 구조체의 직렬화 필드(camelCase). 정의는 저장소 전체에서 하나여야 한다.
fn rust_fields(srcs: &[String], name: &str) -> Vec<String> {
    let heads = [format!("pub struct {name} {{"), format!("pub(crate) struct {name} {{")];
    let found: Vec<&str> = srcs
        .iter()
        .flat_map(|s| heads.iter().filter_map(move |h| s.find(h.as_str()).map(|i| &s[i + h.len()..])))
        .collect();
    assert_eq!(found.len(), 1, "구조체 {name} 정의가 {}개 — 하나여야 한다", found.len());
    let body = &found[0][..found[0].find("\n}").expect("구조체 닫힘 없음")];
    let mut out = Vec::new();
    let mut skip = false;
    for l in body.lines() {
        let t = l.trim_start();
        if t.starts_with("#[serde(skip)]") || t.starts_with("#[serde(skip_serializing)]") {
            skip = true;
            continue;
        }
        if l.starts_with("    ") && !l.starts_with("     ") && !t.starts_with("//") && !t.starts_with('#') {
            let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
            if let Some((ident, _)) = t.split_once(':') {
                if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                    if !skip {
                        out.push(camel(ident));
                    }
                    skip = false;
                }
            }
        }
    }
    out
}

/// TS interface의 최상위 필드(2칸 들여쓰기 `name:`/`name?:`). `extends X`면 X의 필드도 더한다.
fn ts_fields(ts: &str, name: &str) -> Vec<String> {
    let head = format!("export interface {name} ");
    let at = ts.find(&head).unwrap_or_else(|| panic!("engineApi.ts에 interface {name} 없음"));
    let rest = &ts[at + head.len()..];
    let (decl, body) = rest.split_once('{').expect("interface 여는 괄호 없음");
    let body = &body[..body.find("\n}").expect("interface 닫힘 없음")];
    let mut out: Vec<String> = body
        .lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.trim_start().split_once(':').map(|(n, _)| n.trim_end_matches('?').to_string()))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))
        .collect();
    if let Some(parent) = decl.trim().strip_prefix("extends ") {
        out.extend(ts_fields(ts, parent.trim()));
    }
    out
}

/// 설계 문서의 `interface NAME { … }` 발췌가 있으면 그 최상위 필드(중첩 `{ }` 안은 빼고, `//` 주석 줄은 무시).
fn doc_fields(doc: &str, name: &str) -> Option<Vec<String>> {
    let head = format!("interface {name} ");
    let at = doc.find(&head)?;
    let rest = &doc[at + head.len()..];
    let open = rest.find('{')?;
    let mut depth = 0;
    let mut top = String::new();
    for c in rest[open..].chars() {
        match c {
            '{' => {
                depth += 1;
                if depth > 1 {
                    continue;
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                continue;
            }
            _ => {}
        }
        if depth == 1 && c != '{' {
            top.push(c);
        }
    }
    let mut out = Vec::new();
    for line in top.lines() {
        let line = line.split("//").next().unwrap_or("");
        for part in line.split(';') {
            if let Some((n, _)) = part.trim().split_once(':') {
                let n = n.trim().trim_end_matches('?');
                if !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()) {
                    out.push(n.to_string());
                }
            }
        }
    }
    Some(out)
}

#[test]
fn contract_types_match_engine_api_and_design_doc() {
    // (Rust 구조체, TS interface)
    const PAIRS: [(&str, &str); 28] = [
        ("EngineStats", "EngineStats"),
        ("PeerStats", "PeerStats"),
        ("RelayStats", "RelayStats"),
        ("QosStats", "QosStats"),
        ("SendQueueStats", "SendQueueStats"),
        ("CpuStats", "CpuStats"),
        ("ThreadPriorityStats", "ThreadPriorityStats"),
        ("ThreadPlacement", "ThreadPlacement"),
        ("CorePlacementStats", "CorePlacementStats"),
        ("DeviceInfo", "DeviceInfo"),
        ("EngineConfig", "EngineConfig"),
        ("Candidate", "Candidate"),
        ("LocalDescriptor", "LocalDescriptor"),
        ("RelayArgs", "RelayConfig"),
        ("LoopbackRun", "LoopbackRun"),
        ("LoopbackReport", "LoopbackReport"),
        ("EchoReport", "EchoReport"),
        ("RecordOptions", "RecordOptions"),
        ("RecordStats", "RecordStats"),
        ("RecordTrack", "RecordTrack"),
        ("RecordSummary", "RecordSummary"),
        ("RecordMarker", "RecordMarker"),
        ("MarkersFile", "RecordMarkersFile"),
        ("RecordFolderPick", "RecordFolderPick"),
        ("MetronomeConfig", "MetronomeConfig"),
        ("MetronomePlan", "MetronomePlan"),
        ("MetronomeStats", "MetronomeStats"),
        ("MarkerTrack", "RecordMarkerTrackShape"),
    ];
    let srcs = rust_sources();
    let ts = read("client/src/native/engineApi.ts");
    let doc = read("docs/overhaul/native-engine-design.md");
    let mut problems = Vec::new();
    let mut doc_checked = 0;
    for (rs, tsn) in PAIRS {
        // MarkersFile.tracks의 원소는 TS에서 인라인 타입이다 — 그 필드는 아래에서 따로 본다
        if tsn == "RecordMarkerTrackShape" {
            let r: BTreeSet<String> = rust_fields(&srcs, rs).into_iter().collect();
            let want: BTreeSet<String> = ["file", "name", "kind", "peerId", "markers"].iter().map(|s| s.to_string()).collect();
            let inline = ts.find("tracks:{ file: string; name: string; kind: RecordTrack['kind']; peerId: string | null; markers: RecordMarker[] }[]");
            if r != want || inline.is_none() {
                problems.push(format!("MarkerTrack: Rust {r:?} / TS 인라인 tracks 타입 {}", if inline.is_some() { "있음" } else { "없음(형태 바뀜)" }));
            }
            continue;
        }
        let r: BTreeSet<String> = rust_fields(&srcs, rs).into_iter().collect();
        let t: BTreeSet<String> = ts_fields(&ts, tsn).into_iter().collect();
        if r != t {
            problems.push(format!(
                "{rs} ↔ {tsn}: Rust에만 {:?}, TS에만 {:?}",
                r.difference(&t).collect::<Vec<_>>(),
                t.difference(&r).collect::<Vec<_>>()
            ));
        }
        if let Some(d) = doc_fields(&doc, tsn) {
            doc_checked += 1;
            let d: BTreeSet<String> = d.into_iter().collect();
            // 발췌가 extends를 쓰면 부모 필드는 발췌에 없다 — 부모의 발췌와 합쳐 본다
            let d: BTreeSet<String> = if tsn == "MetronomeStats" { d.union(&doc_fields(&doc, "MetronomePlan").unwrap_or_default().into_iter().collect()).cloned().collect() } else { d };
            if d != t {
                problems.push(format!(
                    "문서 interface {tsn}: 문서에만 {:?}, 코드에만 {:?}",
                    d.difference(&t).collect::<Vec<_>>(),
                    t.difference(&d).collect::<Vec<_>>()
                ));
            }
        }
    }
    assert!(doc_checked >= 20, "문서 interface 발췌를 {doc_checked}개만 찾았다 — 문서 형태가 바뀌었으면 시험을 고칠 것");
    assert!(problems.is_empty(), "계약 어긋남:\n{}", problems.join("\n"));
}
