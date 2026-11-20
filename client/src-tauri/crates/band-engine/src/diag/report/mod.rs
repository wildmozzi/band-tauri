//! 진단 zip → 결과표(GOAL-COVERAGE 후보 ②·③). 사람이 실장비·실회선 세션 뒤 "진단 내보내기" zip만 보내면 코디네이터가
//! `cargo run -p band-engine --example diag_report -- <zip>...`으로 GOAL 판정에 필요한 숫자를 얻는다.
//!
//! 읽는 것(`export.rs` 머리말의 zip 구성): `meta.json`(앱 버전·형식·시각), `engine.json`(마지막 세션 설정 — 드라이버·프리셋·
//! 프레임), `timeline.jsonl`(1초 샘플 — 세션·백엔드·xrun·코어 배치·피어·`conn`). 줄은 `serde_json::Value`로 읽고 필드가 없으면
//! "없음"으로 둔다 — 옛 형식(P1-18 `conn`, P2-17h `lossShape`, P1-21 `corePlacement` 이전)도 죽지 않는다. 형식이 바뀌어 필드
//! 이름이 달라지면 시험 `report_reads_real_export`(실제 `diag::export::build`로 만든 zip을 읽는다)가 깨진다. 설정은 `EngineConfig`,
//! 손실 모양은 `LossShapeStats` 타입으로 그대로 역직렬화한다.
//!
//! 판정은 [`goal`]의 기준값(GOAL.md 절 번호 주석)과 나란히 놓고 충족/미달/판정 불가로 적는다. 데이터가 없거나, 세션이 기준보다
//! 짧거나, 측정 구간·분모가 GOAL 정의와 다르면 "판정 불가"와 이유를 적는다 — 충족은 근거가 완전할 때만 쓴다(#246 리뷰: 은닉 %의
//! 분모는 재생 프레임 `produced`, 1-4는 피어가 기록된 시간, 3-3은 충족을 내지 않음, 3-2 문턱은 양쪽 zip 중복을 뺀 하한).
//! 판정 함수마다 "충족으로 잘못 떨어지는 입력"을 시험(`tests.rs` 아래쪽)으로 막아 둔다.
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
    /// targetMs가 기록된 이 피어의 표본 시간(s, 표본 수 × 간격) — 세션 길이가 아니라 피어가 실제로 있던 시간
    pub(crate) covered_s: f64,
    /// 표본 사이가 간격 + `BAND_GAP_S`보다 벌어진 곳(기록 누락·재입장) 수. 틈 속 상태를 몰라 1분 판정을 가를 수 없다
    pub(crate) gaps: usize,
}

/// 은닉·손실(피어 항목마다 첫·마지막 샘플 사이 누적 차분). 분모는 그 피어에게서 실제로 재생한 프레임(`produced`)이다 —
/// 벽시계 시간이 아니다(연결 중·재연결·무수신 리셋 시간은 은닉도 재생도 세지 않으므로, 시간으로 나누면 은닉을 낮게 잡는다, #246 H1).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Conceal {
    /// 재생 프레임 차분(정상 + 은닉). 항목 하나라도 `produced`가 없으면(#246 이전 형식) None
    pub(crate) produced: Option<u64>,
    pub(crate) concealed: u64,
    /// 연결(`connected`) 샘플 시간(s, 표본 수 × 간격)
    pub(crate) connected_s: f64,
    /// 은닉 ÷ 재생 프레임(%). `produced`가 없거나 0이면 None
    pub(crate) pct: Option<f64>,
    /// 방식별(lossShape·produced 없으면 None)
    pub(crate) fec_pct: Option<f64>,
    pub(crate) plc_pct: Option<f64>,
    pub(crate) pcm_pct: Option<f64>,
    pub(crate) underrun_pct: Option<f64>,
    /// 복구 후 손실(lossShape 추정 손실 ÷ 지나간 seq 수 = 재생 − 언더런 은닉). 사본·재전송으로 메운 손실은 빠지므로 실제 회선
    /// 손실 이하다. lossShape·produced 없으면 None
    pub(crate) loss_pct: Option<f64>,
    /// 재전송 창에서 버린 사본·재전송(`rxDupDropped` 차분). 0이 아니면 사본이 손실을 메웠을 수 있다 — 회선 손실 > 복구 후 손실
    pub(crate) dup_dropped: Option<u64>,
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
    /// 첫 소리 없이 `FIRST_AUDIO_MS` 이상 머문 피어 항목 수(소리를 끝내 못 받음 — 3-3 미달, #246 H2)
    pub(crate) no_audio_entries: usize,
    /// 첫 소리 없이 `FIRST_AUDIO_MS`보다 짧게 머문 항목 수(나갔거나 기록이 끝남 — 가를 수 없음)
    pub(crate) no_audio_short: usize,
    pub(crate) loss_shape: Vec<ShapeSummary>,
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
    /// 첫·마지막 샘플 시각(유닉스 ms) — 여러 zip의 세션이 시간상 겹치는지(같은 합주의 양쪽 기록) 볼 때 쓴다
    #[serde(skip)]
    pub(crate) span_ms: Option<(u64, u64)>,
    /// 마지막 실행 샘플의 백엔드
    pub(crate) backend: Option<String>,
    /// 세션 동안 기록된 백엔드 종류(둘 이상이면 중간에 바뀜 — 예: 독점 실패 뒤 공유 폴백)
    pub(crate) backends: Vec<String>,
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
    /// 앞의 zip과 타임라인이 똑같다(같은 zip을 두 번 줌) — 그 zip 이름. 연결 집계에서 뺀다
    pub(crate) duplicate_of: Option<String>,
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
/// 시도 수는 **기록 기준**이다: 한 합주의 두 사람 zip을 함께 주면 같은 연결이 양쪽에서 한 번씩 세어진다(가명이 서로 달라 짝을 맞출
/// 수 없다). 그래서 98% 문턱은 `attempts_lower`(시간이 겹치는 다른 zip 세션들은 그중 시도가 가장 많은 zip 것만 센 하한)로 가른다.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnAgg {
    pub(crate) sessions: usize,
    pub(crate) rows: Vec<ConnPathRow>,
    /// 기록된 시도 수(양쪽 zip이면 같은 연결이 두 번)
    pub(crate) attempts: usize,
    /// 실제 시도 수의 하한(겹치는 다른 zip 세션은 큰 쪽만) — 98% 문턱(`MIN_ATTEMPTS_98`)에 쓴다
    pub(crate) attempts_lower: usize,
    /// 결과 문자열을 모르는(표에 없는 새 형식) 시도 — 성공·실패를 추측하지 않고, 있으면 3-2를 가르지 않는다
    pub(crate) unknown_results: usize,
    /// 같은 zip을 두 번 줘서 뺀 zip 이름
    pub(crate) duplicate_files: Vec<String>,
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
    /// 첫 소리 없이 3초 이상 머문 피어 항목 수(`PeerReport.no_audio_entries` 합)
    pub(crate) no_audio_entries: usize,
    pub(crate) no_audio_short: usize,
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

/// zip 안 파일 하나를 풀 때의 상한(바이트).
const MAX_ENTRY_BYTES: u64 = 64 << 20;

/// 진단 zip 바이트를 읽는다. zip이 아니거나 `timeline.jsonl`이 없으면 오류.
pub(crate) fn read_zip_bytes(source: &str, bytes: Vec<u8>) -> Result<Diag, String> {
    read_zip_bytes_max(source, bytes, MAX_ENTRY_BYTES)
}

fn read_zip_bytes_max(source: &str, bytes: Vec<u8>, max: u64) -> Result<Diag, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("{source}: zip 파일이 아닙니다({e})"))?;
    let mut read = |name: &str| -> Result<Option<String>, String> {
        let mut f = match z.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(format!("{source}: {name}을 열 수 없습니다({e})")),
        };
        // 압축 폭탄 방지: 60분 타임라인은 수 MB다. 상한을 넘으면 잘린 JSON 줄로 끝나 거르기보다 오류로 알린다.
        let mut s = String::new();
        (&mut f).take(max + 1).read_to_string(&mut s).map_err(|e| format!("{source}: {name}을 읽을 수 없습니다({e})"))?;
        if s.len() as u64 > max {
            return Err(format!("{source}: {name}이 {max}바이트를 넘습니다 — 진단 zip이 아닌 것 같습니다"));
        }
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

/// 피어 항목 하나의 첫·마지막 누적(은닉·손실 차분용)과 항목 동안 그대로인 값.
struct Entry {
    first_t: u64,
    last_t: u64,
    first_concealed: u64,
    last_concealed: u64,
    /// 재생 프레임 누적(`produced`, #246 이전 형식이면 None)
    first_produced: Option<u64>,
    last_produced: Option<u64>,
    first_dup: Option<u64>,
    last_dup: Option<u64>,
    first_shape: Option<LossShapeStats>,
    last_shape: Option<LossShapeStats>,
    /// 받는 프레임(ms) — 상대 것만(lossShape `frameMs` → `remoteFrameMs`). 내 설정 프레임으로 대신하지 않는다(상대 프리셋이 다를 수 있다)
    frame_ms: Option<f64>,
    connect_ms: Option<f64>,
    first_audio_ms: Option<f64>,
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
}

/// `t`가 늘어나는 (t, 밖?) 표본 → ±3ms 밖 구간(GOAL 1-4). 1초 표본 하나가 1초다.
fn band_of(samples: &[(u64, bool)], interval_ms: u64) -> Option<Band> {
    if samples.is_empty() {
        return None;
    }
    let iv = interval_ms as f64 / 1000.0;
    let gaps = samples.windows(2).filter(|w| (w[1].0.saturating_sub(w[0].0) as f64 / 1000.0 - iv) > goal::BAND_GAP_S + 1e-9).count();
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
        covered_s: samples.len() as f64 * iv,
        gaps,
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
    let mut backends: Vec<String> = Vec::new();
    for b in run.iter().filter_map(|v| str_at(v, "backend")).filter(|b| !b.is_empty()) {
        if !backends.iter().any(|x| x == b) {
            backends.push(b.to_string());
        }
    }

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
    let mut peers: BTreeMap<String, PeerAcc> = BTreeMap::new();
    for (idx, v) in run.iter().enumerate() {
        let t = t_of(v);
        for p in v.get("peers").and_then(Value::as_array).into_iter().flatten() {
            let name = str_at(p, "peer").or_else(|| str_at(p, "peerId")).unwrap_or("?").to_string();
            let acc = peers.entry(name).or_default();
            let concealed = u64_at(p, "concealed").unwrap_or(0);
            let produced = u64_at(p, "produced");
            let dup = u64_at(p, "rxDupDropped");
            let shape = p.get("lossShape").and_then(|s| serde_json::from_value::<LossShapeStats>(s.clone()).ok());
            let frame = shape.map(|s| s.frame_ms as f64).filter(|f| *f > 0.0).or_else(|| f64_at(p, "remoteFrameMs"));
            let (connect_ms, first_audio_ms) = (f64_at(p, "connectMs"), f64_at(p, "firstAudioMs"));
            // 새 항목: 직전 실행 샘플에 없었거나 누적이 줄었다(재입장).
            let new_entry = match (acc.last_idx, acc.entries.last()) {
                (Some(li), Some(e)) => {
                    li + 1 != idx
                        || concealed < e.last_concealed
                        || matches!((produced, e.last_produced), (Some(a), Some(b)) if a < b)
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
                    first_produced: produced,
                    last_produced: produced,
                    first_dup: dup,
                    last_dup: dup,
                    first_shape: shape,
                    last_shape: shape,
                    frame_ms: frame,
                    connect_ms,
                    first_audio_ms,
                });
            } else if let Some(e) = acc.entries.last_mut() {
                e.last_t = t;
                e.last_concealed = concealed;
                e.last_produced = produced;
                e.last_dup = dup;
                e.last_shape = shape;
                e.frame_ms = frame.or(e.frame_ms);
                e.connect_ms = e.connect_ms.or(connect_ms);
                e.first_audio_ms = e.first_audio_ms.or(first_audio_ms);
            }
            acc.last_idx = Some(idx);

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
            // 은닉·손실: 항목마다 누적 차분을 더한다. 분모는 재생 프레임(produced) — 하나라도 없으면 비율을 내지 않는다.
            let mut concealed = 0u64;
            let mut produced: Option<u64> = Some(0);
            let mut dup: Option<u64> = Some(0);
            let mut delta: Option<LossShapeStats> = None;
            let mut shape_all = true;
            for e in &a.entries {
                concealed += e.last_concealed.saturating_sub(e.first_concealed);
                produced = match (produced, e.first_produced, e.last_produced) {
                    (Some(n), Some(p0), Some(p1)) => Some(n + p1.saturating_sub(p0)),
                    _ => None,
                };
                dup = match (dup, e.first_dup, e.last_dup) {
                    (Some(n), Some(d0), Some(d1)) => Some(n + d1.saturating_sub(d0)),
                    _ => None,
                };
                match (e.first_shape, e.last_shape) {
                    (Some(a0), Some(a1)) => {
                        let d = shape::sub(&a1, &a0);
                        delta = Some(delta.map_or(d, |x| shape::add(&x, &d)));
                    }
                    _ => shape_all = false,
                }
            }
            let delta = delta.filter(|_| shape_all);
            let conceal = (!a.entries.is_empty()).then(|| {
                let of = |n: f64, den: Option<f64>| den.filter(|d| *d > 0.0).map(|d| n * 100.0 / d);
                let prod = produced.map(|p| p as f64);
                // 손실 분모 = 지나간 seq 수: 언더런 은닉은 seq를 소비하지 않는다.
                let seqs = |d: &LossShapeStats| prod.map(|p| p - d.conceal_underrun as f64);
                Conceal {
                    produced,
                    concealed,
                    connected_s: a.connected as f64 * interval_ms as f64 / 1000.0,
                    pct: of(concealed as f64, prod),
                    fec_pct: delta.and_then(|d| of(d.conceal_fec as f64, prod)),
                    plc_pct: delta.and_then(|d| of(d.conceal_plc as f64, prod)),
                    pcm_pct: delta.and_then(|d| of(d.conceal_pcm as f64, prod)),
                    underrun_pct: delta.and_then(|d| of(d.conceal_underrun as f64, prod)),
                    loss_pct: delta.and_then(|d| of(d.estimate().lost, seqs(&d))),
                    dup_dropped: dup,
                }
            });
            let loss_shape =
                shapes.iter().filter(|((s, p, _), _)| *s == session && *p == name).map(|(_, sh)| ShapeSummary::of(sh)).collect();
            let frame_ms = a.entries.iter().rev().find_map(|e| e.frame_ms);
            // 첫 소리: 항목마다 하나. 없는 항목은 머문 시간(첫·마지막 샘플 + 한 간격 — 피어 추가는 첫 샘플 전이다)으로 가른다.
            let (mut no_audio_entries, mut no_audio_short) = (0, 0);
            for e in a.entries.iter().filter(|e| e.first_audio_ms.is_none()) {
                if (e.last_t - e.first_t + interval_ms) as f64 >= goal::FIRST_AUDIO_MS {
                    no_audio_entries += 1;
                } else {
                    no_audio_short += 1;
                }
            }
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
                connect_ms: a.entries.iter().filter_map(|e| e.connect_ms).collect(),
                first_audio_ms: a.entries.iter().filter_map(|e| e.first_audio_ms).collect(),
                no_audio_entries,
                no_audio_short,
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
        span_ms: lines.first().zip(lines.last()).map(|(a, b)| (t_of(a), t_of(b))),
        backend,
        backends,
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
    s.goal = session_goal(&s, interval_ms);
    s
}

fn fmt_s(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// GOAL 2-1(손실 1% 회선에서 은닉 ≤ 1.5%) 판정. `loss_pct`는 복구 후 손실(사본·재전송으로 메운 손실은 빠짐)이다.
/// - 손실이 조건 이상 + 은닉이 한도 안 → 충족(복구 후 손실 ≥ 1%면 회선 손실도 ≥ 1% — 조건 이상으로 나쁜 회선에서 통과).
/// - 손실이 조건 이하 + 은닉이 한도 밖 → 미달. 단 사본이 손실을 메웠으면(`dup_dropped` > 0, 또는 모름) 회선 손실이 1%보다 컸을 수
///   있어 가를 수 없다.
/// - 그 밖(손실이 적은 회선에서 한도 안, 손실이 큰 회선에서 한도 밖)은 조건이 달라 가를 수 없다.
fn conceal_verdict(conceal_pct: f64, loss_pct: f64, dup_dropped: Option<u64>) -> (Verdict, Option<String>) {
    let cond = goal::CONCEAL_LOSS_COND_PCT;
    let ok = conceal_pct <= goal::CONCEAL_MAX_PCT;
    match (loss_pct >= cond, loss_pct <= cond, ok) {
        (true, _, true) => (Verdict::Met, Some("수치만 — 청감은 [사람]".into())),
        (_, true, false) if dup_dropped == Some(0) => (Verdict::NotMet, None),
        (_, true, false) => (
            Verdict::Unknown,
            Some(format!(
                "복구 후 손실 {loss_pct:.2}% ≤ {cond}%지만 사본·재전송 {} — 회선 손실이 더 컸을 수 있어 가를 수 없음",
                dup_dropped.map(|d| format!("{d}개를 버림")).unwrap_or_else(|| "기록 없음".into())
            )),
        ),
        (_, _, true) => (Verdict::Unknown, Some(format!("복구 후 손실 {loss_pct:.2}% < 조건 {cond}% — 손실 1% 회선이 아니라 가를 수 없음(참고값)"))),
        _ => (Verdict::Unknown, Some(format!("복구 후 손실 {loss_pct:.2}% > 조건 {cond}% — 조건보다 나쁜 회선이라 가를 수 없음"))),
    }
}

/// 세션 GOAL 판정(1·2절). 원칙: 데이터가 모자라거나 측정 구간이 GOAL 정의와 다르면 판정 불가, 충족은 근거가 완전할 때만.
fn session_goal(s: &SessionReport, interval_ms: u64) -> Vec<GoalRow> {
    use Verdict::*;
    let mut out = Vec::new();
    let dur = fmt_s(s.duration_s);
    let class = s.backend.as_deref().map(backend_class);
    let mixed_backend = s.backends.len() > 1;

    // 1-1 로컬 처리 지연 — 충족은 세션 최대가 한도 안일 때만(중간에 버퍼가 바뀌어 한동안 넘었으면 중앙값이 가린다).
    let target_11 = format!("ASIO 128/2.5ms ≤ {}ms · WASAPI 독점 ≤ {}ms · 공유 ≤ {}ms", goal::EST_ASIO_MS, goal::EST_EXCLUSIVE_MS, goal::EST_SHARED_MS);
    match (&s.est_latency_ms, class) {
        (None, _) | (_, None) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, "없음".into(), Unknown, Some("실행 샘플·백엔드 기록 없음".into()))),
        (Some(d), Some(c)) => {
            let measured = format!("중앙 {:.2}ms · 최대 {:.2}ms ({})", d.median, d.max, s.backends.join(" → "));
            let frame = s.config.as_ref().map(|c| c.frame_ms);
            let limit = match c {
                _ if mixed_backend => Err(format!("세션 중 백엔드가 바뀜({}) — 한 기준으로 가를 수 없음", s.backends.join(" → "))),
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
                Ok(l) if d.max <= l => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, Met, Some(format!("이 백엔드 기준 ≤ {l}ms, 세션 최대가 한도 안")))),
                Ok(l) if d.median > l => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, NotMet, Some(format!("이 백엔드 기준 ≤ {l}ms, 중앙값부터 넘음")))),
                Ok(l) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, Unknown, Some(format!("중앙은 ≤ {l}ms지만 최대가 넘음 — 세션 중 설정이 바뀌었는지 기록표 확인")))),
                Err(why) => out.push(row("1-1", "로컬 처리 지연 estLatencyMs", target_11, measured, Unknown, Some(why))),
            }
        }
    }

    for p in &s.peers {
        // 1-3 지터 버퍼 추가 지연(안정 회선 = 유선). 연결 샘플이 모두 LAN 경로일 때만 가른다 — 경로가 섞이면 public 구간의 큰 지터가
        // 한도(F + p95 지터)를 키워 충족 쪽으로 치우친다.
        let t13 = "안정 회선(유선) bufferMs 중앙값 ≤ 1프레임 + p95 지터".to_string();
        let name13 = format!("지터 버퍼 추가 지연 — {}", p.peer);
        let secs = p.connected_samples as f64 * interval_ms as f64 / 1000.0;
        let lan_only = !p.paths.is_empty() && p.paths.keys().all(|k| k == "lan") && p.paths.values().sum::<usize>() == p.connected_samples;
        match (&p.buffer_ms, &p.jitter_ms, p.frame_ms) {
            (Some(b), Some(j), Some(f)) => {
                let limit = f + j.p95;
                let measured = format!("중앙 {:.2}ms vs 한도 {:.2}ms(F {f} + p95 지터 {:.2})", b.median, limit, j.p95);
                if !lan_only {
                    let paths = p.paths.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ");
                    out.push(row("1-3", &name13, t13, measured, Unknown, Some(format!("경로 {} — LAN만이 아님. public 경로도 유선이면 기록표 값으로 판단", if paths.is_empty() { "없음".into() } else { paths }))));
                } else if secs < goal::MIN_JUDGE_S {
                    out.push(row("1-3", &name13, t13, measured, Unknown, Some(format!("연결 {secs:.0}s < {}s", goal::MIN_JUDGE_S))));
                } else if b.median <= limit {
                    out.push(row("1-3", &name13, t13, measured, Met, Some("LAN 경로 — 유선인지는 기록표로 확인(Wi-Fi에서 통과했다면 유선은 더 쉽다)".into())));
                } else {
                    out.push(row("1-3", &name13, t13, measured, NotMet, Some("LAN 경로 — Wi-Fi였다면 안정 회선 대상 아님(기록표 확인)".into())));
                }
            }
            _ => out.push(row("1-3", &name13, t13, "없음".into(), Unknown, Some("연결 샘플의 bufferMs·jitterMs 또는 상대 프레임 길이(lossShape·remoteFrameMs) 없음".into()))),
        }

        // 1-4 지연 누적(±3ms 1분) — 세션 길이가 아니라 이 피어가 기록된 시간으로 60분을 본다(#246 M1).
        let t14 = format!("{}분 동안 bufferMs가 목표 ±{}ms 밖에 {}s 이상 머무르지 않음", goal::BAND_SESSION_S / 60.0, goal::BAND_MS, goal::BAND_MAX_S);
        let name14 = format!("지연 누적 — {}", p.peer);
        match &p.band {
            None => out.push(row("1-4", &name14, t14, "없음".into(), Unknown, Some("타임라인에 targetMs(목표 버퍼)가 없음 — #246 이전 형식".into()))),
            Some(b) => {
                let measured = format!(
                    "1분+ 구간 {} · 최장 {:.0}s · 밖 {:.1}% · 피어 기록 {} · 틈 {}",
                    b.over_limit,
                    b.longest_s,
                    b.outside_pct,
                    fmt_s(b.covered_s),
                    b.gaps
                );
                if b.over_limit > 0 {
                    out.push(row("1-4", &name14, t14, measured, NotMet, None));
                } else if b.gaps > 0 {
                    out.push(row("1-4", &name14, t14, measured, Unknown, Some("샘플이 빠진 틈이 있어 그 사이 상태를 모름 — 1분 이탈이 잘렸을 수 있음".into())));
                } else if b.covered_s < goal::BAND_SESSION_S {
                    out.push(row("1-4", &name14, t14, measured, Unknown, Some(format!("피어 기록 {} < 60분", fmt_s(b.covered_s)))));
                } else {
                    out.push(row("1-4", &name14, t14, measured, Met, None));
                }
            }
        }

        // 2-1 손실 1% 은닉 ≤ 1.5% — 분모는 재생 프레임(produced). 없으면 판정 불가(벽시계 시간으로 대신하지 않는다, #246 H1).
        let t21 = format!("손실 {}% 회선에서 은닉 ≤ {}%", goal::CONCEAL_LOSS_COND_PCT, goal::CONCEAL_MAX_PCT);
        let name21 = format!("은닉 — {}", p.peer);
        match &p.conceal {
            None => out.push(row("2-1", &name21, t21, "없음".into(), Unknown, Some("피어 항목 없음".into()))),
            Some(c) => {
                let pct = |v: Option<f64>| v.map(|v| format!("{v:.2}%")).unwrap_or_else(|| "없음".into());
                let measured = format!(
                    "은닉 {} ({}/{}프레임) · 복구 후 손실 {} · 연결 {}",
                    pct(c.pct),
                    c.concealed,
                    c.produced.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                    pct(c.loss_pct),
                    fmt_s(c.connected_s)
                );
                let v = if c.connected_s < goal::MIN_JUDGE_S {
                    (Unknown, Some(format!("연결 {} < {}s", fmt_s(c.connected_s), goal::MIN_JUDGE_S)))
                } else {
                    match (c.pct, c.loss_pct) {
                        (None, _) => (Unknown, Some("재생 프레임 수(produced) 없음(#246 이전 형식) — 은닉 비율의 분모를 몰라 판정 불가".into())),
                        (_, None) => (Unknown, Some("lossShape 없음(P2-17h 이전) — 회선 손실을 몰라 조건 확인 불가".into())),
                        (Some(cp), Some(l)) => conceal_verdict(cp, l, c.dup_dropped),
                    }
                };
                out.push(row("2-1", &name21, t21, measured, v.0, v.1));
            }
        }
    }

    // 2-2 권장 설정 xrun — 세션 내내 독점이어야 한다(중간에 공유로 폴백했으면 권장 설정 30분이 아니다).
    let t22 = format!("권장 설정(WASAPI 독점) {}분 xrun 0", goal::XRUN_WINDOW_S / 60.0);
    let note22 = "xruns는 전역 카운터 — 장치 콜백 놓침 외에 피어 소스 부족(망 원인)도 든다";
    match (s.xruns, class) {
        (None, _) | (_, None) => out.push(row("2-2", "xrun", t22, "없음".into(), Unknown, Some("실행 샘플 없음".into()))),
        (Some(x), Some(c)) => {
            let measured = format!("{x}회 · 세션 {dur}");
            if c != BackendClass::Exclusive || mixed_backend {
                out.push(row("2-2", "xrun", t22, measured, Unknown, Some(format!("백엔드 {} — 세션 내내 권장 설정(wasapiExclusive)이 아님", s.backends.join(" → ")))));
            } else if x > 0 {
                out.push(row("2-2", "xrun", t22, measured, NotMet, Some(note22.into())));
            } else if s.duration_s < goal::XRUN_WINDOW_S {
                out.push(row("2-2", "xrun", t22, measured, Unknown, Some(format!("세션 {dur} < 30분"))));
            } else {
                out.push(row("2-2", "xrun", t22, measured, Met, None));
            }
        }
    }
    out
}

/// 여러 zip의 `conn`과 첫 소리 → 연결 성공률 집계(후보 ③). `duplicate_of`가 있는 zip(같은 zip을 두 번 줌)은 뺀다.
pub(crate) fn aggregate_conn(files: &[FileReport]) -> ConnAgg {
    const RESULTS: [&str; 5] = ["lan", "public", "relay", "failed", "aborted"];
    // (결과, 단계) → (시도 수, 소요 ms들)
    let mut by: BTreeMap<(usize, bool), (usize, Vec<f64>)> = BTreeMap::new();
    let (mut n_sessions, mut with_relay, mut recover, mut switch, mut unknown_results) = (0, 0, 0, 0, 0);
    let (mut no_audio_entries, mut no_audio_short) = (0, 0);
    let mut first_audio_ms = Vec::new();
    // 시도 하한용: (zip 순번, 세션 시간 범위, 그 세션 시도 수)
    let mut spans: Vec<(usize, (u64, u64), usize)> = Vec::new();
    let mut duplicate_files = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        if f.duplicate_of.is_some() {
            duplicate_files.push(f.source.clone());
            continue;
        }
        for s in &f.sessions {
            n_sessions += 1;
            let mut relay_here = false;
            let mut attempts_here = 0;
            for c in &s.conn {
                relay_here |= c.result == "relay";
                match c.phase.as_str() {
                    "connect" | "reconnect" => {
                        // 알 수 없는 결과(새 형식)는 성공·실패로 추측하지 않는다 — 따로 세고 3-2를 가르지 않는다.
                        let Some(r) = RESULTS.iter().position(|r| *r == c.result) else {
                            unknown_results += 1;
                            continue;
                        };
                        let e = by.entry((r, c.phase == "reconnect")).or_default();
                        e.0 += 1;
                        e.1.extend(c.ms);
                        attempts_here += 1;
                    }
                    "recover" => recover += 1,
                    "switch" => switch += 1,
                    _ => {}
                }
            }
            with_relay += usize::from(relay_here);
            for p in &s.peers {
                first_audio_ms.extend(&p.first_audio_ms);
                no_audio_entries += p.no_audio_entries;
                no_audio_short += p.no_audio_short;
            }
            spans.push((fi, s.span_ms.unwrap_or((0, 0)), attempts_here));
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
        attempts_lower: attempts_lower_bound(&spans),
        unknown_results,
        duplicate_files,
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
        no_audio_entries,
        no_audio_short,
        rows,
    }
}

/// 실제 시도 수의 하한: 시간이 겹치는 세션들을 한 무리로 묶고(같은 합주를 여러 사람이 내보냈을 수 있다), 무리마다 zip별 시도 합 중
/// 가장 큰 것만 센다. 같은 zip의 세션은 시간이 겹치지 않으므로 겹침 없는 세션은 그대로 더해진다.
fn attempts_lower_bound(spans: &[(usize, (u64, u64), usize)]) -> usize {
    let mut v: Vec<&(usize, (u64, u64), usize)> = spans.iter().collect();
    v.sort_by_key(|(_, (a, _), _)| *a);
    let mut total = 0;
    let mut i = 0;
    while i < v.len() {
        let mut end = v[i].1 .1;
        let mut per_file: BTreeMap<usize, usize> = BTreeMap::new();
        let mut j = i;
        while j < v.len() && (j == i || v[j].1 .0 <= end) {
            end = end.max(v[j].1 .1);
            *per_file.entry(v[j].0).or_default() += v[j].2;
            j += 1;
        }
        total += per_file.values().copied().max().unwrap_or(0);
        i = j;
    }
    total
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
    let measured = format!(
        "성공 {}/{} = {} (실패 {} · 중단 {}) · 실제 시도 하한 {}",
        a.success,
        a.attempts,
        pct(a.success_pct),
        a.failed,
        a.aborted,
        a.attempts_lower
    );
    let both_sides = "시도는 기록 기준 — 한 합주의 양쪽 zip이면 같은 연결이 두 번 세어진다(문턱은 하한으로)";
    let v32 = match a.success_pct {
        None => (Unknown, Some("conn 기록 없음".into())),
        Some(p) if p < goal::CONNECT_PCT => (NotMet, Some(format!("중단(연결 중 퇴장·정지)도 분모에 든다 — 원인은 연결 기록 표에서. {both_sides}"))),
        Some(_) if a.unknown_results > 0 => (Unknown, Some(format!("결과를 모르는 시도 {}회(새 형식) — 성공·실패를 추측하지 않음", a.unknown_results))),
        Some(_) if a.attempts_lower < goal::MIN_ATTEMPTS_98 => (
            Unknown,
            Some(format!("실제 시도 하한 {}회 < {}회 — 98%를 가르기에 적음. {both_sides}", a.attempts_lower, goal::MIN_ATTEMPTS_98)),
        ),
        Some(_) => (Met, Some(both_sides.into())),
    };
    out.push(row("3-2", "연결 성공률(폴백 포함)", t32, measured, v32.0, v32.1));

    // 3-3 첫 소리: firstAudioMs는 엔진에 피어가 추가된 때부터라 GOAL의 "입장 후"보다 짧다 — 3초를 넘었거나 소리를 끝내 못 받았으면
    // 미달이지만, 3초 이내라고 충족이라 할 수는 없다(#246 H2).
    let t33 = format!("입장 후 {}초 이내 소리", goal::FIRST_AUDIO_MS / 1000.0);
    let over = a.first_audio_ms.iter().filter(|&&x| x > goal::FIRST_AUDIO_MS).count();
    let dist = Dist::of(&a.first_audio_ms);
    let measured = format!(
        "{}개{} · 3초 초과 {over} · 소리 못 받음(3초+ 머묾) {} · 짧게 머물고 소리 없음 {}",
        a.first_audio_ms.len(),
        dist.map(|d| format!(" · 중앙 {:.0}ms · 최대 {:.0}ms", d.median, d.max)).unwrap_or_default(),
        a.no_audio_entries,
        a.no_audio_short
    );
    let span = "firstAudioMs = 엔진에 피어가 추가된 때부터(입장 → 피어 추가 구간 미포함)";
    if over > 0 || a.no_audio_entries > 0 {
        out.push(row("3-3", "첫 소리", t33, measured, NotMet, Some(format!("{span} — 그 구간만으로도 넘음"))));
    } else if a.first_audio_ms.is_empty() {
        out.push(row("3-3", "첫 소리", t33, measured, Unknown, Some("firstAudioMs 기록 없음".into())));
    } else {
        out.push(row("3-3", "첫 소리", t33, measured, Unknown, Some(format!("{span} — 3초 이내인지는 기록표 체감값으로"))));
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
        duplicate_of: None,
        sessions,
    }
}

/// 예제 `diag_report`의 본체: zip 경로들 → Markdown(또는 `json`이면 같은 값의 JSON). 잘못된 zip은 오류 문장.
pub fn run(paths: &[String], json: bool) -> Result<String, String> {
    let diags = paths.iter().map(|p| read_zip(p)).collect::<Result<Vec<_>, _>>()?;
    let r = report(&diags);
    if json {
        serde_json::to_string_pretty(&r).map(|s| s + "\n").map_err(|e| format!("JSON 출력 실패: {e}"))
    } else {
        Ok(markdown(&r))
    }
}

/// zip 여러 개 → 보고서(세션별 + 연결 집계 + 3절 판정).
pub(crate) fn report(diags: &[Diag]) -> Report {
    let mut files: Vec<FileReport> = diags.iter().map(analyze).collect();
    // 같은 zip을 두 번 주면(이름만 달라도) 타임라인이 똑같다 — 뒤의 것을 집계에서 뺀다(#246 M2).
    for i in 1..diags.len() {
        if !diags[i].lines.is_empty() {
            if let Some(j) = (0..i).find(|&j| diags[j].lines == diags[i].lines) {
                files[i].duplicate_of = Some(diags[j].source.clone());
            }
        }
    }
    let conn = aggregate_conn(&files);
    let goal = conn_goal(&conn);
    Report { files, conn, goal }
}

#[cfg(test)]
mod tests;
