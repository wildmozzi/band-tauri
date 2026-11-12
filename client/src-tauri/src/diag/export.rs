//! 진단 zip 구성(P2-5). 무엇을 넣고 빼는지는 `README_TEXT`와 PR 본문 표에 적는다.
//!
//! ```text
//! band-diag-<UTC 시각>.zip
//!   README.txt       내용·마스킹 설명
//!   meta.json        앱 버전, OS, 내보낸 시각, 타임라인 범위
//!   engine.json      현재(마지막) 엔진 설정·통계, 오디오 장치 목록
//!   timeline.jsonl   1초 간격 엔진 통계(최근 60분)
//!   logs/*.log       최근 앱 로그(마스킹, 최대 LOG_BUDGET_BYTES)
//! ```

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::mask::mask_text;
use super::timeline::{to_jsonl, Sample, Timeline, SAMPLE_INTERVAL_MS};
use super::Diagnostics;
use crate::engine::config::EngineConfig;
use crate::engine::device::DeviceInfo;
use crate::manager::EngineManager;

/// 로그 전체 상한(마스킹 전 바이트). 최신 파일의 끝부분부터 채운다.
pub const LOG_BUDGET_BYTES: usize = 2 * 1024 * 1024;

pub const README_TEXT: &str = "Band 진단 자료 (P2-5)

이 파일은 합주 중 끊김·지연 원인을 찾기 위한 자료입니다. 개발자에게 보내 주세요.

meta.json       앱 버전, OS 종류·버전, 내보낸 시각, 타임라인 범위
engine.json     엔진 설정(버퍼·프레임·코덱·지터 범위), 마지막 엔진 통계, 오디오 장치 목록(이름·드라이버·채널·레이트)
timeline.jsonl  1초 간격 엔진 통계(최근 최대 60분): 백엔드·장치 버퍼·지연·xrun·스트림 오류,
                피어별 경로(lan/public/relay)·RTT·지터·버퍼·재생 지연·손실·늦음·은닉(누적)
                session 값이 바뀐 곳은 엔진 재시작이라 누적 값이 0부터 다시 셉니다.
logs/           최근 앱 로그(마스킹됨)

넣지 않거나 가린 것
- 로그인 토큰·릴레이 토큰·암호화 키·JWT: 넣지 않음 / 로그에서 <jwt>, <redacted>로 가림
- 이메일: <email>로 가림
- 사용자·피어 id(UUID): id-xxxxxxxx 가명(같은 id는 같은 값). 짧은 해시라 후보 id를 아는 사람은
  되돌릴 수 있습니다(같은 방 참가자끼리는 서로의 id를 압니다). 완전한 익명화는 아닙니다.
- IP 주소: 대역만(IPv4 a.b.x.x, IPv6 앞 두 그룹), 포트는 남김
- 사용자 폴더 이름(Windows C:\\Users, macOS /Users, Linux /home): <user>
- 방 이름·채팅·닉네임: 엔진 자료에 없음
주의: 오디오 장치 이름은 그대로 들어갑니다(블루투스 장치 이름에 사람 이름이 있을 수 있음).
";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    /// `windows` | `macos` | `linux` …
    pub family: String,
    pub arch: String,
    /// 예: `Microsoft Windows [Version 10.0.26100.1234]`(얻지 못하면 null)
    pub version: Option<String>,
}

impl OsInfo {
    pub fn current() -> Self {
        Self {
            family: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            version: os_version(),
        }
    }
}

#[cfg(windows)]
fn os_version() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("cmd")
        .args(["/C", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

#[cfg(not(windows))]
fn os_version() -> Option<String> {
    None
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineInfo {
    pub interval_ms: u64,
    pub capacity: usize,
    pub samples: usize,
    /// 용량 초과로 버린 샘플 수
    pub dropped: u64,
    pub first_t: Option<u64>,
    pub last_t: Option<u64>,
}

impl TimelineInfo {
    pub fn of(tl: &Timeline) -> Self {
        Self {
            interval_ms: SAMPLE_INTERVAL_MS,
            capacity: tl.capacity(),
            samples: tl.len(),
            dropped: tl.dropped(),
            first_t: tl.iter().next().map(|s| s.t),
            last_t: tl.last().map(|s| s.t),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub format: u32,
    pub app: AppInfo,
    pub os: OsInfo,
    pub exported_at: u64,
    pub exported_at_iso: String,
    pub timeline: TimelineInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    /// 지금 엔진이 돌고 있는지(아니면 아래 값은 마지막 세션 것)
    pub active: bool,
    /// 현재 또는 마지막 세션의 엔진 설정(장치 id = 장치 이름)
    pub config: Option<EngineConfig>,
    /// 마지막 통계(피어 id·릴레이 주소 마스킹)
    pub last_stats: Option<Sample>,
    pub devices: Vec<DeviceInfo>,
    /// 장치 열거 실패 사유
    pub devices_error: Option<String>,
}

/// zip에 들어갈 내용 전부(이미 마스킹된 값).
pub struct Bundle {
    pub meta: Meta,
    pub engine: EngineInfo,
    pub timeline_jsonl: String,
    /// (파일 이름, 마스킹된 내용)
    pub logs: Vec<(String, String)>,
}

impl Bundle {
    pub fn new(meta: Meta, engine: EngineInfo, timeline: &Timeline, logs: Vec<(String, String)>) -> Self {
        Self { meta, engine, timeline_jsonl: to_jsonl(timeline), logs }
    }

    /// zip 바이트로. 항목 순서: README, meta, engine, timeline, logs.
    pub fn to_zip(&self) -> Result<Vec<u8>, String> {
        fn err(e: impl std::fmt::Display) -> String {
            format!("진단 zip 작성 실패: {e}")
        }
        let mut entries: Vec<(String, Vec<u8>)> = vec![
            ("README.txt".into(), README_TEXT.as_bytes().to_vec()),
            ("meta.json".into(), serde_json::to_vec_pretty(&self.meta).map_err(err)?),
            ("engine.json".into(), serde_json::to_vec_pretty(&self.engine).map_err(err)?),
            ("timeline.jsonl".into(), self.timeline_jsonl.as_bytes().to_vec()),
        ];
        for (name, body) in &self.logs {
            entries.push((format!("logs/{}", sanitize_name(name)), body.as_bytes().to_vec()));
        }
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, body) in entries {
            zw.start_file(name, opts).map_err(err)?;
            zw.write_all(&body).map_err(err)?;
        }
        Ok(zw.finish().map_err(err)?.into_inner())
    }
}

/// 진단 zip을 만든다(엔진·타임라인·장치·로그 수집 + 마스킹). 블로킹(장치 열거·파일 읽기)이므로 비UI 스레드에서.
pub fn build(
    manager: &EngineManager,
    diag: &Diagnostics,
    app: AppInfo,
    log_dir: Option<&Path>,
    now_ms: u64,
) -> Result<Vec<u8>, String> {
    let (devices, devices_error) = match manager.backend().list_devices() {
        Ok(d) => (d, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let current = manager.snapshot();
    let logs = log_dir.map(|d| collect_logs(d, LOG_BUDGET_BYTES)).unwrap_or_default();
    let os = OsInfo::current();
    // 타임라인 락 안에서는 필요한 값만 꺼내고, 압축(로그 최대 2MB)은 락 밖에서 한다(기록 스레드를 막지 않게).
    let (timeline_info, timeline_jsonl, last_sample, last_config) = diag.with_timeline(|tl, last_config, _| {
        (TimelineInfo::of(tl), to_jsonl(tl), tl.last().cloned(), last_config.cloned())
    });
    let meta = Meta {
        format: 1,
        app,
        os,
        exported_at: now_ms,
        exported_at_iso: iso_utc(now_ms),
        timeline: timeline_info,
    };
    let engine = EngineInfo {
        active: current.is_some(),
        last_stats: match &current {
            Some(c) => Some(Sample::from_stats(now_ms, c.network, c.session, &c.stats)),
            None => last_sample,
        },
        config: current.map(|c| c.config).or(last_config),
        devices,
        devices_error,
    };
    Bundle { meta, engine, timeline_jsonl, logs }.to_zip()
}

/// 저장 대화상자가 돌려준 경로를 실제로 쓸 경로로. 확장자가 `.zip`이 아니면 `.zip`으로 바꾸는데,
/// 대화상자는 바뀐 이름의 덮어쓰기를 확인하지 않았으므로 그 파일이 이미 있으면 `-1`, `-2`… 를 붙인다.
pub fn target_path(picked: PathBuf) -> PathBuf {
    if picked.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
        return picked; // 대화상자가 덮어쓰기를 확인했다
    }
    let mut path = picked;
    path.set_extension("zip");
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    (1..1000)
        .map(|i| path.with_file_name(format!("{stem}-{i}.zip")))
        .find(|p| !p.exists())
        .unwrap_or(path)
}

/// 같은 폴더의 임시 파일에 쓴 뒤 이름을 바꾼다(쓰다 실패해도 잘린 zip이 남지 않게).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let result = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// zip 안 파일 이름: 경로 구분자·상위 경로 없이.
fn sanitize_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let n = n.trim_start_matches('.').to_string();
    if n.is_empty() { "log.txt".into() } else { n }
}

/// 로그 폴더의 `*.log`(회전된 파일 포함)를 최신 것부터 `budget` 바이트까지 끝부분만 읽어 마스킹한다.
/// 결과는 오래된 파일 → 최신 파일 순서.
pub fn collect_logs(dir: &Path, budget: usize) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("log")))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().ok(), e.path()))
        })
        .collect();
    // 최신 먼저
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut left = budget;
    let mut out = Vec::new();
    for (_, path) in files {
        if left == 0 {
            break;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let (text, used) = tail_text(&bytes, left);
        left -= used;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push((name, mask_text(&text)));
    }
    out.reverse();
    out
}

/// 바이트의 끝 `max`바이트를 줄 단위로 잘라 문자열로(앞쪽 잘린 줄은 버린다). (내용, 사용한 바이트)
pub fn tail_text(bytes: &[u8], max: usize) -> (String, usize) {
    if bytes.len() <= max {
        return (String::from_utf8_lossy(bytes).into_owned(), bytes.len());
    }
    let mut tail = &bytes[bytes.len() - max..];
    if let Some(nl) = tail.iter().position(|&b| b == b'\n') {
        tail = &tail[nl + 1..];
    }
    (format!("…(앞부분 생략)\n{}", String::from_utf8_lossy(tail)), max)
}

/// 유닉스 ms → `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, mi, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}Z", ms % 1000)
}

/// 기본 파일 이름 `band-diag-YYYYMMDD-HHMMSS.zip`(UTC).
pub fn default_file_name(ms: u64) -> String {
    let iso = iso_utc(ms);
    let digits: String = iso[..19].chars().filter(|c| c.is_ascii_digit()).collect();
    format!("band-diag-{}-{}.zip", &digits[..8], &digits[8..14])
}

// Howard Hinnant의 days → civil 알고리즘
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::report::EngineStats;
    use std::io::Read;

    fn meta(tl: &Timeline) -> Meta {
        Meta {
            format: 1,
            app: AppInfo { name: "Band".into(), version: "0.1.0".into() },
            os: OsInfo { family: "windows".into(), arch: "x86_64".into(), version: None },
            exported_at: 0,
            exported_at_iso: iso_utc(0),
            timeline: TimelineInfo::of(tl),
        }
    }

    #[test]
    fn iso_and_file_name() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z");
        // 2026-11-11T09:08:07.123Z
        assert_eq!(iso_utc(1_794_388_087_123), "2026-11-11T09:08:07.123Z");
        assert_eq!(default_file_name(1_794_388_087_123), "band-diag-20261111-090807.zip");
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn tail_drops_partial_first_line() {
        let (t, used) = tail_text(b"aaaa\nbbbb\ncccc\n", 7);
        assert_eq!(used, 7);
        assert!(t.ends_with("cccc\n"));
        assert!(!t.contains("bb"));
        let (t, used) = tail_text(b"short", 100);
        assert_eq!((t.as_str(), used), ("short", 5));
    }

    #[test]
    fn sanitize_rejects_paths() {
        assert_eq!(sanitize_name("../../evil.log"), "_.._evil.log");
        assert_eq!(sanitize_name("Band.log"), "Band.log");
        assert_eq!(sanitize_name(""), "log.txt");
    }

    #[test]
    fn collect_logs_masks_and_orders() {
        let dir = std::env::temp_dir().join(format!("band-diag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.log"), "peer 1.2.3.4:5 a@b.com\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("Band.log"), "new line 10.0.0.7\n").unwrap();
        std::fs::write(dir.join("ignore.txt"), "x").unwrap();
        let logs = collect_logs(&dir, 1 << 20);
        std::fs::remove_dir_all(&dir).ok();
        let names: Vec<_> = logs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["old.log", "Band.log"]);
        assert_eq!(logs[0].1, "peer 1.2.x.x:5 <email>\n");
        assert_eq!(logs[1].1, "new line 10.0.x.x\n");
    }

    #[test]
    fn target_path_never_overwrites_unconfirmed_zip() {
        let dir = std::env::temp_dir().join(format!("band-diag-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 사용자가 .zip을 골랐으면(대화상자가 덮어쓰기 확인) 그대로
        assert_eq!(target_path(dir.join("a.zip")), dir.join("a.zip"));
        // 확장자를 바꾼 이름이 비어 있으면 그 이름
        assert_eq!(target_path(dir.join("report.txt")), dir.join("report.zip"));
        assert_eq!(target_path(dir.join("report")), dir.join("report.zip"));
        // 이미 있으면 접미사
        std::fs::write(dir.join("report.zip"), b"old").unwrap();
        std::fs::write(dir.join("report-1.zip"), b"old").unwrap();
        let p = target_path(dir.join("report.txt"));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(p, dir.join("report-2.zip"));
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_part_file() {
        let dir = std::env::temp_dir().join(format!("band-diag-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.zip");
        std::fs::write(&p, b"old").unwrap();
        write_atomic(&p, b"new").unwrap();
        let body = std::fs::read(&p).unwrap();
        let part_left = dir.join("x.zip.part").exists();
        // 폴더가 없으면 실패하고 임시 파일도 없다
        let missing = dir.join("nope").join("y.zip");
        assert!(write_atomic(&missing, b"z").is_err());
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(body, b"new");
        assert!(!part_left);
    }

    #[test]
    fn collect_logs_missing_dir_is_empty() {
        assert!(collect_logs(Path::new("Z:/definitely/not/here"), 100).is_empty());
    }

    #[test]
    fn zip_contains_expected_entries() {
        let mut tl = Timeline::new(10);
        let mut s = EngineStats::stopped();
        s.running = true;
        tl.push(Sample::from_stats(1_000, true, 1, &s));
        tl.push(Sample::stopped(2_000, true, 1));
        let engine = EngineInfo {
            active: false,
            config: Some(EngineConfig::default()),
            last_stats: tl.last().cloned(),
            devices: Vec::new(),
            devices_error: None,
        };
        let b = Bundle::new(meta(&tl), engine, &tl, vec![("Band.log".into(), "hello\n".into())]);
        let bytes = b.to_zip().unwrap();
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().unwrap().to_string()).collect();
        assert_eq!(names, vec!["README.txt", "meta.json", "engine.json", "timeline.jsonl", "logs/Band.log"]);

        let read = |z: &mut zip::ZipArchive<Cursor<Vec<u8>>>, n: &str| {
            let mut s = String::new();
            z.by_name(n).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        let meta: serde_json::Value = serde_json::from_str(&read(&mut z, "meta.json")).unwrap();
        assert_eq!(meta["app"]["version"], "0.1.0");
        assert_eq!(meta["timeline"]["samples"], 2);
        assert_eq!(meta["timeline"]["firstT"], 1000);
        let engine: serde_json::Value = serde_json::from_str(&read(&mut z, "engine.json")).unwrap();
        assert_eq!(engine["config"]["bufferSize"], 128);
        assert_eq!(engine["devices"], serde_json::json!([]));
        assert_eq!(read(&mut z, "timeline.jsonl").lines().count(), 2);
        assert_eq!(read(&mut z, "logs/Band.log"), "hello\n");
    }
}
