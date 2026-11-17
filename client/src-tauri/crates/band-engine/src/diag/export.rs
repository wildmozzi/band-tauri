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
use super::timeline::{session_placement, to_jsonl, Sample, Timeline, SAMPLE_INTERVAL_MS};
use super::Diagnostics;
use band_dsp::config::EngineConfig;
use crate::engine::device::DeviceInfo;
use crate::engine::report::EngineSnapshot;

/// 로그 전체 상한(마스킹 전 바이트). 최신 파일의 끝부분부터 채운다.
pub(crate) const LOG_BUDGET_BYTES: usize = 2 * 1024 * 1024;

/// `meta.json`의 `format`. 1 → 2(P1-21a): 타임라인 `corePlacement`가 비율 객체(#191)에서 구간 표본 수 `[P, E, LP-E]`(#203)로
/// 바뀌었고, `engine.json` `lastStats.corePlacement`는 늘 세션 시작부터 누적이다.
pub(crate) const FORMAT: u32 = 2;

pub(crate) const README_TEXT: &str = "Band 진단 자료 (P2-5)

이 파일은 합주 중 끊김·지연 원인을 찾기 위한 자료입니다. 개발자에게 보내 주세요.

meta.json       앱 버전, OS 종류·버전, 내보낸 시각, 타임라인 범위
engine.json     엔진 설정(버퍼·프레임·코덱·지터 범위), 마지막 엔진 통계, 오디오 장치 목록(이름·드라이버·채널·레이트)
timeline.jsonl  1초 간격 엔진 통계(최근 최대 60분): 백엔드·장치 버퍼·지연·xrun·스트림 오류,
                피어별 경로(lan/public/relay)·RTT·지터·버퍼·재생 지연·손실·늦음·은닉(누적)
                session 값이 바뀐 곳은 엔진 재시작이라 누적 값이 0부터 다시 셉니다.
                corePlacement(Windows)는 워커·수신 스레드가 깨어날 때 P/E/LP-E 코어 중 어디에 있었는지 센
                [P, E, LP-E] 표본 수입니다. 줄마다 직전 줄 뒤로 센 구간 값이고(실행 시간이 아니라 깨어난 횟수),
                세션의 줄을 모두 더하면 시작부터의 누적입니다. engine.json lastStats의 corePlacement는 누적입니다.
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
pub(crate) struct OsInfo {
    /// `windows` | `macos` | `linux` …
    pub(crate) family: String,
    pub(crate) arch: String,
    /// 예: `Microsoft Windows [Version 10.0.26100.1234]`(얻지 못하면 null)
    pub(crate) version: Option<String>,
}

impl OsInfo {
    pub(crate) fn current() -> Self {
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
pub(crate) struct TimelineInfo {
    pub(crate) interval_ms: u64,
    pub(crate) capacity: usize,
    pub(crate) samples: usize,
    /// 용량 초과로 버린 샘플 수
    pub(crate) dropped: u64,
    pub(crate) first_t: Option<u64>,
    pub(crate) last_t: Option<u64>,
}

impl TimelineInfo {
    pub(crate) fn of(tl: &Timeline) -> Self {
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
pub(crate) struct Meta {
    pub(crate) format: u32,
    pub(crate) app: AppInfo,
    pub(crate) os: OsInfo,
    pub(crate) exported_at: u64,
    pub(crate) exported_at_iso: String,
    pub(crate) timeline: TimelineInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngineInfo {
    /// 지금 엔진이 돌고 있는지(아니면 아래 값은 마지막 세션 것)
    pub(crate) active: bool,
    /// 현재 또는 마지막 세션의 엔진 설정(장치 id = 장치 이름)
    pub(crate) config: Option<EngineConfig>,
    /// 마지막 통계(피어 id·릴레이 주소 마스킹)
    pub(crate) last_stats: Option<Sample>,
    pub(crate) devices: Vec<DeviceInfo>,
    /// 장치 열거 실패 사유
    pub(crate) devices_error: Option<String>,
}

/// zip에 들어갈 내용 전부(이미 마스킹된 값).
pub(crate) struct Bundle {
    pub(crate) meta: Meta,
    pub(crate) engine: EngineInfo,
    pub(crate) timeline_jsonl: String,
    /// (파일 이름, 마스킹된 내용)
    pub(crate) logs: Vec<(String, String)>,
}

impl Bundle {
    #[cfg(test)]
    pub(crate) fn new(meta: Meta, engine: EngineInfo, timeline: &Timeline, logs: Vec<(String, String)>) -> Self {
        Self { meta, engine, timeline_jsonl: to_jsonl(timeline), logs }
    }

    /// zip 바이트로. 항목 순서: README, meta, engine, timeline, logs.
    pub(crate) fn to_zip(&self) -> Result<Vec<u8>, String> {
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

/// 진단 zip을 만든다(타임라인·로그 수집 + 마스킹). 블로킹(파일 읽기)이므로 비UI 스레드에서.
/// `current`(지금 엔진 스냅샷)와 `devices`(장치 열거 결과)는 부르는 쪽(어댑터)이 엔진에서 얻어 넘긴다 —
/// 한 번 쓰는 값이라 클로저가 아니라 값으로 받는다. `diag`는 `manager`를 모른다(엔진 ⑤).
pub fn build(
    current: Option<EngineSnapshot>,
    devices: Result<Vec<DeviceInfo>, String>,
    diag: &Diagnostics,
    app: AppInfo,
    log_dir: Option<&Path>,
    now_ms: u64,
) -> Result<Vec<u8>, String> {
    let (devices, devices_error) = match devices {
        Ok(d) => (d, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let logs = log_dir.map(|d| collect_logs(d, LOG_BUDGET_BYTES)).unwrap_or_default();
    let os = OsInfo::current();
    // 타임라인 락 안에서는 필요한 값만 꺼내고, 압축(로그 최대 2MB)은 락 밖에서 한다(기록 스레드를 막지 않게).
    // 엔진이 없을 때 `lastStats`는 타임라인 마지막 샘플이다. 엔진이 막 사라진 뒤 정지 표시 전(최대 1초)이면 그것이 실행 중 샘플이라
    // `corePlacement`가 구간 값이 된다(#203 리뷰 L2). 그래서 그 세션의 줄을 더해 늘 시작부터 누적으로 맞춘다(현재 엔진의
    // `from_stats`와 같은 뜻 — 정지 표시 샘플이 마지막이어도 직전 세션의 누적이 들어간다).
    let (timeline_info, timeline_jsonl, last_sample, last_config) = diag.with_timeline(|tl, last_config, _| {
        let last = tl.last().cloned().map(|mut s| {
            s.core_placement = session_placement(tl, s.session);
            s
        });
        (TimelineInfo::of(tl), to_jsonl(tl), last, last_config.cloned())
    });
    let meta = Meta {
        format: FORMAT,
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
    crate::sys::fs::write_replace(path, bytes, ".part")
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
pub(crate) fn collect_logs(dir: &Path, budget: usize) -> Vec<(String, String)> {
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
pub(crate) fn tail_text(bytes: &[u8], max: usize) -> (String, usize) {
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
pub(crate) fn iso_utc(ms: u64) -> String {
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
            format: FORMAT,
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
    /// 고정 입력(타임라인 2샘플, 엔진 없음, 가짜 장치 2개, 로그 1개)으로 만든 zip 항목별 SHA-256.
    /// meta.json의 `os`는 실행 환경에 따라 다르므로 빼고 잰다. 기대값은 엔진 ⑤ 이전 코드(`build(&EngineManager, ..)`)로 뽑았다.
    fn golden_digest(bytes: Vec<u8>) -> Vec<(String, String)> {
        use sha2::{Digest, Sha256};
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..z.len())
            .map(|i| {
                let mut f = z.by_index(i).unwrap();
                let name = f.name().unwrap().to_string();
                let mut body = Vec::new();
                f.read_to_end(&mut body).unwrap();
                if name == "meta.json" {
                    let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    v.as_object_mut().unwrap().remove("os");
                    body = serde_json::to_vec_pretty(&v).unwrap();
                }
                let h: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
                (name, h)
            })
            .collect()
    }

    fn golden_inputs(tag: &str) -> (Diagnostics, std::path::PathBuf) {
        use crate::engine::report::EngineStats;
        let d = Diagnostics::new(10);
        let mut stats = EngineStats::stopped();
        stats.running = true;
        d.record(1_000, Some(EngineSnapshot { stats, config: EngineConfig::default(), network: true, session: 3, conn_events: vec![] }));
        d.record(2_000, None);
        let dir = std::env::temp_dir().join(format!("band-diag-golden-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Band.log"), "user a@b.com from 1.2.3.4:5\n").unwrap();
        (d, dir)
    }

    /// 엔진 ⑤ 동작 불변: 같은 입력이면 zip 항목·내용이 ⑤ 이전과 같다(기대값은 이전 코드로 뽑은 해시).
    #[test]
    fn build_matches_pre_stage5_golden() {
        use crate::engine::device::fake::FakeBackend;
        use crate::engine::device::AudioBackend;
        let (d, dir) = golden_inputs("new");
        let app = AppInfo { name: "Band".into(), version: "0.1.0".into() };
        let devices = FakeBackend::silence(48_000).list_devices();
        let bytes = build(None, devices, &d, app, Some(&dir), 1_794_388_087_123).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let expected = [
            // P1-21a: README에 corePlacement 설명, meta.format 1 → 2(의도한 변경 — engine.json·timeline.jsonl 해시는 그대로)
            ("README.txt", "53892e8d86178fe2d6875d8489e2f0bdbd8108ee1e19e9029e3a7e835c0b31a6"),
            ("meta.json", "867d233d696502bd59c5633c283b652e9262fafe3ea585ca3bbc247a158a6aee"),
            ("engine.json", "eff8c630e2dfc6b97ac0f06328dff69d196a03a6087e1b0049ce0309cc36370e"),
            ("timeline.jsonl", "3439e58dad4fa31e0ad39d03171b9c5752e3ed097d82404a7ba3fee74224769a"),
            ("logs/Band.log", "6774d10d922e095e6e4dab8cd7dc9a152b767e4d69a2c4af13ffe7cc8359e982"),
        ];
        let got = golden_digest(bytes);
        let got: Vec<(&str, &str)> = got.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
        assert_eq!(got, expected);
    }

    /// 지금 엔진 스냅샷을 넘기면 그것이 engine.json의 현재 값이 된다(타임라인 마지막 값보다 우선), 장치 열거 실패는 사유로.
    #[test]
    fn build_uses_current_snapshot_and_device_error() {
        use crate::engine::report::EngineStats;
        let (d, dir) = golden_inputs("cur");
        let mut stats = EngineStats::stopped();
        stats.running = true;
        let cfg = EngineConfig { buffer_size: 256, ..EngineConfig::default() };
        let cur = EngineSnapshot { stats, config: cfg, network: false, session: 8, conn_events: vec![] };
        let app = AppInfo { name: "Band".into(), version: "0.1.0".into() };
        let bytes = build(Some(cur), Err("열거 실패".into()), &d, app, None, 5_000).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        z.by_name("engine.json").unwrap().read_to_string(&mut s).unwrap();
        let e: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(e["active"], true);
        assert_eq!(e["config"]["bufferSize"], 256);
        assert_eq!(e["lastStats"]["t"], 5_000);
        assert_eq!(e["lastStats"]["session"], 8);
        assert_eq!(e["devices"], serde_json::json!([]));
        assert_eq!(e["devicesError"], "열거 실패");
        assert!(z.by_name("logs/Band.log").is_err(), "log_dir 없으면 로그 없음");
    }

    /// 엔진이 막 사라지고 정지 표시 전(최대 1초)에 내보내면 마지막 샘플이 실행 중 샘플이다. 그래도 `lastStats.corePlacement`는
    /// 구간 값이 아니라 세션 시작부터 누적이다(#203 리뷰 L2). 정지 표시가 마지막이어도 직전 세션 누적이 들어간다. meta.format 2.
    #[test]
    fn last_stats_core_placement_is_cumulative_after_engine_gone() {
        use crate::engine::report::EngineStats;
        use crate::sys::placement::{CorePlacementStats, ThreadPlacement};
        let at = |w: [u64; 3]| {
            let mut stats = EngineStats::stopped();
            stats.running = true;
            stats.core_placement = Some(CorePlacementStats { worker: ThreadPlacement::from_counts(w[0], w[1], w[2]), net_recv: Some(ThreadPlacement::from_counts(w[0], 0, 0)) });
            EngineSnapshot { stats, config: EngineConfig::default(), network: true, session: 4, conn_events: vec![] }
        };
        let d = Diagnostics::new(10);
        d.record(1_000, Some(at([100, 0, 0])));
        d.record(2_000, Some(at([150, 30, 20])));
        let last = |d: &Diagnostics| -> (serde_json::Value, serde_json::Value) {
            let app = AppInfo { name: "Band".into(), version: "0.1.0".into() };
            let mut z = zip::ZipArchive::new(Cursor::new(build(None, Ok(Vec::new()), d, app, None, 2_500).unwrap())).unwrap();
            let (mut e, mut m) = (String::new(), String::new());
            z.by_name("engine.json").unwrap().read_to_string(&mut e).unwrap();
            z.by_name("meta.json").unwrap().read_to_string(&mut m).unwrap();
            let e: serde_json::Value = serde_json::from_str(&e).unwrap();
            (e["lastStats"]["corePlacement"].clone(), serde_json::from_str::<serde_json::Value>(&m).unwrap()["format"].clone())
        };
        // 정지 표시 전: 마지막 줄은 구간 값 [50, 30, 20]이지만 lastStats는 누적
        let (cp, format) = last(&d);
        assert_eq!(cp, serde_json::json!({"worker": [150, 30, 20], "netRecv": [150, 0, 0]}));
        assert_eq!(format, 2);
        // 정지 표시 뒤
        d.record(3_000, None);
        assert_eq!(last(&d).0, serde_json::json!({"worker": [150, 30, 20], "netRecv": [150, 0, 0]}));
    }

    /// 진단 zip의 timeline.jsonl 피어 샘플과 engine.json 마지막 샘플에 입→귀 추정 항(P2-8b)이 들어간다.
    /// 둘째 피어는 옛 엔진(두 값 없음) → null.
    #[test]
    fn zip_timeline_has_remote_ear_terms() {
        use super::super::timeline::tests::stats_with_peer;
        let d = Diagnostics::new(10);
        let mut stats = stats_with_peer();
        let mut old = stats.peers[0].clone();
        old.peer_id = "old-engine-peer".into();
        old.remote_frame_ms = None;
        old.remote_input_buffer_ms = None;
        stats.peers.push(old);
        d.record(1_000, Some(EngineSnapshot { stats, config: EngineConfig::default(), network: true, session: 1, conn_events: vec![] }));
        let app = AppInfo { name: "Band".into(), version: "0.1.0".into() };
        let bytes = build(None, Ok(Vec::new()), &d, app, None, 2_000).unwrap();
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        z.by_name("timeline.jsonl").unwrap().read_to_string(&mut s).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 1);
        let line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let peers = line["peers"].as_array().unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0]["remoteFrameMs"], 5.0);
        assert!((peers[0]["remoteInputBufferMs"].as_f64().unwrap() - 2.67).abs() < 1e-4);
        assert!(peers[1]["remoteFrameMs"].is_null() && peers[1].get("remoteFrameMs").is_some());
        assert!(peers[1]["remoteInputBufferMs"].is_null() && peers[1].get("remoteInputBufferMs").is_some());

        let mut e = String::new();
        z.by_name("engine.json").unwrap().read_to_string(&mut e).unwrap();
        let e: serde_json::Value = serde_json::from_str(&e).unwrap();
        assert_eq!(e["lastStats"]["peers"][0]["remoteFrameMs"], 5.0);
    }
}
