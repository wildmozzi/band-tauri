//! 진단 zip → 결과표(GOAL-COVERAGE 후보 ②·③). 사람이 실장비·실회선 세션 뒤 "진단 내보내기" zip만 보내면 코디네이터가
//! `cargo run -p band-engine --example diag_report -- <zip>...`으로 GOAL 판정에 필요한 숫자를 얻는다.
//!
//! 읽는 것(`export.rs` 머리말의 zip 구성): `meta.json`(앱 버전·형식·시각), `engine.json`(마지막 세션 설정 — 드라이버·프리셋·
//! 프레임), `timeline.jsonl`(1초 샘플 — 세션·백엔드·xrun·코어 배치·피어·`conn`). 줄은 `serde_json::Value`로 읽고 필드가 없으면
//! "없음"으로 둔다 — 옛 형식(P1-18 `conn`, P2-17h `lossShape`, P1-21 `corePlacement` 이전)도 죽지 않는다. 형식이 바뀌어 필드
//! 이름이 달라지면 시험 `report_reads_real_export`(실제 `diag::export::build`로 만든 zip을 읽는다)가 깨진다. 설정은 `EngineConfig`,
//! 손실 모양은 `LossShapeStats` 타입으로 그대로 역직렬화한다.
//!
//! 판정은 [`goal`]의 기준값(GOAL.md 절 번호 주석)과 나란히 놓고 충족/미달/판정 불가로 적는다. 데이터가 없거나 세션이 기준보다
//! 짧으면 "판정 불가"와 이유를 적는다 — 추측으로 충족이라 쓰지 않는다.
//!
//! 도구 전용 모듈이다(`test-support` 피처 — 예제·시험 빌드에서만 들어가고 제품 구성에는 없다). RT 경로와 무관하다.

mod render;
pub mod shape;

use std::collections::BTreeMap;
use std::io::Read;

use band_dsp::config::EngineConfig;
use band_dsp::jitter::LossShapeStats;
use serde::Serialize;
use serde_json::Value;

pub(crate) use render::markdown;

/// GOAL.md 기준값(한 곳에 모은다). 바뀌면 GOAL.md와 같이 고친다.
pub(crate) mod goal {
    /// 1절 "로컬 처리 지연": ASIO 128샘플/2.5ms 프레임 ≤ 8ms.
    pub(crate) const EST_ASIO_MS: f64 = 8.0;
    pub(crate) const EST_ASIO_BUFFER: u32 = 128;
    pub(crate) const EST_ASIO_FRAME_MS: f64 = 2.5;
    /// 1절 "로컬 처리 지연": WASAPI 독점 ≤ 15ms.
    pub(crate) const EST_EXCLUSIVE_MS: f64 = 15.0;
    /// 1절 "로컬 처리 지연": WASAPI 공유 ≤ 30ms.
    pub(crate) const EST_SHARED_MS: f64 = 30.0;
    /// 1절 "지연 누적": `bufferMs`가 목표 ±3ms 밖에 1분 이상 머무르지 않음(60분 동안).
    pub(crate) const BAND_MS: f64 = 3.0;
    pub(crate) const BAND_MAX_S: f64 = 60.0;
    pub(crate) const BAND_SESSION_S: f64 = 3600.0;
    /// 1절 "지연 누적" 연속 구간 히스테리시스(s): 띠 안 복귀가 이만큼 이하면 같은 구간으로 이어 센다(`tests/sim/long.rs`와 같다).
    pub(crate) const BAND_GAP_S: f64 = 1.0;
    /// 2절 "손실 1% 회선에서 은닉 프레임 비율 ≤ 1.5%". 회선 손실이 조건(1%)보다 크면 판정하지 않는다.
    pub(crate) const CONCEAL_MAX_PCT: f64 = 1.5;
    pub(crate) const CONCEAL_LOSS_COND_PCT: f64 = 1.0;
    /// 2절 "권장 설정(WASAPI 독점) xrun 30분 동안 0회".
    pub(crate) const XRUN_WINDOW_S: f64 = 1800.0;
    /// 3절 "직접 연결 성공률": 콘형 NAT 쌍 100%.
    pub(crate) const DIRECT_PCT: f64 = 100.0;
    /// 3절 "직접 연결 실패 시 릴레이 폴백 — 연결 성공률 ≥ 98%".
    pub(crate) const CONNECT_PCT: f64 = 98.0;
    /// 3절 "연결 소요 시간: 입장 후 3초 이내 소리".
    pub(crate) const FIRST_AUDIO_MS: f64 = 3000.0;
    /// 도구 규칙(GOAL에 없음): 은닉·지터 버퍼 판정에 필요한 최소 연결 시간(s). 이보다 짧으면 판정 불가.
    pub(crate) const MIN_JUDGE_S: f64 = 60.0;
    /// 도구 규칙(GOAL에 없음): 성공률 98%를 "충족"으로 가르려면 시도 50회 이상(1회 실패 = 2%p). 그보다 적으면 실패가
    /// 있어 98% 아래일 때만 "미달"이다.
    pub(crate) const MIN_ATTEMPTS_98: usize = 50;
}

/// 판정.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Verdict {
    Met,
    NotMet,
    Unknown,
}

impl Verdict {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Verdict::Met => "충족",
            Verdict::NotMet => "미달",
            Verdict::Unknown => "판정 불가",
        }
    }
}

/// GOAL 판정표 한 줄.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoalRow {
    /// GOAL.md 절-행(예: `1-1`)
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) measured: String,
    pub(crate) verdict: Verdict,
    /// 판정 불가 이유, 또는 판정에 붙는 주의
    pub(crate) note: Option<String>,
}

fn row(id: &str, name: &str, target: String, measured: String, verdict: Verdict, note: Option<String>) -> GoalRow {
    GoalRow { id: id.into(), name: name.into(), target, measured, verdict, note }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Dist {
    pub(crate) median: f64,
    pub(crate) p95: f64,
    pub(crate) max: f64,
}

impl Dist {
    fn of(v: &[f64]) -> Option<Self> {
        if v.is_empty() {
            return None;
        }
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        Some(Self { median: median_sorted(&s), p95: s[((s.len() as f64 * 0.95).ceil() as usize).clamp(1, s.len()) - 1], max: s[s.len() - 1] })
    }
}

fn median_sorted(s: &[f64]) -> f64 {
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 }
}

fn median(v: &[f64]) -> Option<f64> {
    Dist::of(v).map(|d| d.median)
}

/// 설정 요약(`engine.json` `config` — 마지막 세션 것만 있다).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigSummary {
    pub(crate) input_driver: String,
    pub(crate) output_driver: String,
    pub(crate) buffer_size: u32,
    pub(crate) frame_ms: f64,
    pub(crate) codec: String,
    pub(crate) channels: u8,
    /// 앱 프리셋(`client/src/native/engineConfig.ts` `ENGINE_PRESETS`)과 프레임·코덱·채널·지터 범위가 같으면 그 이름
    pub(crate) preset: String,
    pub(crate) jitter_profile: Option<f32>,
}

/// 앱 프리셋 표(`ENGINE_PRESETS`): (이름, 프레임, 코덱, 채널, 지터 최소, 지터 최대).
const PRESETS: [(&str, f32, &str, u8, f32, f32); 3] =
    [("초저지연", 2.5, "pcm16", 1, 0.0, 40.0), ("균형", 5.0, "opus", 1, 0.0, 80.0), ("안정", 10.0, "opus", 2, 20.0, 150.0)];

fn enum_name<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "?".into())
}

impl ConfigSummary {
    fn of(c: &EngineConfig) -> Self {
        let codec = enum_name(&c.codec);
        let preset = PRESETS
            .iter()
            .find(|p| p.1 == c.frame_ms && p.2 == codec && p.3 == c.channels && c.jitter_min_ms == Some(p.4) && c.jitter_max_ms == Some(p.5))
            .map(|p| p.0.to_string())
            .unwrap_or_else(|| "사용자 지정".into());
        Self {
            input_driver: enum_name(&c.input_driver),
            output_driver: enum_name(&c.output_driver),
            buffer_size: c.buffer_size,
            frame_ms: c.frame_ms as f64,
            codec,
            channels: c.channels,
            preset,
            jitter_profile: c.jitter_profile,
        }
    }
}

/// 세션 동안 워커·수신 스레드의 코어 배치 표본 수 `[P, E, LP-E]`(타임라인 구간 값의 합 = 세션 누적).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Placement {
    pub(crate) worker: [u64; 3],
    pub(crate) net_recv: Option<[u64; 3]>,
    /// meta.format 1(비율 객체, #191) 줄이 섞여 있어 합에서 뺐다
    pub(crate) legacy_lines: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct XrunStep {
    /// 세션 시작부터(s)
    pub(crate) at_s: f64,
    pub(crate) add: u64,
}

/// ±3ms 띠 분석(GOAL 1-4).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Band {
    /// ±3ms 밖에 1분 이상 머문 구간 수
    pub(crate) over_limit: usize,
    pub(crate) longest_s: f64,
    pub(crate) outside_pct: f64,
}

/// 은닉·손실(연결 구간 차분 — 피어 항목마다 첫·마지막 샘플 사이).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Conceal {
    /// 기대 프레임 수(경과 시간 ÷ F)
    pub(crate) frames: f64,
    pub(crate) seconds: f64,
    pub(crate) pct: f64,
    /// 방식별(lossShape 없으면 None)
    pub(crate) fec_pct: Option<f64>,
    pub(crate) plc_pct: Option<f64>,
    pub(crate) pcm_pct: Option<f64>,
    pub(crate) underrun_pct: Option<f64>,
    /// 확정 손실(lossShape 추정 손실 ÷ 기대 프레임). lossShape 없으면 None
    pub(crate) loss_pct: Option<f64>,
}

/// 손실 모양 요약(F마다).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShapeSummary {
    pub(crate) frame_ms: f64,
    pub(crate) loss_runs: [u64; band_dsp::jitter::RUN_BUCKETS],
    pub(crate) late_runs: [u64; band_dsp::jitter::RUN_BUCKETS],
    pub(crate) lost: f64,
    pub(crate) late: f64,
    /// 도착 여유 ≥ F 비율(1F 늦춘 복사본·FEC가 마감 안에 드는 비율)
    pub(crate) margin_ge_f: Option<f64>,
    pub(crate) dup_1f_of_loss: Option<f64>,
}

impl ShapeSummary {
    fn of(sh: &LossShapeStats) -> Self {
        let e = sh.estimate();
        Self {
            frame_ms: sh.frame_ms as f64,
            loss_runs: sh.loss_runs,
            late_runs: sh.late_runs,
            lost: e.lost,
            late: e.late,
            margin_ge_f: e.margin_ge_f,
            dup_1f_of_loss: e.dup_1f_of_loss,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerReport {
    pub(crate) peer: String,
    /// 피어 항목 수(나갔다 다시 들어오면 늘어난다)
    pub(crate) entries: usize,
    /// 연결(`connected`) 샘플 수
    pub(crate) connected_samples: usize,
    /// 경로별 연결 샘플 수(lan/public/relay)
    pub(crate) paths: BTreeMap<String, usize>,
    pub(crate) rtt_ms: Option<Dist>,
    pub(crate) buffer_ms: Option<Dist>,
    pub(crate) jitter_ms: Option<Dist>,
    pub(crate) target_ms: Option<Dist>,
    /// 받는 프레임(ms): lossShape `frameMs` → `remoteFrameMs`
    pub(crate) frame_ms: Option<f64>,
    pub(crate) band: Option<Band>,
    pub(crate) conceal: Option<Conceal>,
    /// 엔진 `lossPct`(창 값) 평균
    pub(crate) loss_pct_engine_mean: Option<f64>,
    pub(crate) connect_ms: Vec<f64>,
    pub(crate) first_audio_ms: Vec<f64>,
    pub(crate) loss_shape: Vec<ShapeSummary>,
}

impl PeerReport {
    /// 연결 샘플이 가장 많은 경로
    pub(crate) fn main_path(&self) -> Option<&str> {
        self.paths.iter().max_by_key(|(_, n)| **n).map(|(p, _)| p.as_str())
    }
}

/// 연결 기록 한 줄(`conn`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnRow {
    pub(crate) at_s: f64,
    pub(crate) peer: String,
    pub(crate) phase: String,
    pub(crate) result: String,
    pub(crate) ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionReport {
    pub(crate) session: u64,
    /// 합주(네트워크) 세션이면 true, 테스트 모드면 false
    pub(crate) network: Option<bool>,
    pub(crate) start: Option<String>,
    pub(crate) end: Option<String>,
    /// 첫 실행 샘플 → 마지막 실행 샘플(s)
    pub(crate) duration_s: f64,
    pub(crate) running_samples: usize,
    pub(crate) backend: Option<String>,
    pub(crate) sample_rate: Option<u64>,
    pub(crate) device_buffer: Option<u64>,
    pub(crate) device_period_ms: Option<f64>,
    /// `engine.json` 설정(이 세션이 마지막 세션일 때만)
    pub(crate) config: Option<ConfigSummary>,
    pub(crate) est_latency_ms: Option<Dist>,
    pub(crate) core_placement: Option<Placement>,
    pub(crate) thread_policy: Option<Value>,
    /// 세션 누적 xrun(마지막 실행 샘플)
    pub(crate) xruns: Option<u64>,
    pub(crate) xrun_steps: Vec<XrunStep>,
    pub(crate) stream_errors: Option<u64>,
    pub(crate) peers: Vec<PeerReport>,
    pub(crate) conn: Vec<ConnRow>,
    pub(crate) goal: Vec<GoalRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileReport {
    pub(crate) source: String,
    /// `meta.json` `format`(P1-21a부터 2). 없으면 None
    pub(crate) format: Option<u64>,
    pub(crate) app_version: Option<String>,
    pub(crate) os: Option<String>,
    pub(crate) exported_at: Option<String>,
    /// 타임라인 링이 넘쳐 버린 샘플 수(60분 초과)
    pub(crate) timeline_dropped: Option<u64>,
    /// JSON이 아닌 타임라인 줄 수
    pub(crate) bad_lines: usize,
    pub(crate) sessions: Vec<SessionReport>,
}

/// 결과 경로별 연결 집계 한 줄.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnPathRow {
    /// lan | public | relay | failed | aborted
    pub(crate) result: String,
    pub(crate) connect: usize,
    pub(crate) connect_median_ms: Option<f64>,
    pub(crate) reconnect: usize,
    pub(crate) reconnect_median_ms: Option<f64>,
}

/// 연결 성공률 집계(후보 ③). 시도 = `connect` + `reconnect`(설계상 시도 하나에 결과 하나). `recover`·`switch`는 시도가 아니다.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnAgg {
    pub(crate) sessions: usize,
    pub(crate) rows: Vec<ConnPathRow>,
    pub(crate) attempts: usize,
    pub(crate) success: usize,
    pub(crate) direct: usize,
    pub(crate) relay: usize,
    pub(crate) failed: usize,
    pub(crate) aborted: usize,
    /// 성공 ÷ 시도(중단 포함 분모 — 빼면 "연결 중" 퇴장이 빠져 위로 치우친다, `net::connlog` 머리말)
    pub(crate) success_pct: Option<f64>,
    /// 직접(lan/public) ÷ 시도
    pub(crate) direct_pct: Option<f64>,
    pub(crate) recover: usize,
    pub(crate) switch: usize,
    /// 릴레이로 붙은(시도·recover·switch 결과 relay) 세션 수
    pub(crate) sessions_with_relay: usize,
    /// 첫 소리(ms, 피어 항목마다 `firstAudioMs`)
    pub(crate) first_audio_ms: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    pub(crate) files: Vec<FileReport>,
    pub(crate) conn: ConnAgg,
    /// 3절(연결) 판정 — 모든 zip을 합친 값
    pub(crate) goal: Vec<GoalRow>,
}

/// 읽은 진단 zip.
pub(crate) struct Diag {
    pub(crate) source: String,
    pub(crate) meta: Option<Value>,
    pub(crate) engine: Option<Value>,
    pub(crate) lines: Vec<Value>,
    pub(crate) bad_lines: usize,
}

/// 진단 zip 바이트를 읽는다. zip이 아니거나 `timeline.jsonl`이 없으면 오류.
pub(crate) fn read_zip_bytes(source: &str, bytes: Vec<u8>) -> Result<Diag, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("{source}: zip 파일이 아닙니다({e})"))?;
    let mut read = |name: &str| -> Result<Option<String>, String> {
        let mut f = match z.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(format!("{source}: {name}을 열 수 없습니다({e})")),
        };
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(|e| format!("{source}: {name}을 읽을 수 없습니다({e})"))?;
        Ok(Some(s))
    };
    let timeline = read("timeline.jsonl")?
        .ok_or_else(|| format!("{source}: 진단 zip이 아닙니다(timeline.jsonl 없음 — 앱의 \"진단 내보내기\" zip을 주세요)"))?;
    let meta = read("meta.json")?.and_then(|t| serde_json::from_str(&t).ok());
    let engine = read("engine.json")?.and_then(|t| serde_json::from_str(&t).ok());
    let mut lines = Vec::new();
    let mut bad_lines = 0;
    for l in timeline.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(l) {
            Ok(v) if v.is_object() => lines.push(v),
            _ => bad_lines += 1,
        }
    }
    Ok(Diag { source: source.into(), meta, engine, lines, bad_lines })
}

pub(crate) fn read_zip(path: &str) -> Result<Diag, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.into());
    read_zip_bytes(&name, bytes)
}

fn f64_at(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}
fn u64_at(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}
fn str_at<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}
fn running(v: &Value) -> bool {
    v.get("running").and_then(Value::as_bool).unwrap_or(false)
}
fn arr3(v: &Value) -> Option<[u64; 3]> {
    let a = v.as_array()?;
    (a.len() == 3).then(|| [a[0].as_u64().unwrap_or(0), a[1].as_u64().unwrap_or(0), a[2].as_u64().unwrap_or(0)])
}

/// 백엔드 문자열(`BackendKind::as_str`, 입출력이 다르면 `a/b`) → GOAL 1-1 행.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendClass {
    Asio,
    Exclusive,
    Shared,
    Other,
}

fn backend_class(b: &str) -> BackendClass {
    match b {
        "cpalAsio" => BackendClass::Asio,
        "wasapiExclusive" => BackendClass::Exclusive,
        "wasapiShared3" | "cpalWasapi" => BackendClass::Shared,
        _ => BackendClass::Other,
    }
}

/// 피어 항목 하나의 첫·마지막 누적(은닉·손실 차분용).
struct Entry {
    first_t: u64,
    last_t: u64,
    first_concealed: u64,
    last_concealed: u64,
    first_shape: Option<LossShapeStats>,
    last_shape: Option<LossShapeStats>,
    frame_ms: Option<f64>,
}

#[derive(Default)]
struct PeerAcc {
    entries: Vec<Entry>,
    /// 직전에 이 피어를 본 실행 샘플 순번
    last_idx: Option<usize>,
    connected: usize,
    paths: BTreeMap<String, usize>,
    rtt: Vec<f64>,
    buffer: Vec<f64>,
    jitter: Vec<f64>,
    target: Vec<f64>,
    loss_engine: Vec<f64>,
    /// (t, 띠 밖?) — targetMs가 있는 샘플만
    band: Vec<(u64, bool)>,
    connect_ms: Vec<f64>,
    first_audio_ms: Vec<f64>,
}

fn push_once(v: &mut Vec<f64>, x: Option<f64>, new_entry: bool) {
    // connectMs·firstAudioMs는 피어 항목 동안 그대로라 항목마다 처음 정해진 값 하나만 모은다.
    if let Some(x) = x {
        if new_entry || v.last() != Some(&x) {
            v.push(x);
        }
    }
}

/// `t`가 늘어나는 (t, 밖?) 표본 → ±3ms 밖 구간(GOAL 1-4). 1초 표본 하나가 1초다.
fn band_of(samples: &[(u64, bool)], interval_ms: u64) -> Option<Band> {
    if samples.is_empty() {
        return None;
    }
    let iv = interval_ms as f64 / 1000.0;
    let mut segs: Vec<f64> = Vec::new();
    let mut cur: Option<(u64, u64)> = None;
    let mut outside = 0usize;
    for &(t, out) in samples {
        if !out {
            continue;
        }
        outside += 1;
        cur = match cur {
            // 띠 안 복귀(사이 시간)가 BAND_GAP_S 이하면 이어 센다
            Some((s, last)) if (t.saturating_sub(last) as f64 / 1000.0 - iv) <= goal::BAND_GAP_S + 1e-9 => Some((s, t)),
            Some((s, last)) => {
                segs.push((last - s) as f64 / 1000.0 + iv);
                Some((t, t))
            }
            None => Some((t, t)),
        };
    }
    if let Some((s, last)) = cur {
        segs.push((last - s) as f64 / 1000.0 + iv);
    }
    Some(Band {
        over_limit: segs.iter().filter(|&&d| d >= goal::BAND_MAX_S).count(),
        longest_s: segs.iter().copied().fold(0.0, f64::max),
        outside_pct: outside as f64 * 100.0 / samples.len() as f64,
    })
}

fn analyze_session(session: u64, lines: &[&Value], cfg: Option<ConfigSummary>, interval_ms: u64) -> SessionReport {
    let run: Vec<&Value> = lines.iter().copied().filter(|v| running(v)).collect();
    let t_of = |v: &Value| u64_at(v, "t").unwrap_or(0);
    let start_t = run.first().map(|v| t_of(v)).or_else(|| lines.first().map(|v| t_of(v)));
    let end_t = run.last().map(|v| t_of(v)).or_else(|| lines.last().map(|v| t_of(v)));
    let duration_s = match (run.first(), run.last()) {
        (Some(a), Some(b)) => (t_of(b).saturating_sub(t_of(a))) as f64 / 1000.0,
        _ => 0.0,
    };
    let last_run = run.last().copied();
    let backend = run.iter().rev().filter_map(|v| str_at(v, "backend")).find(|b| !b.is_empty()).map(str::to_string);

    // xrun: 세션 누적. 첫 실행 샘플 값은 시작부터(기록 전 몫 포함)이므로 0초 증가로 센다.
    let mut xrun_steps = Vec::new();
    let mut prev = 0u64;
    for v in &run {
        if let Some(x) = u64_at(v, "xruns") {
            if x > prev {
                xrun_steps.push(XrunStep { at_s: (t_of(v) - start_t.unwrap_or(0)) as f64 / 1000.0, add: x - prev });
            }
            prev = x;
        }
    }

    // 코어 배치: 줄마다 구간 값 [P, E, LP-E](format 2) — 합 = 세션 누적. 비율 객체(format 1)는 더할 수 없어 센다.
    let mut placement: Option<Placement> = None;
    for v in &run {
        let Some(cp) = v.get("corePlacement") else { continue };
        let p = placement.get_or_insert(Placement { worker: [0; 3], net_recv: None, legacy_lines: 0 });
        match cp.get("worker").and_then(arr3) {
            Some(w) => {
                p.worker.iter_mut().zip(w).for_each(|(a, b)| *a += b);
                if let Some(r) = cp.get("netRecv").and_then(arr3) {
                    p.net_recv.get_or_insert([0; 3]).iter_mut().zip(r).for_each(|(a, b)| *a += b);
                }
            }
            None => p.legacy_lines += 1,
        }
    }

    // 피어
    let cfg_frame = cfg.as_ref().map(|c| c.frame_ms);
    let mut peers: BTreeMap<String, PeerAcc> = BTreeMap::new();
    for (idx, v) in run.iter().enumerate() {
        let t = t_of(v);
        for p in v.get("peers").and_then(Value::as_array).into_iter().flatten() {
            let name = str_at(p, "peer").or_else(|| str_at(p, "peerId")).unwrap_or("?").to_string();
            let acc = peers.entry(name).or_default();
            let concealed = u64_at(p, "concealed").unwrap_or(0);
            let shape = p.get("lossShape").and_then(|s| serde_json::from_value::<LossShapeStats>(s.clone()).ok());
            let frame = shape.map(|s| s.frame_ms as f64).filter(|f| *f > 0.0).or_else(|| f64_at(p, "remoteFrameMs")).or(cfg_frame);
            // 새 항목: 직전 실행 샘플에 없었거나 누적이 줄었다(재입장).
            let new_entry = match (acc.last_idx, acc.entries.last()) {
                (Some(li), Some(e)) => {
                    li + 1 != idx
                        || concealed < e.last_concealed
                        || matches!((shape, e.last_shape), (Some(a), Some(b)) if shape::weight(&a) < shape::weight(&b))
                }
                _ => true,
            };
            if new_entry {
                acc.entries.push(Entry {
                    first_t: t,
                    last_t: t,
                    first_concealed: concealed,
                    last_concealed: concealed,
                    first_shape: shape,
                    last_shape: shape,
                    frame_ms: frame,
                });
            } else if let Some(e) = acc.entries.last_mut() {
                e.last_t = t;
                e.last_concealed = concealed;
                e.last_shape = shape;
                if frame.is_some() {
                    e.frame_ms = frame;
                }
            }
            acc.last_idx = Some(idx);
            push_once(&mut acc.connect_ms, f64_at(p, "connectMs"), new_entry);
            push_once(&mut acc.first_audio_ms, f64_at(p, "firstAudioMs"), new_entry);

            let connected = str_at(p, "state") == Some("connected");
            let buffer = f64_at(p, "bufferMs");
            if let Some(target) = f64_at(p, "targetMs") {
                // 재생이 멈춘(연결 아님) 표본은 띠 밖으로 센다(`tests/sim/long.rs`와 같다).
                let inside = connected && buffer.is_some_and(|b| (b - target).abs() <= goal::BAND_MS);
                acc.band.push((t, !inside));
                if connected {
                    acc.target.push(target);
                }
            }
            if !connected {
                continue;
            }
            acc.connected += 1;
            if let Some(path) = str_at(p, "path") {
                *acc.paths.entry(path.to_string()).or_default() += 1;
            }
            acc.rtt.extend(f64_at(p, "rttMs"));
            acc.buffer.extend(buffer);
            acc.jitter.extend(f64_at(p, "jitterMs"));
            acc.loss_engine.extend(f64_at(p, "lossPct"));
        }
    }
    let line_vals: Vec<Value> = lines.iter().map(|v| (*v).clone()).collect();
    let shapes = shape::from_lines(&line_vals);
    let peers: Vec<PeerReport> = peers
        .into_iter()
        .map(|(name, a)| {
            let mut frames = 0.0;
            let mut seconds = 0.0;
            let mut concealed = 0u64;
            let mut delta: Option<LossShapeStats> = None;
            let mut shape_all = true;
            for e in &a.entries {
                let Some(f) = e.frame_ms.filter(|f| *f > 0.0) else { continue };
                let secs = (e.last_t - e.first_t) as f64 / 1000.0;
                seconds += secs;
                frames += secs * 1000.0 / f;
                concealed += e.last_concealed.saturating_sub(e.first_concealed);
                match (e.first_shape, e.last_shape) {
                    (Some(a0), Some(a1)) => {
                        let d = shape::sub(&a1, &a0);
                        delta = Some(delta.map_or(d, |x| shape::add(&x, &d)));
                    }
                    _ => shape_all = false,
                }
            }
            let delta = delta.filter(|_| shape_all);
            let pct = |n: f64| n * 100.0 / frames;
            let conceal = (frames > 0.0).then(|| Conceal {
                frames,
                seconds,
                pct: pct(concealed as f64),
                fec_pct: delta.map(|d| pct(d.conceal_fec as f64)),
                plc_pct: delta.map(|d| pct(d.conceal_plc as f64)),
                pcm_pct: delta.map(|d| pct(d.conceal_pcm as f64)),
                underrun_pct: delta.map(|d| pct(d.conceal_underrun as f64)),
                loss_pct: delta.map(|d| pct(d.estimate().lost)),
            });
            let loss_shape =
                shapes.iter().filter(|((s, p, _), _)| *s == session && *p == name).map(|(_, sh)| ShapeSummary::of(sh)).collect();
            let frame_ms = a.entries.iter().rev().find_map(|e| e.frame_ms);
            PeerReport {
                peer: name,
                entries: a.entries.len(),
                connected_samples: a.connected,
                paths: a.paths,
                rtt_ms: Dist::of(&a.rtt),
                buffer_ms: Dist::of(&a.buffer),
                jitter_ms: Dist::of(&a.jitter),
                target_ms: Dist::of(&a.target),
                frame_ms,
                band: band_of(&a.band, interval_ms),
                conceal,
                loss_pct_engine_mean: (!a.loss_engine.is_empty()).then(|| a.loss_engine.iter().sum::<f64>() / a.loss_engine.len() as f64),
                connect_ms: a.connect_ms,
                first_audio_ms: a.first_audio_ms,
                loss_shape,
            }
        })
        .collect();

    let mut conn = Vec::new();
    for v in lines {
        for c in v.get("conn").and_then(Value::as_array).into_iter().flatten() {
            let t = u64_at(c, "t").unwrap_or_else(|| t_of(v));
            conn.push(ConnRow {
                at_s: t.saturating_sub(start_t.unwrap_or(t)) as f64 / 1000.0,
                peer: str_at(c, "peer").unwrap_or("?").into(),
                phase: str_at(c, "phase").unwrap_or("?").into(),
                result: str_at(c, "result").unwrap_or("?").into(),
                ms: f64_at(c, "ms"),
            });
        }
    }

    let est: Vec<f64> = run.iter().filter_map(|v| f64_at(v, "estLatencyMs")).filter(|x| *x > 0.0).collect();
    let mut s = SessionReport {
        session,
        network: lines.last().and_then(|v| v.get("network")).and_then(Value::as_bool),
        start: start_t.map(super::export::iso_utc),
        end: end_t.map(super::export::iso_utc),
        duration_s,
        running_samples: run.len(),
        backend,
        sample_rate: last_run.and_then(|v| u64_at(v, "sampleRate")),
        device_buffer: last_run.and_then(|v| u64_at(v, "deviceBuffer")),
        device_period_ms: last_run.and_then(|v| f64_at(v, "devicePeriodMs")),
        config: cfg,
        est_latency_ms: Dist::of(&est),
        core_placement: placement,
        thread_policy: last_run.and_then(|v| v.get("threadPolicy")).cloned(),
        xruns: last_run.and_then(|v| u64_at(v, "xruns")),
        xrun_steps,
        stream_errors: last_run.and_then(|v| u64_at(v, "streamErrors")),
        peers,
        conn,
        goal: Vec::new(),
    };
    s.goal = session_goal(&s);
    s
}

fn fmt_s(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// GOAL 2-1(손실 1% 회선에서 은닉 ≤ 1.5%) 판정. 손실이 조건 이상인 회선에서 은닉이 한도 안이면 충족, 손실이 조건 이하인데
/// 은닉이 한도를 넘으면 미달이다. 그 밖(손실이 적은 회선에서 한도 안, 손실이 큰 회선에서 한도 밖)은 조건이 달라 가를 수 없다.
fn conceal_verdict(conceal_pct: f64, loss_pct: f64) -> (Verdict, Option<String>) {
    let cond = goal::CONCEAL_LOSS_COND_PCT;
    let ok = conceal_pct <= goal::CONCEAL_MAX_PCT;
    match (loss_pct >= cond, loss_pct <= cond, ok) {
        (true, _, true) => (Verdict::Met, Some("수치만 — 청감은 [사람]".into())),
        (_, true, false) => (Verdict::NotMet, None),
        (_, _, true) => (Verdict::Unknown, Some(format!("회선 손실 {loss_pct:.2}% < 조건 {cond}% — 손실 1% 회선이 아니라 가를 수 없음(참고값)"))),
        _ => (Verdict::Unknown, Some(format!("회선 손실 {loss_pct:.2}% > 조건 {cond}% — 조건보다 나쁜 회선이라 가를 수 없음"))),
    }
}

/// 세션 GOAL 판정(1·2절).
fn session_goal(s: &SessionReport) -> Vec<GoalRow> {
    use Verdict::*;
    let mut out = Vec::new();
    let dur = fmt_s(s.duration_s);
    let class = s.backend.as_deref().map(backend_class);

    // 1-1 로컬 처리 지연
    let target_11 = format!("ASIO 128/2.5ms ≤ {}ms · WASAPI 독점 ≤ {}ms · 공유 ≤ {}ms", goal::EST_ASIO_MS, goal::EST_EXCLUSIVE_MS, goal::EST_SHARED_MS);
    match (&s.est_latency_ms, class) {
        (None, _) | (_, None) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, "없음".into(), Unknown, Some("실행 샘플·백엔드 기록 없음".into()))),
        (Some(d), Some(c)) => {
            let measured = format!("중앙 {:.2}ms · 최대 {:.2}ms ({})", d.median, d.max, s.backend.as_deref().unwrap_or("?"));
            let frame = s.config.as_ref().map(|c| c.frame_ms);
            let limit = match c {
                BackendClass::Exclusive => Ok(goal::EST_EXCLUSIVE_MS),
                BackendClass::Shared => Ok(goal::EST_SHARED_MS),
                BackendClass::Asio => {
                    if s.device_buffer.is_some_and(|b| b as u32 <= goal::EST_ASIO_BUFFER) && frame == Some(goal::EST_ASIO_FRAME_MS) {
                        Ok(goal::EST_ASIO_MS)
                    } else {
                        Err("기준은 ASIO 버퍼 ≤128·프레임 2.5ms(이 세션 설정이 다르거나 engine.json에 설정 없음)".to_string())
                    }
                }
                BackendClass::Other => Err(format!("백엔드 {}는 GOAL 1-1 행이 없음", s.backend.as_deref().unwrap_or("?"))),
            };
            match limit {
                Ok(l) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, if d.median <= l { Met } else { NotMet }, Some(format!("이 백엔드 기준 ≤ {l}ms, 중앙값으로 판정")))),
                Err(why) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, Unknown, Some(why))),
            }
        }
    }

    for p in &s.peers {
        // 1-3 지터 버퍼 추가 지연(안정 회선 = 유선 LAN)
        let t13 = "안정 회선(유선 LAN) bufferMs 중앙값 ≤ 1프레임 + p95 지터".to_string();
        let name13 = format!("지터 버퍼 추가 지연 — {}", p.peer);
        let secs = p.connected_samples as f64 * 1.0;
        match (&p.buffer_ms, &p.jitter_ms, p.frame_ms) {
            (Some(b), Some(j), Some(f)) => {
                let limit = f + j.p95;
                let measured = format!("중앙 {:.2}ms vs 한도 {:.2}ms(F {f} + p95 지터 {:.2})", b.median, limit, j.p95);
                if p.main_path() != Some("lan") {
                    out.push(row("1-3", &name13, t13, measured, Unknown, Some(format!("경로 {} — LAN이 아니라 안정 회선 기준 대상 아님", p.main_path().unwrap_or("없음")))));
                } else if secs < goal::MIN_JUDGE_S {
                    out.push(row("1-3", &name13, t13, measured, Unknown, Some(format!("연결 {secs:.0}s < {}s", goal::MIN_JUDGE_S))));
                } else if b.median <= limit {
                    out.push(row("1-3", &name13, t13, measured, Met, Some("LAN 경로 — 유선인지는 기록표로 확인".into())));
                } else {
                    out.push(row("1-3", &name13, t13, measured, NotMet, Some("LAN 경로 — Wi-Fi였다면 안정 회선 대상 아님(기록표 확인)".into())));
                }
            }
            _ => out.push(row("1-3", &name13, t13, "없음".into(), Unknown, Some("연결 샘플의 bufferMs·jitterMs 또는 프레임 길이 없음".into()))),
        }

        // 1-4 지연 누적(±3ms 1분)
        let t14 = format!("{}분 동안 bufferMs가 목표 ±{}ms 밖에 {}s 이상 머무르지 않음", goal::BAND_SESSION_S / 60.0, goal::BAND_MS, goal::BAND_MAX_S);
        let name14 = format!("지연 누적 — {}", p.peer);
        match &p.band {
            None => out.push(row("1-4", &name14, t14, "없음".into(), Unknown, Some("타임라인에 targetMs(목표 버퍼)가 기록되지 않음 — 진단 형식에 추가 필요".into()))),
            Some(b) => {
                let measured = format!("1분+ 구간 {} · 최장 {:.0}s · 밖 {:.1}% · 세션 {dur}", b.over_limit, b.longest_s, b.outside_pct);
                if b.over_limit > 0 {
                    out.push(row("1-4", &name14, t14, measured, NotMet, None));
                } else if s.duration_s < goal::BAND_SESSION_S {
                    out.push(row("1-4", &name14, t14, measured, Unknown, Some(format!("세션 {dur} < 60분", ))));
                } else {
                    out.push(row("1-4", &name14, t14, measured, Met, None));
                }
            }
        }

        // 2-1 손실 1% 은닉 ≤ 1.5%
        let t21 = format!("손실 {}% 회선에서 은닉 ≤ {}%", goal::CONCEAL_LOSS_COND_PCT, goal::CONCEAL_MAX_PCT);
        let name21 = format!("은닉 — {}", p.peer);
        match &p.conceal {
            None => out.push(row("2-1", &name21, t21, "없음".into(), Unknown, Some("프레임 길이를 알 수 없거나 연결 구간 없음".into()))),
            Some(c) => {
                let loss = c.loss_pct.map(|l| format!("{l:.2}%")).unwrap_or_else(|| "없음".into());
                let measured = format!("은닉 {:.2}% · 손실 {loss} · {}", c.pct, fmt_s(c.seconds));
                let v = if c.seconds < goal::MIN_JUDGE_S {
                    (Unknown, Some(format!("연결 {} < {}s", fmt_s(c.seconds), goal::MIN_JUDGE_S)))
                } else {
                    match c.loss_pct {
                        None => (Unknown, Some("lossShape 없음(P2-17h 이전) — 회선 손실을 몰라 조건 확인 불가".into())),
                        Some(l) => conceal_verdict(c.pct, l),
                    }
                };
                out.push(row("2-1", &name21, t21, measured, v.0, v.1));
            }
        }
    }

    // 2-2 권장 설정 xrun
    let t22 = format!("권장 설정(WASAPI 독점) {}분 xrun 0", goal::XRUN_WINDOW_S / 60.0);
    match (s.xruns, class) {
        (None, _) | (_, None) => out.push(row("2-2", "xrun", t22, "없음".into(), Unknown, Some("실행 샘플 없음".into()))),
        (Some(x), Some(c)) => {
            let measured = format!("{x}회 · 세션 {dur}");
            if c != BackendClass::Exclusive {
                out.push(row("2-2", "xrun", t22, measured, Unknown, Some(format!("백엔드 {} — 권장 설정(wasapiExclusive) 아님", s.backend.as_deref().unwrap_or("?")))));
            } else if x > 0 {
                out.push(row("2-2", "xrun", t22, measured, NotMet, None));
            } else if s.duration_s < goal::XRUN_WINDOW_S {
                out.push(row("2-2", "xrun", t22, measured, Unknown, Some(format!("세션 {dur} < 30분"))));
            } else {
                out.push(row("2-2", "xrun", t22, measured, Met, None));
            }
        }
    }
    out
}

/// 여러 세션의 `conn`과 첫 소리 → 연결 성공률 집계(후보 ③).
pub(crate) fn aggregate_conn<'a>(sessions: impl IntoIterator<Item = &'a SessionReport>) -> ConnAgg {
    const RESULTS: [&str; 5] = ["lan", "public", "relay", "failed", "aborted"];
    // (결과, 단계) → (시도 수, 소요 ms들)
    let mut by: BTreeMap<(usize, bool), (usize, Vec<f64>)> = BTreeMap::new();
    let (mut n_sessions, mut with_relay, mut recover, mut switch) = (0, 0, 0, 0);
    let mut first_audio_ms = Vec::new();
    for s in sessions {
        n_sessions += 1;
        let mut relay_here = false;
        for c in &s.conn {
            relay_here |= c.result == "relay";
            match c.phase.as_str() {
                "connect" | "reconnect" => {
                    // 알 수 없는 결과(새 형식)는 시도로 세지 않는다 — 표에 없는 값을 성공·실패로 추측하지 않는다.
                    let Some(r) = RESULTS.iter().position(|r| *r == c.result) else { continue };
                    let e = by.entry((r, c.phase == "reconnect")).or_default();
                    e.0 += 1;
                    e.1.extend(c.ms);
                }
                "recover" => recover += 1,
                "switch" => switch += 1,
                _ => {}
            }
        }
        with_relay += usize::from(relay_here);
        for p in &s.peers {
            first_audio_ms.extend(&p.first_audio_ms);
        }
    }
    let rows: Vec<ConnPathRow> = RESULTS
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let get = |re: bool| by.get(&(i, re)).map(|(n, ms)| (*n, median(ms))).unwrap_or((0, None));
            let (c, cm) = get(false);
            let (rc, rm) = get(true);
            ConnPathRow { result: r.to_string(), connect: c, connect_median_ms: cm, reconnect: rc, reconnect_median_ms: rm }
        })
        .collect();
    let n = |r: &str| rows.iter().find(|x| x.result == r).map(|x| x.connect + x.reconnect).unwrap_or(0);
    let direct = n("lan") + n("public");
    let relay = n("relay");
    let (failed, aborted) = (n("failed"), n("aborted"));
    let attempts = direct + relay + failed + aborted;
    let pct = |k: usize| (attempts > 0).then(|| k as f64 * 100.0 / attempts as f64);
    ConnAgg {
        sessions: n_sessions,
        attempts,
        success: direct + relay,
        direct,
        relay,
        failed,
        aborted,
        success_pct: pct(direct + relay),
        direct_pct: pct(direct),
        recover,
        switch,
        sessions_with_relay: with_relay,
        first_audio_ms,
        rows,
    }
}

/// 3절 판정(모든 zip 합).
fn conn_goal(a: &ConnAgg) -> Vec<GoalRow> {
    use Verdict::*;
    let mut out = Vec::new();
    let pct = |v: Option<f64>| v.map(|v| format!("{v:.1}%")).unwrap_or_else(|| "없음".into());
    let t31 = format!("콘형 NAT 쌍 직접 {}%, 같은 공유기 LAN 우선", goal::DIRECT_PCT);
    if a.attempts == 0 {
        out.push(row("3-1", "직접 연결 성공률", t31, "없음".into(), Unknown, Some("conn 기록 없음(P1-18 이전이거나 연결 시도 없음)".into())));
    } else {
        out.push(row(
            "3-1",
            "직접 연결 성공률",
            t31,
            format!("직접 {}/{} = {}", a.direct, a.attempts, pct(a.direct_pct)),
            Unknown,
            Some("NAT 종류가 진단에 없음 — 기록표의 회선 정보로 판단".into()),
        ));
    }
    let t32 = format!("릴레이 폴백 포함 연결 성공률 ≥ {}%", goal::CONNECT_PCT);
    let measured = format!("성공 {}/{} = {} (실패 {} · 중단 {})", a.success, a.attempts, pct(a.success_pct), a.failed, a.aborted);
    let v32 = match a.success_pct {
        None => (Unknown, Some("conn 기록 없음".into())),
        Some(p) if p < goal::CONNECT_PCT => (NotMet, Some("중단(연결 중 퇴장·정지)도 분모에 든다 — 원인은 연결 기록 표에서".into())),
        Some(_) if a.attempts < goal::MIN_ATTEMPTS_98 => (Unknown, Some(format!("시도 {}회 < {}회 — 98%를 가르기에 적음", a.attempts, goal::MIN_ATTEMPTS_98))),
        Some(_) => (Met, None),
    };
    out.push(row("3-2", "연결 성공률(폴백 포함)", t32, measured, v32.0, v32.1));
    let t33 = format!("입장 후 {}초 이내 소리", goal::FIRST_AUDIO_MS / 1000.0);
    match Dist::of(&a.first_audio_ms) {
        None => out.push(row("3-3", "첫 소리", t33, "없음".into(), Unknown, Some("firstAudioMs 기록 없음".into()))),
        Some(d) => {
            let over = a.first_audio_ms.iter().filter(|&&x| x > goal::FIRST_AUDIO_MS).count();
            let measured = format!("{}개 · 중앙 {:.0}ms · 최대 {:.0}ms · 3초 초과 {over}", a.first_audio_ms.len(), d.median, d.max);
            let v = if over > 0 { NotMet } else { Met };
            out.push(row("3-3", "첫 소리", t33, measured, v, Some("firstAudioMs = 엔진에 피어가 추가된 때부터(입장 버튼부터가 아님)".into())));
        }
    }
    out
}

/// zip 하나 → 세션별 보고서.
pub(crate) fn analyze(d: &Diag) -> FileReport {
    let interval_ms = d.meta.as_ref().and_then(|m| m.get("timeline")).and_then(|t| u64_at(t, "intervalMs")).unwrap_or(1000);
    // engine.json 설정은 마지막 세션 것(`lastStats.session`)이다.
    let cfg = d.engine.as_ref().and_then(|e| e.get("config")).and_then(|c| serde_json::from_value::<EngineConfig>(c.clone()).ok()).map(|c| ConfigSummary::of(&c));
    let cfg_session = d.engine.as_ref().and_then(|e| e.get("lastStats")).and_then(|s| u64_at(s, "session"));
    let mut order: Vec<u64> = Vec::new();
    let mut groups: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for v in &d.lines {
        let s = u64_at(v, "session").unwrap_or(0);
        if !groups.contains_key(&s) {
            order.push(s);
        }
        groups.entry(s).or_default().push(v);
    }
    let sessions = order
        .into_iter()
        .map(|s| {
            let c = if cfg_session == Some(s) { cfg.clone() } else { None };
            analyze_session(s, &groups[&s], c, interval_ms)
        })
        .collect();
    let meta = d.meta.as_ref();
    FileReport {
        source: d.source.clone(),
        format: meta.and_then(|m| u64_at(m, "format")),
        app_version: meta.and_then(|m| m.get("app")).and_then(|a| str_at(a, "version")).map(str::to_string),
        os: meta.and_then(|m| m.get("os")).map(|o| {
            [str_at(o, "family"), str_at(o, "arch"), str_at(o, "version")].into_iter().flatten().collect::<Vec<_>>().join(" ")
        }),
        exported_at: meta.and_then(|m| str_at(m, "exportedAtIso")).map(str::to_string),
        timeline_dropped: meta.and_then(|m| m.get("timeline")).and_then(|t| u64_at(t, "dropped")),
        bad_lines: d.bad_lines,
        sessions,
    }
}

/// 예제 `diag_report`의 본체: zip 경로들 → Markdown(또는 `json`이면 같은 값의 JSON). 잘못된 zip은 오류 문장.
pub fn run(paths: &[String], json: bool) -> Result<String, String> {
    let diags = paths.iter().map(|p| read_zip(p)).collect::<Result<Vec<_>, _>>()?;
    let r = report(&diags);
    if json {
        serde_json::to_string_pretty(&r).map(|s| s + "
").map_err(|e| format!("JSON 출력 실패: {e}"))
    } else {
        Ok(markdown(&r))
    }
}

/// zip 여러 개 → 보고서(세션별 + 연결 집계 + 3절 판정).
pub(crate) fn report(diags: &[Diag]) -> Report {
    let files: Vec<FileReport> = diags.iter().map(analyze).collect();
    let conn = aggregate_conn(files.iter().flat_map(|f| f.sessions.iter()));
    let goal = conn_goal(&conn);
    Report { files, conn, goal }
}

#[cfg(test)]
mod tests;
