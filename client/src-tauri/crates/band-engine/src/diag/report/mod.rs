//! 진단 zip → 결과표(GOAL-COVERAGE 후보 ②·③). 사람이 실장비·실회선 세션 뒤 "진단 내보내기" zip만 보내면 코디네이터가
//! `cargo run -p band-engine --example diag_report -- <zip>...`으로 GOAL 판정에 필요한 숫자를 얻는다.
//!
//! 읽는 것(`export.rs` 머리말의 zip 구성): `meta.json`(앱 버전·형식·시각), `engine.json`(마지막 세션 설정 — 드라이버·프리셋·
//! 프레임), `timeline.jsonl`(1초 샘플 — 세션·백엔드·xrun·코어 배치·피어·`conn`). 줄은 `serde_json::Value`로 읽고 필드가 없으면
//! "없음"으로 둔다 — 옛 형식(P1-18 `conn`, P2-17h `lossShape`, P1-21 `corePlacement` 이전)도 죽지 않는다. 형식이 바뀌어 필드
//! 이름이 달라지면 시험 `report_reads_real_export`(실제 `diag::export::build`로 만든 zip을 읽는다)가 깨진다. 설정은 `EngineConfig`,
//! 손실 모양은 `LossShapeStats` 타입으로 그대로 역직렬화한다.
//!
//! meta.format 3(진단 값 노출)부터 읽는 것: 피어 `rebuffers`·`rebufferMs`(리버퍼 실측 — 2-1 "소리 없는 시간"은 이 값과 기대치
//! 비교 추정 중 **큰 쪽**, #250 H2), `entryLeadMs`(내 입장 → 피어 추가 — 시작 때 있던 연주자에만 있다. 3-3을 `entryLeadMs +
//! firstAudioMs`로, 없는 항목은 참고값, #250 M1), 줄의 `nat`(STUN 매핑 추정)·`selfPeer`(내보낸 쪽 가명 — 3-1 콘형 쌍의 짝 맞추기,
//! #250 H1), `sendQueueP99Ms`·`relay`·`txCodec`(요약 표). format 3 미만이면 이 값들이 없다고 보고 예전 방식(추정·판정 불가)을 쓴다.
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
    /// 도구 규칙(GOAL에 없음, #246 H1''·H1'''): 피어마다 **첫 연결 구간**(이어진 `connected` 표본 묶음)의 "소리 없는 시간"에서만
    /// 측정 오차로 빼 주는 시간(s). 2-1 상한(소리 없는 시간을 모두 은닉으로 본 값)에 넣는 무음은 첫 구간이 `max(0, 무음 − 이 값)`,
    /// 그 뒤 구간(재연결·재입장)은 무음 전부다. 그래서 피어 하나에서 빼는 총량은 구간 수와 상관없이 이 값 이하다.
    /// - 구간마다 빼면 재연결이 잦을 때 "구간 수 × 0.5s"가 빠져, 무음 50%인 세션이 충족으로 나왔다(#246 4차 리뷰 H1'''). 재연결 뒤
    ///   지터 버퍼를 다시 채우는 시간은 측정 오차가 아니라 듣는 사람에게 실제로 끊긴 시간이다. 뒤 구간 양 끝의 시각 어긋남(수십 ms)도
    ///   상한에 들어가는데, 충족을 어렵게 만드는 쪽이라 그대로 둔다.
    /// - 구간 안에서는 기록 시각 차이와 `produced` 차분이 이어 더해진다(망원 합). 그래서 1초 표본 간격의 흔들림과 장치 주기 반올림은
    ///   상쇄되고 구간 양 끝만 남는다: 첫 연결 표본 뒤 지터 버퍼를 채우는 동안(목표 깊이 수십 ms + 첫 패킷 대기, 수백 ms 이하)과
    ///   양 끝 표본의 기록 시각 ↔ 통계 스냅숏 시각 어긋남(수십 ms). 둘 다 무음을 늘리는 쪽이고 0.5s 안이다.
    /// - 기대 시간에 비례하는 허용치는 두지 않는다. 예전 허용치(기대 시간의 2% + 구간마다 1s)는 GOAL 한도 1.5%보다 커서, 그 아래
    ///   무음이 통째로 빠지고 충족이 났다(#246 3차 리뷰 H1'').
    /// - 장치 클록과 시스템 시계의 차(보통 0.01% 안팎, 넓게 잡아 0.1%)는 빼 주지 않는다. 장치가 느리면 가짜 무음이 상한에 그대로
    ///   들어가 충족을 어렵게 만드는 쪽이다. 장치가 빠르면 같은 구간의 진짜 무음을 그만큼 가린다(은닉 상한이 클록 몫 최대 약 0.1%p,
    ///   여기에 첫 구간 끝 오차 0.5s 몫 — 3600s 세션이면 약 0.014%p — 만큼 낮게 나옴, #246 L1). 보통 클록 차 0.01%면 0.01%p
    ///   수준이다. 구간마다 따로 0 아래를 자르므로(`PeerAcc::close_run`) 그 몫이 다른 구간의 무음까지 지우지는 않는다(#246 L2'').
    pub(crate) const SILENT_EDGE_S: f64 = 0.5;
    /// 도구 규칙(GOAL에 없음): 성공률 98%를 "충족"으로 가르려면 시도 50회 이상(1회 실패 = 2%p). 그보다 적으면 실패가
    /// 있어 98% 아래일 때만 "미달"이다.
    pub(crate) const MIN_ATTEMPTS_98: usize = 50;
    /// 도구 규칙(GOAL에 없음, #250 M2): 3-1 "콘형 NAT 쌍 직접 100%"를 충족으로 가르는 최소 표본. 짝이 확인된 콘형 쌍이
    /// `MIN_CONE_PAIRS` 이상이고, 그 안의 **실제 시도 수 하한**(무리마다 엔드포인트별 시도 수 중 큰 쪽 — 같은 연결이 양쪽 zip에 한 번씩
    /// 남으므로 합하지 않는다)이 `MIN_CONE_ATTEMPTS` 이상이어야 한다. 3회는 TEST-SESSION의 반복 횟수(다른 네트워크 연결 3회, P1-20
    /// 행)와 같다 — 한 번의 연결로 "100%"를 내지 않는다. 그보다 적으면 판정 불가(직접이 아닌 시도가 있으면 수와 상관없이 미달).
    pub(crate) const MIN_CONE_PAIRS: usize = 1;
    pub(crate) const MIN_CONE_ATTEMPTS: usize = 3;
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
    /// 이어진 연결 표본 쌍 사이 시간의 합(s, 기록 시각 `t` 차이) — 재생을 기대하는 시간
    pub(crate) playing_span_s: f64,
    /// 소리 없는 시간(s): 기대 재생 시간 − 재생 프레임 × F(연결 구간마다 계산하고 음수 구간은 0, #246 L2''). 200ms 넘는 무수신 뒤
    /// 지터 버퍼가 리셋되면 패킷이 다시 와 채울 때까지 `produced`도 `concealed`도 늘지 않는다(`playout.rs` `STARVE_RESET_MS`, 피어는
    /// 계속 `connected`). 그 시간은 은닉 %에 안 보이므로 따로 센다(#246 H1'). 쌍 하나라도 `produced`·상대 프레임 길이가 없으면 None
    pub(crate) silent_s: Option<f64>,
    /// 모든 연결 구간에 엔진 리버퍼 실측(`rebufferMs` 차분, format 3)이 있다. 참이어도 `silent_s`는 구간마다 실측과 기대치 추정 중
    /// **큰 쪽**이다(#250 H2): 실측은 처음 채우기(연결됐지만 소리 미도착)·리버퍼로 세지 않는 정지(워커·출력 스트림 정지)·카운터
    /// 리셋을 못 보고, 추정은 장치 클록이 빠를 때 진짜 무음을 조금 가린다 — 보수적인 쪽을 쓴다.
    pub(crate) silent_measured: bool,
    /// 리버퍼 실측 합(s, 모든 구간에 카운터가 있을 때만). 없으면 None
    pub(crate) silent_counter_s: Option<f64>,
    /// 기대치 비교로 추정한 소리 없는 시간(s, 구간마다 0 아래를 자른 합). 모르면 None
    pub(crate) silent_est_s: Option<f64>,
    /// 무수신 재버퍼 횟수(`rebuffers` 차분 합, format 3). 없으면 None
    pub(crate) rebuffers: Option<u64>,
    /// 2-1 상한에 넣은 소리 없는 시간(s): 구간마다 `max(실측, 추정 − 끝 오차)` — 끝 오차(`SILENT_EDGE_S`)는 첫 구간의 추정에서만
    /// 뺀다(#246 H1''', #250 H2). 실측이 없는 구간(format 3 미만)은 추정만
    pub(crate) silent_hi_s: Option<f64>,
    /// 연결 구간 수(재생 기록이 있는 이어진 `connected` 표본 쌍 묶음). 2 이상이면 재연결·재입장이 있었다(#246 L3)
    pub(crate) runs: usize,
    /// 은닉 하한(%): 은닉 ÷ 재생 프레임 — 소리 없는 시간은 은닉이 아니라고 본 값. `produced`가 없거나 0이면 None
    pub(crate) pct: Option<f64>,
    /// 은닉 상한(%): (은닉 + 무음 프레임) ÷ (재생 + 무음 프레임), 무음 프레임 = `silent_hi_s` ÷ F. 소리 없는 시간을 모르면 None
    pub(crate) pct_hi: Option<f64>,
    /// 방식별(lossShape·produced 없으면 None)
    pub(crate) fec_pct: Option<f64>,
    pub(crate) plc_pct: Option<f64>,
    pub(crate) pcm_pct: Option<f64>,
    pub(crate) underrun_pct: Option<f64>,
    /// 복구 후 손실(lossShape 추정 손실 ÷ 지나간 seq 수 = 재생 − 언더런 은닉). 사본·재전송으로 메운 손실은 빠지므로 실제 회선
    /// 손실 이하다. lossShape·produced 없으면 None
    pub(crate) loss_pct: Option<f64>,
    /// 복구 후 손실 상한(%): (손실 + 무음 프레임) ÷ (지나간 seq + 무음 프레임). 무음 동안의 패킷은 리셋 때문에 손실에 잡히지 않는데,
    /// 그때 회선이 끊겼을 수 있다. 그래서 하한만 보고 "손실 1% 이하 회선"이라 할 수 없다(#246 H1'' 점검). 소리 없는 시간을 모르면 None
    pub(crate) loss_pct_hi: Option<f64>,
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
    /// 입장 후 첫 소리(ms, 항목마다 `entryLeadMs + firstAudioMs`, format 3) — GOAL 3-3 정의 구간
    pub(crate) entry_first_audio_ms: Vec<f64>,
    /// 첫 소리는 있는데 `entryLeadMs`가 없는 항목 수(옛 클라이언트·옛 형식) — 3-3 충족을 막는다
    pub(crate) no_lead_entries: usize,
    /// 내가 이 피어에게 보낸 코덱(연결 샘플 수, `opus`·`pcm16`, 자동 전환이면 `opus(자동: 이유)`)
    pub(crate) tx_codec: BTreeMap<String, usize>,
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
    /// STUN 매핑 추정(`nat.mapping`: open | independent | dependent | unknown, format 3)과 응답 서버 수. 없으면 None
    pub(crate) nat: Option<String>,
    pub(crate) nat_responses: Option<u64>,
    /// 릴레이 등록 상태(실행 샘플에 나온 순서, 중복 없이)와 엔진 ↔ 릴레이 RTT. 릴레이를 안 썼으면 빈 목록·None
    pub(crate) relay_states: Vec<String>,
    pub(crate) relay_rtt_ms: Option<Dist>,
    /// 송신 큐 p99(ms, 실행 샘플마다 직전 기록값을 이어 쓴 분포 — `sendQueueP99Ms`는 바뀐 줄에만 있다, format 3)
    pub(crate) send_queue_p99_ms: Option<Dist>,
    /// 세션 누적 xrun(마지막 실행 샘플)
    pub(crate) xruns: Option<u64>,
    pub(crate) xrun_steps: Vec<XrunStep>,
    pub(crate) stream_errors: Option<u64>,
    pub(crate) peers: Vec<PeerReport>,
    pub(crate) conn: Vec<ConnRow>,
    /// 내보낼 때 아직 연결 중(`connecting`·`reconnecting`)이던 시도 수 — 결과 이벤트가 아직 없다(#246 L2'). 세션이 끝난 기록이면
    /// 0(엔진 정지가 남은 시도를 `aborted`로 남긴다)
    pub(crate) pending: usize,
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
    /// 내보낼 때 아직 연결 중이던 시도(결과 없음) — 분모(`attempts`)에 들고 성공으로 세지 않는다. 3-2는 이들이 모두 성공·모두 실패인
    /// 두 경우가 같은 판정일 때만 가른다(#246 L2')
    pub(crate) pending: usize,
    /// 성공 ÷ 시도(중단·미완 포함 분모 — 빼면 "연결 중" 퇴장이 빠져 위로 치우친다, `net::connlog` 머리말)
    pub(crate) success_pct: Option<f64>,
    /// 직접(lan/public) ÷ 시도
    pub(crate) direct_pct: Option<f64>,
    pub(crate) recover: usize,
    pub(crate) switch: usize,
    /// 릴레이로 붙은(시도·recover·switch 결과 relay) 세션 수
    pub(crate) sessions_with_relay: usize,
    /// 첫 소리(ms, 피어 항목마다 `firstAudioMs`)
    pub(crate) first_audio_ms: Vec<f64>,
    /// 입장 후 첫 소리(ms, `entryLeadMs + firstAudioMs`)와 그 값이 없는(첫 소리만 있는) 항목 수
    pub(crate) entry_first_audio_ms: Vec<f64>,
    pub(crate) no_lead_entries: usize,
    /// 세션 수: NAT 매핑 추정별(`없음` = format 3 이전)
    pub(crate) nat_sessions: BTreeMap<String, usize>,
    /// 콘형 쌍으로 확인된 무리(시간이 겹치는 세션들 — 참가자 모두의 zip이 있고 모두 independent·open) 수와 그 안의 시도(양쪽 합):
    /// 직접(lan/public) · 직접 아님(relay/failed) · 결과 모름(aborted/미완)
    pub(crate) cone_groups: usize,
    pub(crate) cone_attempts: usize,
    pub(crate) cone_direct: usize,
    pub(crate) cone_nondirect: usize,
    pub(crate) cone_unresolved: usize,
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
    /// 입장 → 피어 추가(ms, `entryLeadMs`)
    entry_lead_ms: Option<f64>,
    /// 리버퍼 (횟수, ms) 누적(format 3 — 키 없음 = 0)
    first_rebuf: Option<(u64, f64)>,
    last_rebuf: Option<(u64, f64)>,
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
    /// 직전 표본 (t, 연결?, produced, 리버퍼 ms) — 소리 없는 시간 계산용
    prev: Option<(u64, bool, Option<u64>, Option<f64>)>,
    /// 이어진 연결 표본 쌍 사이 시간 합(ms) — 재생을 기대한 시간
    play_span_ms: f64,
    /// 지금 연결 구간: (기대 시간 ms, 재생 프레임 × F ms, 마지막 F, 리버퍼 실측 ms — 쌍 하나라도 카운터가 없으면 None)
    run: Option<(f64, f64, f64, Option<f64>)>,
    /// 닫힌 구간 중 리버퍼 실측이 있는 수와 그 합(ms), 기대치 추정 무음 합(ms)
    measured_runs: usize,
    silent_counter_ms: f64,
    silent_est_ms: f64,
    /// 연결 샘플의 내 송신 코덱
    tx_codec: BTreeMap<String, usize>,
    /// 닫힌 구간들: 소리 없는 시간 합(ms, 구간마다 음수는 0), 상한에 넣는 무음(ms·프레임, 첫 구간만 − `SILENT_EDGE_S`)
    silent_ms: f64,
    silent_hi_ms: f64,
    silent_hi_frames: f64,
    /// 닫힌 구간 수 — 0이면 지금 구간이 첫 구간(끝 오차를 뺄 구간)이다
    runs: usize,
    /// 쌍에 produced나 상대 프레임 길이가 없었다 — 소리 없는 시간을 모른다
    play_unknown: bool,
}

impl PeerAcc {
    /// 연결 구간을 닫는다. 구간마다 따로 0 아래를 자르므로, 한 구간의 음수(장치 클록이 빠름)가 다른 구간의 진짜 무음을 지우지 않는다(#246 L2'').
    /// 끝 오차(`SILENT_EDGE_S`)는 첫 구간에서만 뺀다 — 빼는 총량이 구간 수에 비례하지 않게(#246 H1''').
    /// 리버퍼 실측(format 3)이 있으면 구간마다 실측과 추정 중 **큰 쪽**을 쓴다(#250 H2). 실측만 쓰면 카운터가 못 보는 무음 — 처음
    /// 채우기(연결됐지만 소리 미도착), 리버퍼로 세지 않는 정지(워커·출력 스트림 정지/재시작: 지터는 `Playing`인데 pop이 안 됨),
    /// 카운터 리셋 — 이 상한에서 통째로 빠져 충족이 났다. 실측은 끝 오차 없이 전부, 추정은 첫 구간에서만 끝 오차를 뺀 값으로 견준다.
    /// 장치 클록 차로 생기는 추정 오차(±200ppm이면 60분에 0.72초)는 장치가 느리면 충족을 어렵게 하는 쪽이고, 빠르면 실측이 받친다.
    fn close_run(&mut self) {
        if let Some((span, got, f, counter)) = self.run.take() {
            let est = (span - got).max(0.0);
            self.silent_est_ms += est;
            let edge = if self.runs == 0 { goal::SILENT_EDGE_S * 1000.0 } else { 0.0 };
            let est_hi = (est - edge).max(0.0);
            let (silent, hi) = match counter {
                Some(c) => {
                    self.measured_runs += 1;
                    self.silent_counter_ms += c;
                    (c.max(est), c.max(est_hi))
                }
                None => (est, est_hi),
            };
            self.runs += 1;
            self.silent_ms += silent;
            self.silent_hi_ms += hi;
            self.silent_hi_frames += hi / f;
        }
    }
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

/// `counters`: meta.format ≥ 3 — 리버퍼 카운터(`rebuffers`·`rebufferMs`)의 키가 없으면 0이다.
fn analyze_session(session: u64, lines: &[&Value], cfg: Option<ConfigSummary>, interval_ms: u64, counters: bool) -> SessionReport {
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
            let entry_lead_ms = f64_at(p, "entryLeadMs");
            let rebuf = match (u64_at(p, "rebuffers"), f64_at(p, "rebufferMs")) {
                (None, None) if !counters => None,
                (n, ms) => Some((n.unwrap_or(0), ms.unwrap_or(0.0))),
            };
            // 새 항목: 직전 실행 샘플에 없었거나 누적이 줄었다(재입장).
            let new_entry = match (acc.last_idx, acc.entries.last()) {
                (Some(li), Some(e)) => {
                    li + 1 != idx
                        || concealed < e.last_concealed
                        || matches!((produced, e.last_produced), (Some(a), Some(b)) if a < b)
                        || matches!((shape, e.last_shape), (Some(a), Some(b)) if shape::weight(&a) < shape::weight(&b))
                        || matches!((rebuf, e.last_rebuf), (Some(a), Some(b)) if a.0 < b.0 || a.1 < b.1)
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
                    entry_lead_ms,
                    first_rebuf: rebuf,
                    last_rebuf: rebuf,
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
                e.entry_lead_ms = e.entry_lead_ms.or(entry_lead_ms);
                e.last_rebuf = rebuf;
            }
            acc.last_idx = Some(idx);

            let connected = str_at(p, "state") == Some("connected");
            // 소리 없는 시간(#246 H1'): 같은 항목에서 이어진 두 연결 표본 사이에 기대한 재생 시간과 실제 재생(produced 차분 × F)을
            // 더한다. 쌍마다 기록 시각 차이를 쓰므로 표본 간격 흔들림은 합에서 상쇄된다. format 3부터는 엔진 리버퍼 실측(`rebufferMs`
            // 차분)도 같은 쌍에서 더해 두고, 구간을 닫을 때 실측이 있으면 그것을 쓴다(`PeerAcc::close_run`).
            let rebuf_ms = rebuf.map(|r| r.1);
            match acc.prev {
                Some((pt, true, pp, pr)) if connected && !new_entry => {
                    let f = frame.or_else(|| acc.entries.last().and_then(|e| e.frame_ms));
                    match (pp, produced, f) {
                        (Some(a), Some(b), Some(f)) => {
                            let span = t.saturating_sub(pt) as f64;
                            acc.play_span_ms += span;
                            let measured = pr.zip(rebuf_ms).map(|(x, y)| (y - x).max(0.0));
                            let r = acc.run.get_or_insert((0.0, 0.0, f, Some(0.0)));
                            *r = (r.0 + span, r.1 + b.saturating_sub(a) as f64 * f, f, r.3.zip(measured).map(|(x, y)| x + y));
                        }
                        _ => acc.play_unknown = true,
                    }
                }
                _ => acc.close_run(),
            }
            acc.prev = Some((t, connected, produced, rebuf_ms));
            let buffer = f64_at(p, "bufferMs");
            if let Some(target) = f64_at(p, "targetMs") {
                // 재생이 멈춘 표본은 띠 밖으로 센다(`tests/sim/long.rs`와 같다): 연결이 아니거나 bufferMs 0(지터 버퍼가 `Playing`이
                // 아니면 0을 낸다 — 목표가 3ms 이하이면 |0 − 목표| ≤ 3이라 띠 안으로 셀 뻔했다, #246 L1').
                let inside = connected && buffer.is_some_and(|b| b > 0.0 && (b - target).abs() <= goal::BAND_MS);
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
            if let Some(c) = str_at(p, "txCodec") {
                let auto = p.get("txCodecAuto").and_then(Value::as_bool).unwrap_or(false);
                let key = match (auto, str_at(p, "txCodecReason")) {
                    (true, Some(r)) => format!("{c}(자동: {r})"),
                    (true, None) => format!("{c}(자동)"),
                    _ => c.to_string(),
                };
                *acc.tx_codec.entry(key).or_default() += 1;
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
        .map(|(name, mut a)| {
            a.close_run();
            // 은닉·손실: 항목마다 누적 차분을 더한다. 분모는 재생 프레임(produced) — 하나라도 없으면 비율을 내지 않는다.
            let mut concealed = 0u64;
            let mut produced: Option<u64> = Some(0);
            let mut dup: Option<u64> = Some(0);
            let mut delta: Option<LossShapeStats> = None;
            let mut shape_all = true;
            let mut rebuffers: Option<u64> = Some(0);
            for e in &a.entries {
                rebuffers = match (rebuffers, e.first_rebuf, e.last_rebuf) {
                    (Some(n), Some(r0), Some(r1)) => Some(n + r1.0.saturating_sub(r0.0)),
                    _ => None,
                };
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
                // 상한에 넣는 무음(구간별 무음 − 끝 오차). 연결 쌍이 하나도 없으면 무음 0이라 F가 없어도 된다.
                let silent_hi = (!a.play_unknown).then_some((a.silent_hi_ms, a.silent_hi_frames));
                let hi = |n: f64, den: Option<f64>| match (den, silent_hi) {
                    (Some(d), Some((_, sf))) if d + sf > 0.0 && d > 0.0 => Some((n + sf) * 100.0 / (d + sf)),
                    _ => None,
                };
                Conceal {
                    produced,
                    concealed,
                    connected_s: a.connected as f64 * interval_ms as f64 / 1000.0,
                    playing_span_s: a.play_span_ms / 1000.0,
                    silent_s: (!a.play_unknown).then_some(a.silent_ms / 1000.0),
                    silent_measured: a.runs > 0 && a.measured_runs == a.runs,
                    silent_counter_s: (a.runs > 0 && a.measured_runs == a.runs).then_some(a.silent_counter_ms / 1000.0),
                    silent_est_s: (!a.play_unknown).then_some(a.silent_est_ms / 1000.0),
                    rebuffers,
                    silent_hi_s: silent_hi.map(|(ms, _)| ms / 1000.0),
                    runs: a.runs,
                    pct: of(concealed as f64, prod),
                    pct_hi: hi(concealed as f64, prod),
                    fec_pct: delta.and_then(|d| of(d.conceal_fec as f64, prod)),
                    plc_pct: delta.and_then(|d| of(d.conceal_plc as f64, prod)),
                    pcm_pct: delta.and_then(|d| of(d.conceal_pcm as f64, prod)),
                    underrun_pct: delta.and_then(|d| of(d.conceal_underrun as f64, prod)),
                    loss_pct: delta.and_then(|d| of(d.estimate().lost, seqs(&d))),
                    loss_pct_hi: delta.and_then(|d| hi(d.estimate().lost, seqs(&d))),
                    dup_dropped: dup,
                }
            });
            let loss_shape =
                shapes.iter().filter(|((s, p, _), _)| *s == session && *p == name).map(|(_, sh)| ShapeSummary::of(sh)).collect();
            let frame_ms = a.entries.iter().rev().find_map(|e| e.frame_ms);
            // 첫 소리: 항목마다 하나. 없는 항목은 머문 시간(첫·마지막 샘플 + 한 간격 — 피어 추가는 첫 샘플 전이다)으로 가른다. 입장 → 추가
            // (`entryLeadMs`)를 알면 그만큼 더한다 — 입장부터 3초가 지났는데 소리가 없으면 미달이다.
            let (mut no_audio_entries, mut no_audio_short) = (0, 0);
            for e in a.entries.iter().filter(|e| e.first_audio_ms.is_none()) {
                if (e.last_t - e.first_t + interval_ms) as f64 + e.entry_lead_ms.unwrap_or(0.0) >= goal::FIRST_AUDIO_MS {
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
                entry_first_audio_ms: a.entries.iter().filter_map(|e| Some(e.entry_lead_ms? + e.first_audio_ms?)).collect(),
                no_lead_entries: a.entries.iter().filter(|e| e.first_audio_ms.is_some() && e.entry_lead_ms.is_none()).count(),
                tx_codec: a.tx_codec,
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

    // 미완 시도: 세션 마지막 줄이 실행 중(내보낼 때 아직 합주 중)이면 그 줄에서 연결 중인 피어마다 결과 없는 시도가 하나 있다.
    let pending = lines
        .last()
        .filter(|v| running(v))
        .and_then(|v| v.get("peers"))
        .and_then(Value::as_array)
        .map(|ps| ps.iter().filter(|p| matches!(str_at(p, "state"), Some("connecting" | "reconnecting"))).count())
        .unwrap_or(0);

    let est: Vec<f64> = run.iter().filter_map(|v| f64_at(v, "estLatencyMs")).filter(|x| *x > 0.0).collect();
    // NAT 추정(바뀐 줄에만 — 세션의 마지막 값), 릴레이 상태·RTT, 송신 큐 p99(바뀐 줄에만 — 앞 값을 이어 쓴다).
    let nat = lines.iter().rev().find_map(|v| v.get("nat"));
    let mut relay_states: Vec<String> = Vec::new();
    let mut relay_rtt = Vec::new();
    let mut send_queue = Vec::new();
    let mut sq_last: Option<f64> = None;
    for v in &run {
        if let Some(r) = v.get("relay").filter(|r| r.is_object()) {
            if let Some(st) = str_at(r, "state") {
                if !relay_states.iter().any(|x| x == st) {
                    relay_states.push(st.to_string());
                }
            }
            relay_rtt.extend(f64_at(r, "rttMs"));
        }
        sq_last = f64_at(v, "sendQueueP99Ms").or(sq_last);
        send_queue.extend(sq_last);
    }
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
        // #245: threadPolicy는 같은 세션에서 값이 바뀐 줄(세션 첫 줄, pending 뒤 다음 줄)에만 있다 — 그 세션의 마지막 값을 쓴다.
        thread_policy: lines.iter().rev().find_map(|v| v.get("threadPolicy")).cloned(),
        nat: nat.and_then(|n| str_at(n, "mapping")).map(str::to_string),
        nat_responses: nat.and_then(|n| u64_at(n, "responses")),
        relay_states,
        relay_rtt_ms: Dist::of(&relay_rtt),
        send_queue_p99_ms: Dist::of(&send_queue),
        xruns: last_run.and_then(|v| u64_at(v, "xruns")),
        xrun_steps,
        stream_errors: last_run.and_then(|v| u64_at(v, "streamErrors")),
        peers,
        conn,
        pending,
        goal: Vec::new(),
    };
    s.goal = session_goal(&s, interval_ms);
    s
}

fn fmt_s(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// (하한, 상한) % 표기: 같으면 하나, 다르면 "하한–상한%", 상한을 모르면 하한만(2-1 행과 피어 표가 같이 쓴다).
pub(crate) fn pct_range(lo: Option<f64>, hi: Option<f64>) -> String {
    match (lo, hi) {
        (Some(lo), Some(hi)) if hi - lo >= 0.005 => format!("{lo:.2}–{hi:.2}%"),
        (Some(lo), _) => format!("{lo:.2}%"),
        (None, _) => "없음".into(),
    }
}

/// 소리 없는 시간 표기(2-1 행과 피어 표가 같이 쓴다). 리버퍼 실측(format 3)이 있으면 실측·추정을 나란히 적는다 — 쓰는 값은 구간마다
/// 큰 쪽이다(#250 H2).
pub(crate) fn silent_text(c: &Conceal) -> String {
    let base = match c.silent_s {
        Some(s) if c.playing_span_s > 0.0 => format!("{s:.1}s({:.1}%)", s * 100.0 / c.playing_span_s),
        Some(s) => format!("{s:.1}s"),
        None => return "모름(produced·상대 프레임 미기록)".into(),
    };
    match (c.silent_measured, c.silent_counter_s) {
        (true, Some(m)) => {
            let est = c.silent_est_s.map(|e| format!(" · 기대치 추정 {e:.1}s")).unwrap_or_default();
            format!("{base}(리버퍼 실측 {m:.1}s · 재버퍼 {}회{est} — 구간마다 큰 쪽)", c.rebuffers.unwrap_or(0))
        }
        _ => base,
    }
}

/// GOAL 2-1(손실 1% 회선에서 은닉 ≤ 1.5%) 판정. `loss_pct`는 복구 후 손실(사본·재전송으로 메운 손실은 빠짐)이다.
/// - 손실이 조건 이상 + 은닉이 한도 안 → 충족(복구 후 손실 ≥ 1%면 회선 손실도 ≥ 1% — 조건 이상으로 나쁜 회선에서 통과).
/// - 손실이 조건 이하 + 은닉이 한도 밖 → 미달. 단 사본이 손실을 메웠으면(`dup_dropped` > 0, 또는 모름) 회선 손실이 1%보다 컸을 수
///   있어 가를 수 없다.
/// - 그 밖(손실이 적은 회선에서 한도 안, 손실이 큰 회선에서 한도 밖)은 조건이 달라 가를 수 없다.
///
/// 은닉과 손실은 (하한, 상한) 구간으로 받는다(#246 H1''). 하한은 소리 없는 시간을 빼고 본 값, 상한은 그 시간을 모두 은닉·손실로 본
/// 값이다. 충족은 가장 나쁜 쪽(은닉 상한, 손실 하한)으로도 성립할 때만, 미달은 가장 좋은 쪽(은닉 하한, 손실 상한)으로도 성립할 때만
/// 낸다. 그 사이는 판정 불가다. `silent`는 비고용 (소리 없는 시간, 상한에 넣은 시간) s다.
fn conceal_verdict(conceal: (f64, f64), loss: (f64, f64), dup_dropped: Option<u64>, silent: (f64, f64)) -> (Verdict, Option<String>) {
    let (cond, max) = (goal::CONCEAL_LOSS_COND_PCT, goal::CONCEAL_MAX_PCT);
    let range = |(lo, hi): (f64, f64)| if hi > lo { format!("{lo:.2}–{hi:.2}%") } else { format!("{lo:.2}%") };
    let silent_note = || format!("소리 없는 시간 {:.1}s(상한에 {:.1}s를 은닉·손실로 넣음)", silent.0, silent.1);
    // 리버퍼 실측(format 3)이 있어도 상한에 넣는 시간은 구간마다 실측과 추정 중 큰 쪽이다(#250 H2) — 판정 규칙은 같다.
    if loss.0 >= cond && conceal.1 <= max {
        return (Verdict::Met, Some("수치만 — 청감은 [사람]".into()));
    }
    if loss.1 <= cond && conceal.0 > max {
        return match dup_dropped {
            Some(0) => (Verdict::NotMet, None),
            d => (
                Verdict::Unknown,
                Some(format!(
                    "복구 후 손실 {} ≤ {cond}%지만 사본·재전송 {} — 회선 손실이 더 컸을 수 있어 가를 수 없음",
                    range(loss),
                    d.map(|d| format!("{d}개를 버림")).unwrap_or_else(|| "기록 없음".into())
                )),
            ),
        };
    }
    let note = if conceal.0 <= max && max < conceal.1 {
        // 손실도 조건 밖이면 같이 적는다 — 무음 쪽이 풀려도 손실 1% 회선이 아니면 가를 수 없다(#246 L2)
        let loss_also = if loss.0 >= cond {
            String::new()
        } else if loss.1 <= cond {
            format!("; 복구 후 손실 {}도 조건 {cond}% 아래라 손실 1% 회선이 아님", range(loss))
        } else {
            format!("; 복구 후 손실 {}도 조건 {cond}%에 걸침", range(loss))
        };
        format!(
            "은닉 하한 {:.2}% ≤ {max}% < 상한 {:.2}% — {}, 그 시간이 은닉이었는지 몰라 가를 수 없음{loss_also}",
            conceal.0,
            conceal.1,
            silent_note()
        )
    } else if loss.0 < cond && cond < loss.1 {
        format!("복구 후 손실 {}가 조건 {cond}%에 걸침 — {}, 무음 동안 회선이 끊겼는지 몰라 가를 수 없음", range(loss), silent_note())
    } else if conceal.1 <= max {
        format!("복구 후 손실 {} < 조건 {cond}% — 손실 1% 회선이 아니라 가를 수 없음(참고값)", range(loss))
    } else {
        format!("복구 후 손실 {} > 조건 {cond}% — 조건보다 나쁜 회선이라 가를 수 없음", range(loss))
    };
    (Verdict::Unknown, Some(note))
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
                let measured = format!(
                    "은닉 {} ({}/{}프레임) · 복구 후 손실 {} · 연결 {} · 소리 없는 시간 {}",
                    pct_range(c.pct, c.pct_hi),
                    c.concealed,
                    c.produced.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                    pct_range(c.loss_pct, c.loss_pct_hi),
                    fmt_s(c.connected_s),
                    silent_text(c)
                );
                let v = if c.connected_s < goal::MIN_JUDGE_S {
                    (Unknown, Some(format!("연결 {} < {}s", fmt_s(c.connected_s), goal::MIN_JUDGE_S)))
                } else {
                    // 무수신 리셋 뒤 무음은 은닉에도 분모에도 들지 않는다 — 끊김이 잦은 회선이 은닉 %로는 좋아 보인다(#246 H1'). 그래서
                    // 무음을 은닉·손실로 넣은 상한과 뺀 하한의 구간으로 가른다(#246 H1'').
                    match (c.pct.zip(c.pct_hi), c.loss_pct.zip(c.loss_pct_hi), c.silent_s.zip(c.silent_hi_s)) {
                        _ if c.pct.is_none() => (Unknown, Some("재생 프레임 수(produced) 없음(#246 이전 형식) — 은닉 비율의 분모를 몰라 판정 불가".into())),
                        _ if c.loss_pct.is_none() => (Unknown, Some("lossShape 없음(P2-17h 이전) — 회선 손실을 몰라 조건 확인 불가".into())),
                        (Some(cp), Some(l), Some(sil)) => {
                            let (v, note) = conceal_verdict(cp, l, c.dup_dropped, sil);
                            // 재연결이 잦으면 구간마다 생기는 무음을 사람이 알아보도록 구간 수를 함께 적는다(#246 L3)
                            let runs = (c.runs > 1).then(|| format!("연결 구간 {}개(재연결·재입장 {}회)", c.runs, c.runs - 1));
                            let note = match (note, runs) {
                                (Some(n), Some(r)) => Some(format!("{n} · {r}")),
                                (n, r) => n.or(r),
                            };
                            (v, note)
                        }
                        _ => (Unknown, Some("상대 프레임 길이(lossShape·remoteFrameMs) 없음 — 재생이 멈춘 시간을 확인할 수 없어 판정 불가".into())),
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
    let (mut no_audio_entries, mut no_audio_short, mut pending) = (0, 0, 0);
    let mut first_audio_ms = Vec::new();
    let (mut entry_first_audio_ms, mut no_lead_entries) = (Vec::new(), 0);
    let mut nat_sessions: BTreeMap<String, usize> = BTreeMap::new();
    // 시도 하한용: (zip 순번, 세션 시간 범위, 그 세션 시도 수)
    let mut spans: Vec<(usize, (u64, u64), usize)> = Vec::new();
    // 3-1 콘형 쌍용: spans와 같은 순서로 (NAT 매핑, 피어 수, [직접, 직접 아님, 결과 모름])
    let mut cone_info: Vec<(Option<String>, usize, [usize; 3])> = Vec::new();
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
            let mut kinds = [0usize; 3];
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
                        // RESULTS 순서: lan, public → 직접 / relay, failed → 직접 아님 / aborted → 결과 모름
                        kinds[match r {
                            0 | 1 => 0,
                            2 | 3 => 1,
                            _ => 2,
                        }] += 1;
                    }
                    "recover" => recover += 1,
                    "switch" => switch += 1,
                    _ => {}
                }
            }
            with_relay += usize::from(relay_here);
            pending += s.pending;
            attempts_here += s.pending;
            kinds[2] += s.pending;
            *nat_sessions.entry(s.nat.clone().unwrap_or_else(|| "없음".into())).or_default() += 1;
            for p in &s.peers {
                first_audio_ms.extend(&p.first_audio_ms);
                entry_first_audio_ms.extend(&p.entry_first_audio_ms);
                no_lead_entries += p.no_lead_entries;
                no_audio_entries += p.no_audio_entries;
                no_audio_short += p.no_audio_short;
            }
            spans.push((fi, s.span_ms.unwrap_or((0, 0)), attempts_here));
            cone_info.push((s.nat.clone(), s.peers.len(), kinds));
        }
    }
    // 콘형 쌍(3-1): 시간이 겹치는 세션 무리마다, 참가자 모두의 zip이 있고(zip 수 = 가장 많이 본 피어 수 + 1) 모두 매핑 독립·NAT 없음이면
    // 그 무리의 시도는 콘형 쌍끼리의 시도다. 피어 가명은 zip마다 달라 짝을 맞출 수 없으므로 수로만 확인한다(같은 시간에 다른 방에서
    // 따로 합주한 기록을 함께 주면 틀릴 수 있다 — 한 합주의 zip만 함께 준다).
    let (mut cone_groups, mut cone) = (0, [0usize; 3]);
    for g in overlap_groups(&spans.iter().map(|x| x.1).collect::<Vec<_>>()) {
        let mut files: Vec<usize> = g.iter().map(|&i| spans[i].0).collect();
        files.sort_unstable();
        files.dedup();
        let max_peers = g.iter().map(|&i| cone_info[i].1).max().unwrap_or(0);
        let all_cone = g.iter().all(|&i| matches!(cone_info[i].0.as_deref(), Some("independent" | "open")));
        if files.len() >= 2 && max_peers + 1 == files.len() && all_cone {
            cone_groups += 1;
            for &i in &g {
                cone.iter_mut().zip(cone_info[i].2).for_each(|(a, b)| *a += b);
            }
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
    let attempts = direct + relay + failed + aborted + pending;
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
        pending,
        success_pct: pct(direct + relay),
        direct_pct: pct(direct),
        recover,
        switch,
        sessions_with_relay: with_relay,
        first_audio_ms,
        entry_first_audio_ms,
        no_lead_entries,
        nat_sessions,
        cone_groups,
        cone_attempts: cone.iter().sum(),
        cone_direct: cone[0],
        cone_nondirect: cone[1],
        cone_unresolved: cone[2],
        no_audio_entries,
        no_audio_short,
        rows,
    }
}

/// 시간 범위가 겹치는 것끼리 묶는다(시작 순으로 훑어, 다음 시작이 지금 무리의 끝 이하면 같은 무리). 돌려주는 값은 입력 순번.
fn overlap_groups(spans: &[(u64, u64)]) -> Vec<Vec<usize>> {
    let mut idx: Vec<usize> = (0..spans.len()).collect();
    idx.sort_by_key(|&i| spans[i].0);
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut end = 0u64;
    for i in idx {
        match out.last_mut() {
            Some(g) if spans[i].0 <= end => {
                g.push(i);
                end = end.max(spans[i].1);
            }
            _ => {
                out.push(vec![i]);
                end = spans[i].1;
            }
        }
    }
    out
}

/// 실제 시도 수의 하한: 시간이 겹치는 세션들을 한 무리로 묶고(같은 합주를 여러 사람이 내보냈을 수 있다), 무리마다 zip별 시도 합 중
/// 가장 큰 것만 센다. 같은 zip의 세션은 시간이 겹치지 않으므로 겹침 없는 세션은 그대로 더해진다.
fn attempts_lower_bound(spans: &[(usize, (u64, u64), usize)]) -> usize {
    overlap_groups(&spans.iter().map(|x| x.1).collect::<Vec<_>>())
        .into_iter()
        .map(|g| {
            let mut per_file: BTreeMap<usize, usize> = BTreeMap::new();
            for i in g {
                *per_file.entry(spans[i].0).or_default() += spans[i].2;
            }
            per_file.values().copied().max().unwrap_or(0)
        })
        .sum()
}

/// 3절 판정(모든 zip 합).
fn conn_goal(a: &ConnAgg) -> Vec<GoalRow> {
    use Verdict::*;
    let mut out = Vec::new();
    let pct = |v: Option<f64>| v.map(|v| format!("{v:.1}%")).unwrap_or_else(|| "없음".into());
    let t31 = format!("콘형 NAT 쌍 직접 {}%, 같은 공유기 LAN 우선", goal::DIRECT_PCT);
    // 콘형 쌍: 같은 시간대 세션 무리에서 참가자 모두의 zip이 있고 모두 STUN 매핑 독립(콘형)·NAT 없음일 때만(format 3, `aggregate_conn`).
    let nats = a.nat_sessions.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ");
    let cone_measured = format!(
        "콘형 쌍 무리 {} · 그 안 시도 {}: 직접 {} · 릴레이/실패 {} · 중단/미완 {} (전체 직접 {}/{} = {}) · 세션별 NAT {nats}",
        a.cone_groups,
        a.cone_attempts,
        a.cone_direct,
        a.cone_nondirect,
        a.cone_unresolved,
        a.direct,
        a.attempts,
        pct(a.direct_pct)
    );
    let cone_note = "콘형 = 두 STUN 서버가 같은 공인 주소를 봄(매핑 독립, RFC 4787). 필터링 종류는 모르지만 콘형끼리 홀펀칭에는 무관";
    if a.attempts == 0 {
        out.push(row("3-1", "직접 연결 성공률", t31, "없음".into(), Unknown, Some("conn 기록 없음(P1-18 이전이거나 연결 시도 없음)".into())));
    } else if a.cone_nondirect > 0 {
        out.push(row("3-1", "직접 연결 성공률", t31, cone_measured, NotMet, Some(format!("콘형 쌍인데 직접으로 못 붙은 시도가 있음 — 원인은 연결 기록 표에서. {cone_note}"))));
    } else if a.cone_attempts == 0 {
        let why = if a.nat_sessions.keys().all(|k| k == "없음") {
            "NAT 추정이 진단에 없음(format 3 이전) — 기록표의 회선 정보로 판단".to_string()
        } else {
            "콘형 쌍으로 확인된 세션 무리 없음 — 같은 합주의 참가자 모두의 zip이 있고 모두 independent·open이어야 한다".to_string()
        };
        out.push(row("3-1", "직접 연결 성공률", t31, cone_measured, Unknown, Some(why)));
    } else if a.cone_unresolved > 0 {
        out.push(row("3-1", "직접 연결 성공률", t31, cone_measured, Unknown, Some(format!("콘형 쌍 시도 중 결과 없는 것(중단·미완) {}회 — 가를 수 없음", a.cone_unresolved))));
    } else if a.cone_attempts < goal::MIN_CONE_ATTEMPTS {
        out.push(row("3-1", "직접 연결 성공률", t31, cone_measured, Unknown, Some(format!("콘형 쌍 시도 {}회 < {}회(양쪽 zip에서 하나씩)", a.cone_attempts, goal::MIN_CONE_ATTEMPTS))));
    } else {
        out.push(row("3-1", "직접 연결 성공률", t31, cone_measured, Met, Some(format!("{cone_note}. 같은 공유기 LAN 우선은 경로 표(lan)로 확인"))));
    }
    let t32 = format!("릴레이 폴백 포함 연결 성공률 ≥ {}%", goal::CONNECT_PCT);
    let measured = format!(
        "성공 {}/{} = {} (실패 {} · 중단 {} · 미완 {}) · 실제 시도 하한 {}",
        a.success,
        a.attempts,
        pct(a.success_pct),
        a.failed,
        a.aborted,
        a.pending,
        a.attempts_lower
    );
    let both_sides = "시도는 기록 기준 — 한 합주의 양쪽 zip이면 같은 연결이 두 번 세어진다(문턱은 하한으로)";
    // 미완(내보낼 때 연결 중)이 모두 성공했을 때의 성공률 — 이것도 98% 아래여야 미달이다.
    let best = (a.attempts > 0).then(|| (a.success + a.pending) as f64 * 100.0 / a.attempts as f64);
    let v32 = match a.success_pct {
        None => (Unknown, Some("conn 기록 없음".into())),
        Some(_) if best.is_some_and(|b| b < goal::CONNECT_PCT) => {
            (NotMet, Some(format!("중단(연결 중 퇴장·정지)·미완도 분모에 든다 — 원인은 연결 기록 표에서. {both_sides}")))
        }
        Some(p) if p < goal::CONNECT_PCT => (
            Unknown,
            Some(format!("내보낼 때 아직 연결 중인 시도 {}회(미완) — 결과에 따라 98% 위아래가 갈려 가를 수 없음", a.pending)),
        ),
        Some(_) if a.unknown_results > 0 => (Unknown, Some(format!("결과를 모르는 시도 {}회(새 형식) — 성공·실패를 추측하지 않음", a.unknown_results))),
        Some(_) if a.attempts_lower < goal::MIN_ATTEMPTS_98 => (
            Unknown,
            Some(format!("실제 시도 하한 {}회 < {}회 — 98%를 가르기에 적음. {both_sides}", a.attempts_lower, goal::MIN_ATTEMPTS_98)),
        ),
        Some(_) => (Met, Some(both_sides.into())),
    };
    out.push(row("3-2", "연결 성공률(폴백 포함)", t32, measured, v32.0, v32.1));

    // 3-3 첫 소리: GOAL 구간은 "입장 후"다. format 3부터 클라이언트가 입장 → 피어 추가(`entryLeadMs`)를 알려 줘 `entryLeadMs +
    // firstAudioMs`가 그 구간이다. 모든 항목에 그 값이 있고 모두 3초 이내일 때만 충족이다. 없는 항목이 있으면 `firstAudioMs`(피어 추가부터 —
    // 입장보다 짧다)로는 3초를 넘었거나 소리를 끝내 못 받았을 때만 미달이고 그 밖은 판정 불가다(#246 H2).
    let t33 = format!("입장 후 {}초 이내 소리", goal::FIRST_AUDIO_MS / 1000.0);
    let over = a.first_audio_ms.iter().filter(|&&x| x > goal::FIRST_AUDIO_MS).count();
    let over_entry = a.entry_first_audio_ms.iter().filter(|&&x| x > goal::FIRST_AUDIO_MS).count();
    let dist = Dist::of(&a.first_audio_ms);
    let entry_dist = Dist::of(&a.entry_first_audio_ms);
    let measured = format!(
        "입장 후 {}개{} · 3초 초과 {over_entry} · 입장 값 없음 {} | 피어 추가 후 {}개{} · 3초 초과 {over} | 소리 못 받음(3초+) {} · 짧게 머물고 소리 없음 {}",
        a.entry_first_audio_ms.len(),
        entry_dist.map(|d| format!(" · 중앙 {:.0}ms · 최대 {:.0}ms", d.median, d.max)).unwrap_or_default(),
        a.no_lead_entries,
        a.first_audio_ms.len(),
        dist.map(|d| format!(" · 중앙 {:.0}ms · 최대 {:.0}ms", d.median, d.max)).unwrap_or_default(),
        a.no_audio_entries,
        a.no_audio_short
    );
    let span = "firstAudioMs = 엔진에 피어가 추가된 때부터(입장 → 피어 추가 구간 미포함)";
    if over_entry > 0 {
        out.push(row("3-3", "첫 소리", t33, measured, NotMet, Some("입장 후(entryLeadMs + firstAudioMs) 3초 넘은 항목이 있음".into())));
    } else if over > 0 || a.no_audio_entries > 0 {
        out.push(row("3-3", "첫 소리", t33, measured, NotMet, Some(format!("{span} — 그 구간만으로도 넘음"))));
    } else if a.first_audio_ms.is_empty() {
        out.push(row("3-3", "첫 소리", t33, measured, Unknown, Some("firstAudioMs 기록 없음".into())));
    } else if a.no_lead_entries > 0 {
        out.push(row(
            "3-3",
            "첫 소리",
            t33,
            measured,
            Unknown,
            Some(format!("입장 → 피어 추가(entryLeadMs)가 없는 항목 {}개(옛 앱·형식) — {span}, 3초 이내인지는 기록표 체감값으로", a.no_lead_entries)),
        ));
    } else {
        let short = if a.no_audio_short > 0 { format!(" · 3초 전에 나가 소리 없는 항목 {}개는 가를 수 없어 뺐다", a.no_audio_short) } else { String::new() };
        out.push(row("3-3", "첫 소리", t33, measured, Met, Some(format!("모든 항목의 입장 후 첫 소리가 3초 이내{short}"))));
    }
    out
}

/// zip 하나 → 세션별 보고서.
pub(crate) fn analyze(d: &Diag) -> FileReport {
    let interval_ms = d.meta.as_ref().and_then(|m| m.get("timeline")).and_then(|t| u64_at(t, "intervalMs")).unwrap_or(1000);
    // engine.json 설정은 마지막 세션 것(`lastStats.session`)이다.
    let cfg = d.engine.as_ref().and_then(|e| e.get("config")).and_then(|c| serde_json::from_value::<EngineConfig>(c.clone()).ok()).map(|c| ConfigSummary::of(&c));
    let cfg_session = d.engine.as_ref().and_then(|e| e.get("lastStats")).and_then(|s| u64_at(s, "session"));
    // format 3부터 리버퍼 카운터 키가 없으면 0이다(0이면 줄에 넣지 않는다). 그 전 형식은 카운터가 아예 없다.
    let counters = d.meta.as_ref().and_then(|m| u64_at(m, "format")).is_some_and(|f| f >= 3);
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
            analyze_session(s, &groups[&s], c, interval_ms, counters)
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
