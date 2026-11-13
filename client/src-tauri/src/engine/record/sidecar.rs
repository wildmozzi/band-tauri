//! 녹음 계약 DTO(`engine_record_*`, `session.json`, `markers.json`)와 세션 폴더 이름.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::marks::MarkKind;
use crate::diag::mask::pseudonymize;
use crate::engine::wav::RecordFormat;

/// `engine_record_start` 옵션.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordOptions {
    pub format: RecordFormat,
    /// 믹스 트랙(내가 들은 피어 게인·팬 + 내 입력, 클릭 없음)도 저장.
    pub include_mix: bool,
    /// 세션 폴더 이름(파일 이름에 못 쓰는 문자는 `_`). 없으면 `band-rec-<UTC 시각>`.
    pub name: Option<String>,
    /// 내 입력 트랙 이름(기본 "me").
    pub local_name: Option<String>,
    /// 피어별 트랙 이름(peerId → 표시 이름). 없으면 peerId 가명(`id-xxxxxxxx`, PR #62 L5).
    pub track_names: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Local,
    Peer,
    Mix,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordTrack {
    pub kind: TrackKind,
    pub name: String,
    pub file: String,
    /// 피어 id 가명(`id-` + 해시 8자, 진단 내보내기 `diag::mask::pseudonymize`와 같은 값 — 같은 id는 같은 값, PR #62 L5).
    /// `session.json`·`markers.json`·`engine_record_stop` 반환 모두 가명이다. 원문은 녹음 중 쓰기 스레드 안에만 있다.
    pub peer_id: Option<String>,
}

/// 쓰기 스레드 안의 트랙 메타 → 밖으로 내보내는 형태(피어 id 가명화). 표시 이름(닉네임)·파일 이름은 그대로 둔다.
pub(super) fn public_track(t: &RecordTrack) -> RecordTrack {
    RecordTrack { peer_id: t.peer_id.as_deref().map(pseudonymize), ..t.clone() }
}

/// `engine_record_stop` 반환(세션 폴더의 `session.json`과 같은 내용).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSummary {
    pub folder: String,
    pub format: RecordFormat,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_sec: f64,
    pub bytes: u64,
    pub dropped_frames: u64,
    pub started_at_ms: u64,
    pub tracks: Vec<RecordTrack>,
    pub error: Option<String>,
    /// 은닉·버림·언더런 구간 수(모든 트랙 합, 가까운 것은 합침). 자세한 위치는 `markersFile`(P2-4a).
    pub markers: u32,
    /// 마커 사이드카 파일 이름(세션 폴더 안, 보통 `markers.json`). 쓰지 못했으면 None.
    pub markers_file: Option<String>,
}

/// `markers.json`의 구간 하나. 위치는 녹음 시작 기준 프레임(트랙 파일의 샘플 위치와 같음).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordMarker {
    pub kind: MarkKind,
    pub start: u64,
    /// 끝(제외). `drop`은 길이 없는 지점이라 `start`와 같다.
    pub end: u64,
    pub start_sec: f64,
    pub end_sec: f64,
    /// 은닉·0 채움·버린 오디오의 양(프레임, 합친 이벤트의 합).
    pub frames: u64,
    /// 합친 이벤트 수.
    pub events: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerTrack {
    pub file: String,
    pub name: String,
    pub kind: TrackKind,
    pub peer_id: Option<String>,
    pub markers: Vec<RecordMarker>,
}

/// `markers.json` 전체.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkersFile {
    pub sample_rate: u32,
    /// 같은 종류 이벤트를 이 간격(프레임) 안이면 한 구간으로 합쳤다.
    pub merge_gap_frames: u64,
    /// 워커 이벤트 위치 허용 오차(프레임, ±). 언더런은 청크 머리라 정확하다. 은닉 마커는 실측으로 실제 위치보다 0~1패킷
    /// 이르다(PR #62 L1, `tests/record.rs` `concealment_marker_positions_match_recorded_concealment`).
    pub accuracy_frames: u64,
    /// 큐가 가득 차 잃은 워커 이벤트 수.
    pub lost_events: u64,
    /// 트랙·종류별 구간 상한(`MARK_TRACK_CAP`)을 넘어 구간으로 남기지 못하고 세기만 한 이벤트 수(PR #62 M2).
    pub overflow_events: u64,
    pub tracks: Vec<MarkerTrack>,
}

/// `EngineStats.recording`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordStats {
    pub folder: String,
    pub format: RecordFormat,
    pub sample_rate: u32,
    pub duration_sec: f64,
    pub bytes: u64,
    pub tracks: u32,
    pub dropped_frames: u64,
    pub error: Option<String>,
}

/// 파일·폴더 이름에 쓸 수 있게 고친다(Windows 금지 문자·제어 문자 → `_`, 끝 점·공백 제거, 60자).
pub fn sanitize_name(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .take(60)
        .collect();
    let t = mapped.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_string();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
        "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = t.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        format!("_{t}")
    } else {
        t
    }
}

/// `band-rec-YYYYMMDD-HHMMSSZ`(UTC). 프론트가 `name`으로 지역 시각을 넘기는 것이 보통이다.
pub fn default_session_name(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("band-rec-{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
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
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `dir/base`가 있으면 `base (2)`, `base (3)`… 중 없는 것을 만든다.
pub(super) fn unique_dir(dir: &Path, base: &str) -> std::io::Result<PathBuf> {
    for k in 1..1000 {
        let name = if k == 1 { base.to_string() } else { format!("{base} ({k})") };
        let p = dir.join(name);
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("같은 이름의 녹음 폴더가 너무 많습니다"))
}
