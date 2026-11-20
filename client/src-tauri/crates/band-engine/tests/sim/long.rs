//! 장시간 하네스(P1-6): 6피어 풀메시 60분을 가상 시간으로 돌려 GOAL 1·2절 수치를 자동 판정한다.
//!
//! 모델은 `tune::run_multi`(피어 6 → 지터 버퍼 → PeerFeed(워커) → OutputRt(콜백), 결정적)를 그대로 쓴다.
//! 이 엔진 하나가 원격 스트림 6개를 받는다(6인 풀메시에서 한 사람이 받는 것은 5개 — 1개 더 얹어 여유 있게 검증).
//! 엔진은 원격 스트림마다 독립된 지터 버퍼를 쓰므로 수신 쪽 하나가 풀메시 전체의 수신 경로를 대표한다. 스트림마다 다른 장치 드리프트(±200ppm), 무작위·버스트 손실(0.5~2%), 지터(분포·계단 변화), 와이파이 스파이크,
//! UDP 5초 단절로 흔든다. 측정만 하고 엔진 로직은 바꾸지 않는다.
//!
//! WS 재접속: `performSession.connectionLost`는 엔진을 유지하고(UDP 메시 계속), `welcome(resumed)`는 descriptor만 다시
//! 알린다. 즉 30초 이내 재접속은 엔진(지터·워커·콜백)에 아무 입력도 주지 않으므로 이 하네스의 대상이 아니다.
//! 세션 재개 실패(새 clientId)는 엔진 재시작이라 "끊김 없음" 범위 밖이다. UDP 단절 5초는 `net::peer`의 LOST_AFTER(5s)에
//! Reconnecting으로 바뀌지만 FAIL_AFTER(15s) 전이라 피어가 제거되지 않는다 → 지터 버퍼가 5초 공백을 겪는 것과 같다.
//!
//! 리포트: `SIM_REPORT_DIR`(없으면 OS 임시 폴더/band-tauri-sim)에 `sim-report-<태그>.md`와 `.json`.
//! - 짧은 버전(5분)은 기본 `cargo test`에서 돈다.
//! - 60분 전체: `cargo test -p band-engine --no-default-features --features test-support --test sim long_sim_6peers_60min -- --ignored --nocapture` (CI 야간 잡 `long-sim`).

use super::tune::{run_multi, JitDist, MultiOut, MultiSim, NetPeer, PeerOut, Sample, WakeModel};
use serde_json::{json, Value};
use std::fmt::Write as _;

const SEC: u64 = 1_000_000;
/// 정착 시간(s): 이 뒤부터 지표를 잰다.
const SETTLE_S: f64 = 10.0;
/// GOAL 1절: bufferMs가 목표 ±3ms 밖에 1분 이상 머무르지 않음.
const BAND_MS: f32 = 3.0;
const BAND_MAX_S: f64 = 60.0;
/// 연속 구간 히스테리시스: 띠 안으로 이만큼(s) 이하 잠깐 들어왔다 나가면 같은 구간으로 이어 센다
/// (경계를 오가는 피어가 연속 판정을 피해 가지 않도록, PR #52 리뷰 M2).
const BAND_GAP_S: f64 = 1.0;
/// GOAL 1절 보조 판정: 띠 밖 총 시간 비율 상한(%). 60분 중 6분 — 연속 판정만으로는 절반 넘게 띠 밖인 피어가 PASS가 된다.
const BAND_PCT_MAX: f64 = 10.0;
/// GOAL 2절: 손실 1%에서 은닉 ≤ 1.5%.
const CONCEAL_GOAL_PCT: f64 = 1.5;
/// 언더런 은닉 예산(%): GOAL 2절(손실 1% → 은닉 ≤ 1.5%)에서 손실 몫을 뺀 0.5%p. 손실 은닉은 손실을 따라가므로
/// 지터 버퍼가 줄일 수 있는 것은 언더런 몫이다(P1-6a의 측정 기준).
const UNDERRUN_MAX_PCT: f64 = 0.5;
/// GOAL 2절: UDP 5초 단절 후 자동 복구 — 복구 시간 ≤ 5초.
const RECOVER_MAX_S: f64 = 5.0;
/// 지연 누적: 같은 망 조건의 첫 창과 마지막 창 bufferMs 중앙값 차이 상한(ms).
const ACCUM_MAX_MS: f32 = 1.0;
/// 은닉 비율에서 빼는 구간: 단절 시작부터 끝 + 이만큼(s).
const OUTAGE_EXCL_S: f64 = 10.0;

// ── 꼬리 지터 회선의 회귀 방지 래칫(PR #248 리뷰 M1) ──
// **GOAL 기준이 아니다.** GOAL 1절은 꼬리 지터 회선을 대상에서 빼고 "지연이 조금 늘더라도 끊김 우선"이라고만 한다. 아래 한도는
// 지금 엔진의 늦은 패킷 예산(`tail_extra`)이 내는 동작을 관측한 분포에 여유를 두고 묶어, 예산이 커지거나 내려오지 않는 회귀를
// 잡는 값이다. `TailDelay`는 예산을 지연에서 빼고 `TailCap`의 한도는 코드 상한(1F)과 같아서, 그 둘만으로는 예산이 0~1F 안
// 어디에 머무는지 아무도 보지 않았다(예산이 1F에 붙어 마지막 창 bufferMs가 +2.7ms 올라도 통과).
// 관측 분포(2026-11-20, 꼬리 피어 #1 = F 5ms·지수 지터 2ms, 5분 시드 변형 40개 / 60분 시드 변형 10개):
//   기본 조건 구간 예산 중앙값          5분 1.50~2.50ms(0.30~0.50F), 시드 평균 2.01ms   60분 2.00ms(전 시드)
//   지터 계단이 풀린 뒤 0.65F 이하까지  5분 5~35s                                       60분 5~26s
//   bufferMs − 1절 한도(첫 / 마지막 창) 5분 −1.12~+0.69 / −0.88~+0.49ms                 60분 −0.25~+0.10 / −0.16~+0.17ms
// 변형 실험(엔진을 작업 트리에서만 바꿔 5분 시드 변형 0~4): 예산 내림을 끄면(1F에 고착 — 리뷰 M1의 변형) 수준 5.00ms·감쇠 없음·
// 마지막 창 한도 초과 +2.47~+2.82ms로 셋 다 5/5 실패, 예산 상한을 2F로 올리면 `TailCap`이 5/5 실패. 느린 내림만 끄거나 내림
// 문턱을 0.6 → 0.3으로 낮춘 약한 변형(수준 2.25~3.00ms, 지연 +0.3ms 안팎)은 실행 하나의 한도로는 통과하고 시드 평균
// (`TAIL_LEVEL_MEAN_MAX_F`, 5시드 평균 2.65·2.70ms)으로 잡는다.
// 엔진을 의도해서 바꿔 이 분포가 움직이면(예산 규칙 조정 등) 시드 스윕(`long_sim_seed_sweep_5min`·`_60min`)을 다시 돌려 한도와
// 이 주석·GOAL-COVERAGE를 함께 고친다.
/// 예산 수준: 기본 망 조건 구간(사건·지터 계단 구간 밖)의 예산 중앙값 상한(프레임 배수). 관측 최대 0.50F에 0.15F(5ms 프레임에서
/// 격자 3칸 0.75ms)를 더했다. 예산이 상한에 붙으면 1.0F다.
const TAIL_LEVEL_MAX_F: f32 = 0.65;
/// 예산 감쇠: 꼬리가 굵어졌다 돌아온 뒤(지터 계단 하강) 예산이 `TAIL_LEVEL_MAX_F` 이하로 내려오기까지의 시간 상한(s).
/// 관측 최대 35s. 내려오지 않으면(고착) 무한대다.
const TAIL_DECAY_MAX_S: f64 = 60.0;
/// 총 지연: 예산을 포함한 bufferMs 창 중앙값이 1절 한도(1F + p95 지터)를 넘는 폭의 상한(ms). 관측 최대 +0.69ms에 0.56ms를
/// 더했다. 1절 한도 기준으로 재는 것은 시드마다 달라지는 p95 지터를 빼기 위해서다(bufferMs 자체는 10.5~12.4ms로 더 흩어진다).
const TAIL_TOTAL_OVER_MS: f32 = 1.25;
/// 시드 평균 래칫(시드 스윕 전용): 시드 변형 `TAIL_MEAN_MIN_SEEDS`개 이상을 돌렸을 때 꼬리 지터 피어의 예산 수준 평균 상한
/// (프레임 배수). 실행 하나의 한도(`TAIL_LEVEL_MAX_F`)는 시드 흩어짐(±0.5ms)을 덮어야 해서 느슨하다 — 평균은 흩어짐이 작아
/// (5분 40시드를 5개씩 묶은 평균 1.90~2.10ms = 0.38~0.42F, 60분 2.00ms) 더 작은 상승을 잡는다. 0.48F = 2.40ms.
const TAIL_LEVEL_MEAN_MAX_F: f32 = 0.48;
const TAIL_MEAN_MIN_SEEDS: usize = 5;

/// 회선 분류 — GOAL.md 1절 **"안정 회선"의 뜻**(사용자 결정 2026-11-12, P1-6d)을 하네스 판정에 옮긴 한 곳이다.
///
/// | GOAL.md 문구 | 하네스 |
/// |---|---|
/// | "합주의 기본 환경은 유선(LAN) 연결" — 안정 회선 | [`LineClass::Stable`]: 망 지터 분포가 유계(`JitDist::Uniform`)인 회선. 1절 "지터 버퍼 추가 지연"(중앙값 ≤ 1프레임 + p95 지터)을 첫·마지막 창에 그대로 건다. |
/// | "지연이 가끔 크게 튀는 회선(와이파이·LTE 등 꼬리 긴 지터)은 안정 회선이 아니므로 … 기준의 대상이 아니다" | [`LineClass::Tail`]: 실행 중 한 번이라도 꼬리 긴 분포(`JitDist::Exp`·`JitDist::Pareto`)를 쓰는 회선. 1절 "지터 버퍼 추가 지연" 판정에서 뺀다. |
/// | "그런 회선에서는 지연이 조금 늘더라도 끊김(은닉) 기준(2절)을 우선한다" | `Tail`에도 2절(은닉·언더런)과 지연 누적(±3ms 띠)은 똑같이 걸고, "조금"을 수치로 묶는다: 늦은 패킷 예산(`tail_extra`)을 뺀 bufferMs ≤ 1프레임 + p95 지터(`TailDelay`·`TailDelayLast`), 예산 ≤ 1프레임(`TailCap`), 예산을 뺀 bufferMs의 첫·마지막 창 차이 ≤ `ACCUM_MAX_MS`. 셋을 합치면 bufferMs ≤ 1절 한도 + 1프레임이고 시간이 지나도 늘지 않는다. |
///
/// 위 셋은 예산을 지연에서 빼므로 예산 자체가 커지는 회귀를 보지 못한다. 그래서 꼬리 지터 회선에는 **회귀 방지 래칫**(GOAL 기준이
/// 아니다 — 지금 동작의 관측 분포 + 여유)을 더 건다: 예산 수준(`TailLevel`), 예산 감쇠(`TailDecay`), 예산을 포함한 총 bufferMs
/// (`TailTotal`·`TailTotalLast`). 한도와 근거는 `TAIL_LEVEL_MAX_F`·`TAIL_DECAY_MAX_S`·`TAIL_TOTAL_OVER_MS`에 있다.
///
/// 분류는 지터 **분포**만 본다. 와이파이 스파이크 회선(#4: 0.3초 +30ms)은 GOAL 문구로는 안정 회선이 아니지만, 스파이크 구간을
/// 사건으로 빼고(`event_x`) 그 사이의 유선형(균등) 구간에 1절을 그대로 건다 — 문구보다 엄격한 쪽이다(5분 40시드에서 여유 ≥ 0.58ms).
/// 손실 모양(버스트·2%)은 1절과 무관하다(`LossRule`이 2절 한도만 바꾼다).
#[derive(Clone, Copy, PartialEq, Debug)]
enum LineClass {
    Stable,
    Tail,
}

impl LineClass {
    fn of(np: &NetPeer) -> Self {
        // 분포가 늘면 여기서 컴파일이 멈춘다 — 새 분포가 유계인지 꼬리가 긴지 정하고 넣을 것.
        let heavy = |d: JitDist| match d {
            JitDist::Uniform => false,
            JitDist::Exp | JitDist::Pareto => true,
        };
        if heavy(np.dist) || np.dist_switch.is_some_and(|(_, d)| heavy(d)) || np.dist_steps.iter().any(|&(_, d)| heavy(d)) {
            LineClass::Tail
        } else {
            LineClass::Stable
        }
    }
    fn label(self) -> &'static str {
        match self {
            LineClass::Stable => "안정",
            LineClass::Tail => "꼬리 지터",
        }
    }
    /// JSON 값.
    fn key(self) -> &'static str {
        match self {
            LineClass::Stable => "stable",
            LineClass::Tail => "tail",
        }
    }
    /// 이 분류에 거는 판정 지표. 2절(은닉·언더런)과 띠 밖 비율은 공통이고, 1절 지터 버퍼 추가 지연은 안정 회선에만,
    /// 꼬리 지터 회선에는 그 자리에 상한 판정 셋과 회귀 방지 래칫 넷을 건다(빼기만 하고 아무것도 안 보는 피어가 없게).
    fn metrics(self) -> &'static [Metric] {
        match self {
            LineClass::Stable => &[Metric::Conceal, Metric::Underrun, Metric::BandPct, Metric::GoalDelay, Metric::GoalDelayLast],
            LineClass::Tail => &[
                Metric::Conceal,
                Metric::Underrun,
                Metric::BandPct,
                Metric::TailDelay,
                Metric::TailDelayLast,
                Metric::TailCap,
                Metric::TailLevel,
                Metric::TailDecay,
                Metric::TailTotal,
                Metric::TailTotalLast,
            ],
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LossRule {
    /// 무작위 손실 ≤ 1%: GOAL 2절 한도 1.5%.
    Goal,
    /// 버스트 손실·2%: 실제 손실률 + 0.5%p(GOAL 밖 회선, 은닉이 손실을 크게 넘지 않는지만 본다).
    ActualPlus,
}

/// 알려진 미달이 있을 수 있는 판정 지표.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Metric {
    /// 평시 은닉 비율(손실 + 언더런, %).
    Conceal,
    /// 평시 언더런 은닉 비율(%).
    Underrun,
    /// ±3ms 띠 밖 총 시간 비율(%).
    BandPct,
    /// GOAL 1절 지터 버퍼 추가 지연: 첫 창 bufferMs 중앙값(ms), 한도 1프레임 + 첫 창 p95 지터(P1-6c, PR #63 리뷰 M2).
    GoalDelay,
    /// 같은 판정을 마지막 창에(PR #65 리뷰 M3): `tail_extra`처럼 시간이 지나며 움직이는 상태가 세션 끝에 쌓인 지연을 잡는다.
    GoalDelayLast,
    /// 꼬리 지터 회선(1절 대상 밖)의 상한: 첫 창에서 늦은 패킷 예산(`tail_extra`)을 뺀 bufferMs 중앙값(ms), 한도는 1절과 같은
    /// 1프레임 + 첫 창 p95 지터. 1절을 넘는 지연이 끊김 우선으로 얹은 예산뿐인지 본다.
    TailDelay,
    /// 같은 판정을 마지막 창에.
    TailDelayLast,
    /// 꼬리 지터 회선의 늦은 패킷 예산 최댓값(ms, 정착 뒤 전 구간), 한도 1프레임.
    TailCap,
    /// 회귀 방지 래칫(GOAL 기준 아님): 기본 망 조건 구간의 늦은 패킷 예산 중앙값(ms), 한도 `TAIL_LEVEL_MAX_F` × 프레임.
    TailLevel,
    /// 회귀 방지 래칫: 지터 계단이 풀린 뒤 예산이 `TailLevel` 한도 이하로 내려오기까지의 시간(s), 한도 `TAIL_DECAY_MAX_S`.
    /// 그런 계단이 없는 회선은 0(잴 것이 없음).
    TailDecay,
    /// 회귀 방지 래칫: 예산을 포함한 첫 창 bufferMs 중앙값(ms), 한도 1절 한도 + `TAIL_TOTAL_OVER_MS`.
    TailTotal,
    /// 같은 판정을 마지막 창에.
    TailTotalLast,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::Conceal => "은닉",
            Metric::Underrun => "언더런 은닉",
            Metric::BandPct => "±3ms 밖 비율",
            Metric::GoalDelay => "1절 지터 버퍼 지연(첫 창 bufferMs)",
            Metric::GoalDelayLast => "1절 지터 버퍼 지연(마지막 창 bufferMs)",
            Metric::TailDelay => "꼬리 지터 회선 지연 상한(첫 창 bufferMs − 늦은 패킷 예산)",
            Metric::TailDelayLast => "꼬리 지터 회선 지연 상한(마지막 창 bufferMs − 늦은 패킷 예산)",
            Metric::TailCap => "꼬리 지터 회선 늦은 패킷 예산 최댓값",
            Metric::TailLevel => "꼬리 지터 회선 늦은 패킷 예산 수준(기본 조건 구간 중앙값, 회귀 래칫)",
            Metric::TailDecay => "꼬리 지터 회선 늦은 패킷 예산 감쇠 시간(지터 계단이 풀린 뒤, 회귀 래칫)",
            Metric::TailTotal => "꼬리 지터 회선 총 지연(첫 창 bufferMs, 회귀 래칫)",
            Metric::TailTotalLast => "꼬리 지터 회선 총 지연(마지막 창 bufferMs, 회귀 래칫)",
        }
    }
    /// (값 단위, 허용폭 단위).
    fn unit(self) -> (&'static str, &'static str) {
        match self {
            Metric::TailDecay => ("s", "s"),
            Metric::GoalDelay | Metric::GoalDelayLast | Metric::TailDelay | Metric::TailDelayLast | Metric::TailCap | Metric::TailLevel | Metric::TailTotal | Metric::TailTotalLast => ("ms", "ms"),
            _ => ("%", "%p"),
        }
    }
}

/// 알려진 GOAL 미달: 원인 분석을 마치고 별도 백로그로 넘긴 항목. 리포트에는 FAIL(알려진)로 남기고 테스트는 통과시키되
/// 기준값(실행 길이별 측정값) ± `tol` 안에 있어야 한다(래칫, PR #52 리뷰 M3).
/// - 기준값 + tol 초과: 악화 → 실패.
/// - 기준값 − tol 미만: 개선 → 실패("기준값을 새 측정값으로 낮춰라"). 상한이 오래 남아 회귀를 숨기지 않게 한다.
/// - 한도 안으로 들어옴: 해소 → 실패("known 지정을 지워라").
///
/// 시뮬레이션은 결정적이라 같은 코드면 같은 값이 나온다. tol은 엔진 변경을 감지하는 문턱이다.
#[derive(Clone, Copy)]
struct Known {
    metric: Metric,
    /// 5분 버전 기준값(None = 이 길이에서는 미달 아님, 한도 안이어야 한다).
    short: Option<f64>,
    /// 60분 버전 기준값.
    long: Option<f64>,
    tol: f64,
    why: &'static str,
}

impl Known {
    fn base(&self, seconds: f64) -> Option<f64> {
        if seconds <= 300.0 {
            self.short
        } else {
            self.long
        }
    }
}

// 기준값은 측정값으로 넣고, 고치는 PR에서 갱신하거나 지운다. 지금 알려진 미달은 없다(모든 `Spec.known`이 비어 있다).
// - P1-6a(손실 뒤 언더런 은닉 재해석): #2·#5의 은닉·언더런·띠 밖 미달과 #1 5분 띠 밖 미달 해소.
// - P1-6b(늦은 패킷 예산 `tail_extra`): #1 꼬리 긴 지터의 은닉 미달(1.94% / 1.93%) 해소.
// - P1-6d·P2-16d(회선 분류 `LineClass`): #1 마지막 창 1절 지연(`KNOWN_1`, 5분 12.17ms > 11.68ms) — 꼬리 지터 회선은 1절 대상이
//   아니므로 미달이 아니다. 그 값은 시드마다 한도 양쪽으로 갈렸다(5분 40시드 마지막 창 여유 −0.49~+0.88ms, 11개 초과 / 첫 창
//   −0.69~+1.12ms, 7개 초과 / 60분 7시드 첫 창 4개 초과) — 고정 시드에서만 래칫 안이었다.
// 새 미달을 known으로 넘길 때 tol은 0.3%p를 쓴다(래칫 문턱). ms 지표는 0.1ms를 쓴다.

struct Spec {
    np: NetPeer,
    rule: LossRule,
    known: &'static [Known],
}

/// 6피어 시나리오. 사건은 길이 비율로 놓아 5분 버전과 60분 버전이 같은 모양이 된다.
fn scenario(seconds: f64) -> Vec<Spec> {
    let at = |f: f64| (seconds * f * 1e6) as u64;
    let mut v = Vec::new();
    // #0 기준 회선: 손실 1%(무작위), 지터 1ms, +170ppm.
    let mut np = NetPeer::new(240, 1001);
    np.drift_ppm = 170.0;
    np.net_jitter_us = 1000;
    np.loss = 0.01;
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #1 RTT 계단(꼬리 지터 회선 — `LineClass::Tail`, 1절 지터 버퍼 추가 지연 대상 밖): 지수 지터 2ms·손실 0.5%·−200ppm, 30%~60% 구간에 +8ms·지터 4ms.
    let mut np = NetPeer::new(240, 1002);
    np.in_buf = 96;
    np.drift_ppm = -200.0;
    np.net_jitter_us = 2000;
    np.dist = JitDist::Exp;
    np.loss = 0.005;
    np.steps = vec![(at(0.30), 8000, 4000), (at(0.60), 0, 2000)];
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #2 버스트 손실: 평균 3패킷 버스트(나쁨 진입 0.4%) + 무작위 0.3% ≈ 1.5%, +200ppm, 장치 버퍼 256.
    let mut np = NetPeer::new(240, 1003);
    np.in_buf = 256;
    np.drift_ppm = 200.0;
    np.net_jitter_us = 3000;
    np.loss = 0.003;
    np.burst = Some((0.004, 1.0 / 3.0));
    v.push(Spec { np, rule: LossRule::ActualPlus, known: &[] });
    // #3 UDP 5초 단절 두 번(50%, 85%), 2.5ms 프레임, 손실 1%, 지터 5ms, −120ppm.
    let mut np = NetPeer::new(120, 1004);
    np.drift_ppm = -120.0;
    np.net_jitter_us = 5000;
    np.loss = 0.01;
    np.outages = vec![(at(0.50), 5 * SEC), (at(0.85), 5 * SEC)];
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #4 와이파이 스파이크(0.3초 +30ms)가 길이의 1/8마다, 손실 0.5%, +60ppm, 장치 버퍼 144.
    let mut np = NetPeer::new(240, 1005);
    np.in_buf = 144;
    np.drift_ppm = 60.0;
    np.net_jitter_us = 1000;
    np.loss = 0.005;
    np.spikes = (1..8).map(|k| (at(k as f64 / 8.0 + 0.01), 300_000, 30_000)).collect();
    v.push(Spec { np, rule: LossRule::Goal, known: &[] });
    // #5 나쁜 회선: 손실 2%(무작위 1.5% + 버스트), 25%에 5초 단절, 70~80%에 지터 8ms, −160ppm, 장치 버퍼 480(10ms 몰아 보냄).
    let mut np = NetPeer::new(240, 1006);
    np.in_buf = 480;
    np.drift_ppm = -160.0;
    np.net_jitter_us = 2000;
    np.loss = 0.015;
    np.burst = Some((0.0025, 0.5));
    np.outages = vec![(at(0.25), 5 * SEC)];
    np.steps = vec![(at(0.70), 0, 8000), (at(0.80), 0, 2000)];
    v.push(Spec { np, rule: LossRule::ActualPlus, known: &[] });
    v
}

fn describe(np: &NetPeer) -> String {
    let mut s = format!(
        "F {:.1}ms, 장치 {}샘플, 드리프트 {:+}ppm, 지터 {:?} {:.1}ms, 손실 {:.1}%",
        np.frame as f64 / 48.0,
        np.in_buf,
        np.drift_ppm,
        np.dist,
        np.net_jitter_us as f64 / 1000.0,
        np.loss * 100.0
    );
    if let Some((g, b)) = np.burst {
        let _ = write!(s, " + 버스트(진입 {:.2}%, 평균 {:.1}패킷)", g * 100.0, 1.0 / b);
    }
    for (t, len) in &np.outages {
        let _ = write!(s, ", 단절 {:.0}s@{:.0}s", *len as f64 / 1e6, *t as f64 / 1e6);
    }
    for (t, extra, j) in &np.steps {
        let _ = write!(s, ", 계단@{:.0}s(+{:.0}ms, 지터 {:.1}ms)", *t as f64 / 1e6, *extra as f64 / 1000.0, *j as f64 / 1000.0);
    }
    if !np.spikes.is_empty() {
        let _ = write!(s, ", 스파이크 {}회(0.3s +30ms)", np.spikes.len());
    }
    s
}

/// 예산 수준 한도(ms): `TAIL_LEVEL_MAX_F` × 프레임.
fn tail_level_limit(frame_ms: f32) -> f32 {
    TAIL_LEVEL_MAX_F * frame_ms
}

/// 마지막 지터 계단 하강 시각(s): 지터가 직전 조건보다 작아지는 마지막 `steps` 항목 — 꼬리가 굵어졌다(늦은 패킷 예산이 오름)
/// 돌아오는 시점이다. 예산 감쇠(`Metric::TailDecay`)를 여기서부터 잰다.
fn jitter_drop_at(np: &NetPeer) -> Option<f64> {
    let mut prev = np.net_jitter_us;
    let mut at = None;
    for &(t, _, j) in &np.steps {
        if j < prev {
            at = Some(t as f64 / 1e6);
        }
        prev = j;
    }
    at
}

fn quantile(mut v: Vec<f32>, q: f64) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = ((v.len() as f64 * q).ceil() as usize).clamp(1, v.len()) - 1;
    v[k]
}

struct PeerResult {
    desc: String,
    class: LineClass,
    frame_ms: f32,
    buf_med: f32,
    buf_p95: f32,
    tgt_med: f32,
    /// 첫·마지막 창 p95 지터(ms): 1초 블록 상대 지연 p95의 중앙값, 사건 구간에 걸친 블록 제외.
    p95_jit: f32,
    p95_last: f32,
    /// 첫 창 목표 구성 중앙값(ms): 지터 추정, 소비자 여유(lead), 삽입 문턱 + 여유(목표 − 나머지 셋), 늦은 패킷 예산 여유.
    tgt_first: f32,
    jit_first: f32,
    lead_first: f32,
    tail_first: f32,
    /// 마지막 창 늦은 패킷 예산 중앙값(ms)과 정착 뒤 전 구간 최댓값(ms).
    tail_last: f32,
    tail_max: f32,
    /// 첫·마지막 창에서 늦은 패킷 예산을 뺀 bufferMs 중앙값(ms, 표본마다 `buf − tail`). 예산이 0인 회선에서는 bufferMs와 같다.
    base_first: f32,
    base_last: f32,
    /// 기본 망 조건 구간(사건 구간과 지터·지연 계단이 걸린 구간 밖)의 늦은 패킷 예산 중앙값(ms).
    tail_base: f32,
    /// 마지막 지터 계단 하강(꼬리가 굵어졌다 돌아옴) 뒤 예산이 `TailLevel` 한도 이하로 내려오기까지의 시간(s). 그런 계단이
    /// 없으면 `None`, 끝까지 내려오지 않으면 무한대.
    tail_decay_s: Option<f64>,
    /// `tail_extra` 조정 횟수, 초기화 횟수(정착 후, PR #65 리뷰 L3).
    tail_adjusts: u64,
    tail_resets: u64,
    /// ±3ms 밖(재생 안 함 포함) 최장 연속 시간(히스테리시스 BAND_GAP_S)과 그 시작(s), 총 시간(s), 총 비율(%).
    band_max_s: f64,
    band_max_at: f64,
    band_total_s: f64,
    band_pct: f64,
    /// 평시(단절·스파이크 구간과 계단 직후 10초 제외) 은닉 비율 — 판정 기준. 손실 몫 + 언더런 몫.
    conceal_pct: f64,
    conceal_loss_pct: f64,
    conceal_underrun_pct: f64,
    /// 단절 구간만 뺀 전체 은닉 비율(계단·스파이크 포함, 참고).
    conceal_all: f64,
    conceal_limit: f64,
    loss_pct: f64,
    rule: LossRule,
    known: &'static [Known],
    /// 단절별 (끝 시각 s, 복구 시간 s).
    recoveries: Vec<(f64, Option<f64>)>,
    buf_first: f32,
    buf_last: f32,
    off_first: f32,
    off_last: f32,
    rebuffers: u64,
    late_dropped: u64,
    corrections: u64,
    max_corr_cents: f32,
}

impl PeerResult {
    fn band_ok(&self) -> bool {
        self.band_max_s < BAND_MAX_S
    }
    fn recover_ok(&self) -> bool {
        self.recoveries.iter().all(|r| r.1.is_some_and(|s| s <= RECOVER_MAX_S))
    }
    /// 지연 누적(ms): 같은 망 조건의 첫 창 → 마지막 창 변화. 꼬리 지터 회선은 늦은 패킷 예산을 뺀 값으로 본다 — 예산은 늦은
    /// 도착 비율을 따라 0~1프레임 사이를 0.25~1ms 걸음으로 오르내리는 값이라(쌓이는 지연이 아니다) 두 창의 차이가 걸음 크기만큼
    /// 난다(#1 5분 40시드 bufferMs 차이 최대 1.38ms). 예산 자체는 `TailCap`(≤ 1프레임)이 묶는다.
    fn accum(&self) -> f32 {
        match self.class {
            LineClass::Stable => self.buf_last - self.buf_first,
            LineClass::Tail => self.base_last - self.base_first,
        }
    }
    fn accum_ok(&self) -> bool {
        self.accum().abs() <= ACCUM_MAX_MS
    }
    /// GOAL 1절 한도(ms): 1프레임 + 첫 창 p95 지터.
    fn goal_delay_limit(&self) -> f32 {
        self.frame_ms + self.p95_jit
    }
    /// 마지막 창 한도(ms): 1프레임 + 마지막 창 p95 지터.
    fn goal_delay_limit_last(&self) -> f32 {
        self.frame_ms + self.p95_last
    }
    fn value(&self, m: Metric) -> f64 {
        match m {
            Metric::Conceal => self.conceal_pct,
            Metric::Underrun => self.conceal_underrun_pct,
            Metric::BandPct => self.band_pct,
            Metric::GoalDelay => self.buf_first as f64,
            Metric::GoalDelayLast => self.buf_last as f64,
            Metric::TailDelay => self.base_first as f64,
            Metric::TailDelayLast => self.base_last as f64,
            Metric::TailCap => self.tail_max as f64,
            Metric::TailLevel => self.tail_base as f64,
            Metric::TailDecay => self.tail_decay_s.unwrap_or(0.0),
            Metric::TailTotal => self.buf_first as f64,
            Metric::TailTotalLast => self.buf_last as f64,
        }
    }
    fn limit(&self, m: Metric) -> f64 {
        match m {
            Metric::Conceal => self.conceal_limit,
            Metric::Underrun => UNDERRUN_MAX_PCT,
            Metric::BandPct => BAND_PCT_MAX,
            Metric::GoalDelay => self.goal_delay_limit() as f64,
            Metric::GoalDelayLast => self.goal_delay_limit_last() as f64,
            Metric::TailDelay => self.goal_delay_limit() as f64,
            Metric::TailDelayLast => self.goal_delay_limit_last() as f64,
            // 예산은 0.25ms 격자 값이라 f32 → f64 반올림 잡음만 허용한다.
            Metric::TailCap => self.frame_ms as f64 + 1e-3,
            // 아래 넷은 회귀 방지 래칫이다(GOAL 기준 아님, 근거는 `TAIL_LEVEL_MAX_F` 위 주석).
            Metric::TailLevel => tail_level_limit(self.frame_ms) as f64 + 1e-3,
            Metric::TailDecay => TAIL_DECAY_MAX_S,
            Metric::TailTotal => (self.goal_delay_limit() + TAIL_TOTAL_OVER_MS) as f64,
            Metric::TailTotalLast => (self.goal_delay_limit_last() + TAIL_TOTAL_OVER_MS) as f64,
        }
    }
    fn ok(&self, m: Metric) -> bool {
        self.value(m) <= self.limit(m)
    }
    /// 이 피어의 분류에 거는 지표인가.
    fn judged(&self, m: Metric) -> bool {
        self.class.metrics().contains(&m)
    }
    /// 판정 대상이 아니면 통과로 센다(표의 "전 피어" 집계용).
    fn ok_if_judged(&self, m: Metric) -> bool {
        !self.judged(m) || self.ok(m)
    }
}

/// 지표 하나의 판정.
#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Pass,
    /// 알려진 미달, 기준값 ± tol 안.
    Known,
    Fail,
}

fn analyze(np: &NetPeer, rule: LossRule, known: &'static [Known], p: &PeerOut, seconds: f64) -> PeerResult {
    let steady: Vec<&Sample> = p.samples.iter().filter(|s| s.t >= SETTLE_S).collect();
    let playing_buf: Vec<f32> = steady.iter().filter(|s| s.playing).map(|s| s.buf).collect();
    let buf_med = quantile(playing_buf.clone(), 0.5);
    let buf_p95 = quantile(playing_buf, 0.95);
    let tgt_med = quantile(steady.iter().filter(|s| s.playing).map(|s| s.tgt).collect(), 0.5);

    // ±3ms 밖 연속 구간(재생 멈춤 = 밖). 띠 안 복귀가 BAND_GAP_S 이하면 같은 구간으로 이어 센다.
    let (mut run_start, mut last_out) = (None::<f64>, f64::NEG_INFINITY);
    let (mut band_max_s, mut band_max_at, mut band_total_s) = (0.0f64, 0.0f64, 0.0f64);
    for s in &steady {
        let out = !s.playing || (s.buf - s.tgt).abs() > BAND_MS;
        if out {
            band_total_s += 0.1;
            // 표본 간격 0.1s: 사이에 낀 띠 안 표본이 BAND_GAP_S 이하(≤ 10개)면 이어 센다.
            if run_start.is_none() || s.t - last_out > BAND_GAP_S + 0.1 + 1e-6 {
                run_start = Some(s.t);
            }
            last_out = s.t;
            let st = run_start.unwrap_or(s.t);
            if s.t - st + 0.1 > band_max_s {
                band_max_s = s.t - st + 0.1;
                band_max_at = st;
            }
        }
    }
    let band_pct = band_total_s * 100.0 / (steady.len() as f64 * 0.1).max(0.1);

    // 사건 구간: 단절(시작 ~ 끝 + 10초), 계단(시작 후 10초), 스파이크(시작 ~ 끝 + 10초).
    let outage_x: Vec<(f64, f64)> = np.outages.iter().map(|&(s, l)| (s as f64 / 1e6, (s + l) as f64 / 1e6 + OUTAGE_EXCL_S)).collect();
    let mut event_x = outage_x.clone();
    event_x.extend(np.steps.iter().map(|&(s, _, _)| (s as f64 / 1e6, s as f64 / 1e6 + OUTAGE_EXCL_S)));
    event_x.extend(np.spikes.iter().map(|&(s, l, _)| (s as f64 / 1e6, (s + l) as f64 / 1e6 + OUTAGE_EXCL_S)));
    // 은닉 비율(정착 후, 구간 제외): (전체, 손실 몫, 언더런 몫) %.
    let conceal_excl = |x: &[(f64, f64)]| {
        let (mut conc, mut conc_loss, mut prod) = (0u64, 0u64, 0u64);
        for w in steady.windows(2) {
            let (a, b) = (w[0], w[1]);
            if x.iter().any(|&(x0, x1)| b.t > x0 && a.t < x1) {
                continue;
            }
            conc += b.concealed - a.concealed;
            conc_loss += b.concealed_loss - a.concealed_loss;
            prod += b.produced - a.produced;
        }
        let pct = |n: u64| n as f64 * 100.0 / prod.max(1) as f64;
        // 손실 몫이 전체를 넘을 수 있다(PR #248 리뷰 Low — 고치지 않고 크기를 묶어 둔다). 엔진은 마감에 뒤 패킷도 없으면 언더런
        // 은닉으로 세고(`concealed`만 +1), 나중에 그 seq를 손실로 넘길 때 손실 몫으로 다시 분류한다(`concealed_loss`만 +1, 늦게).
        // 두 증가 사이에 제외 구간의 경계가 끼면 한 표본 쌍은 빠지고 다른 쌍은 남아 한쪽 카운터만 센다:
        //   - 은닉은 제외 구간 안, 재분류는 밖 → 손실 몫만 +1(전체보다 커질 수 있다 — 5분 시드 변형 10·33에서 1프레임).
        //   - 은닉은 밖, 재분류는 안 → 전체만 +1(언더런 몫이 1프레임 크게 남는다).
        // 어긋남은 제외 구간 경계 하나에 재분류 대기 중인 프레임 수(보통 0~1)까지다. 5분 실행은 피어당 생산 약 6만 프레임·경계
        // 14개 이하라 최악에도 0.03%p 미만이고(언더런 한도 0.5%, 관측 0.00~0.10%), 은닉 전체(`conc`)는 영향이 없다. 표본 쌍을
        // 재분류 시점에 맞춰 옮기려면 엔진이 프레임별 분류 시각을 내보내야 해서(엔진 변경) 하네스에서는 뺄셈만 0으로 자른다.
        (pct(conc), pct(conc_loss), pct(conc.saturating_sub(conc_loss)))
    };
    let conceal_all = conceal_excl(&outage_x).0;
    let (conceal_pct, conceal_loss_pct, conceal_underrun_pct) = conceal_excl(&event_x);
    let net_sent = (p.tx_sent - p.tx_outage).max(1);
    let loss_pct = p.tx_lost as f64 * 100.0 / net_sent as f64;
    let conceal_limit = match rule {
        LossRule::Goal => CONCEAL_GOAL_PCT,
        LossRule::ActualPlus => loss_pct + 0.5,
    };

    // 복구: 단절 끝 이후 처음으로 재생 중 + ±3ms 안이 1초 이어지는 시각.
    let recoveries = np
        .outages
        .iter()
        .map(|&(s, l)| {
            let e = (s + l) as f64 / 1e6;
            let ok = |x: &Sample| x.playing && (x.buf - x.tgt).abs() <= BAND_MS;
            let after: Vec<&Sample> = p.samples.iter().filter(|x| x.t >= e && x.t < e + 30.0).collect();
            let rec = after.iter().enumerate().find(|(k, x)| ok(x) && after[*k..].iter().take_while(|y| y.t <= x.t + 1.0).all(|y| ok(y))).map(|(_, x)| x.t - e);
            (e, rec)
        })
        .collect();

    // 지연 누적: 처음·마지막 창의 bufferMs 중앙값 비교. 창 안의 사건 구간(event_x)은 빼서 같은 망 조건만 비교한다
    // (5분 버전은 마지막 창에 #3 단절·#4 스파이크가 들어온다, PR #52 리뷰 M4).
    let win = accum_win(seconds);
    let quiet = |s: &Sample| !event_x.iter().any(|&(x0, x1)| s.t >= x0 && s.t < x1);
    let in_win = |a: f64, b: f64, f: &dyn Fn(&Sample) -> f32| quantile(steady.iter().filter(|s| s.playing && s.t >= a && s.t < b && quiet(s)).map(|s| f(s)).collect(), 0.5);
    let buf_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.buf);
    let buf_last = in_win(seconds - win, seconds + 1.0, &|s| s.buf);
    let off_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.buf - s.tgt);
    let off_last = in_win(seconds - win, seconds + 1.0, &|s| s.buf - s.tgt);
    // p95 지터: 1초 블록 [b, b+1)이 사건 구간에 걸치면 뺀다(bufferMs 쪽 `quiet`와 같은 구간, PR #65 리뷰 M3).
    let quiet_blk = |b: f64| !event_x.iter().any(|&(x0, x1)| b + 1.0 > x0 && b < x1);
    let p95_in = |a: f64, b: f64| quantile(p.block_p95.iter().filter(|x| x.0 >= a && x.0 < b && quiet_blk(x.0)).map(|x| x.1).collect(), 0.5);
    let p95_jit = p95_in(SETTLE_S, SETTLE_S + win);
    let p95_last = p95_in(seconds - win, seconds + 1.0);
    let tgt_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.tgt);
    let jit_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.jit);
    let lead_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.lead);
    let tail_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.tail);
    let tail_last = in_win(seconds - win, seconds + 1.0, &|s| s.tail);
    let tail_max = steady.iter().map(|s| s.tail).fold(0.0f32, f32::max);
    let base_first = in_win(SETTLE_S, SETTLE_S + win, &|s| s.buf - s.tail);
    let base_last = in_win(seconds - win, seconds + 1.0, &|s| s.buf - s.tail);
    // 예산 수준: 기본 망 조건(시작 때의 지연·지터)인 구간 전체의 예산 중앙값. 계단이 걸린 구간(조건이 다름)과 사건 구간(계단
    // 직후 10초 포함)은 뺀다 — 계단이 풀린 뒤 예산이 내려오는 구간은 들어가므로 느린 감쇠도 값을 올린다.
    let base_cond = |t: f64| np.steps.iter().rev().find(|s| t >= s.0 as f64 / 1e6).is_none_or(|&(_, extra, j)| extra == 0 && j == np.net_jitter_us);
    let tail_base = quantile(steady.iter().filter(|s| s.playing && quiet(s) && base_cond(s.t)).map(|s| s.tail).collect(), 0.5);
    // 예산 감쇠: 지터 계단이 풀린 시각부터 예산이 수준 한도 이하인 첫 표본까지(0.25ms 격자 값이라 반올림 잡음만 허용).
    let level = tail_level_limit(np.frame as f32 / 48.0) + 1e-3;
    let tail_decay_s = jitter_drop_at(np).map(|t0| steady.iter().find(|s| s.t >= t0 && s.tail <= level).map_or(f64::INFINITY, |s| s.t - t0));

    PeerResult {
        desc: describe(np),
        class: LineClass::of(np),
        frame_ms: np.frame as f32 / 48.0,
        buf_med,
        buf_p95,
        tgt_med,
        p95_jit,
        p95_last,
        tgt_first,
        jit_first,
        lead_first,
        tail_first,
        tail_last,
        tail_max,
        base_first,
        base_last,
        tail_base,
        tail_decay_s,
        tail_adjusts: p.js.tail_adjusts - p.js_settle.tail_adjusts,
        tail_resets: p.js.tail_resets - p.js_settle.tail_resets,
        band_max_s,
        band_max_at,
        band_total_s,
        band_pct,
        conceal_pct,
        conceal_loss_pct,
        conceal_underrun_pct,
        conceal_all,
        conceal_limit,
        loss_pct,
        rule,
        known,
        recoveries,
        buf_first,
        buf_last,
        off_first,
        off_last,
        rebuffers: p.js.rebuffers,
        late_dropped: p.js.late_dropped,
        corrections: p.corrections(),
        max_corr_cents: p.js.max_corr_cents,
    }
}

/// 지연 누적 비교 창 길이(s).
fn accum_win(seconds: f64) -> f64 {
    ((seconds - SETTLE_S) / 6.0).clamp(30.0, 300.0)
}

fn pf(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

struct Report {
    md: String,
    json: Value,
    fails: Vec<String>,
}

/// 래칫 판정: 값 v, 한도 lim, 알려진 미달 기준값 base(± tol).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ratchet {
    Pass,
    /// 알려진 미달, 기준값 ± tol 안(테스트 통과).
    Known,
    /// 새 미달.
    New,
    /// 기준값 + tol 초과.
    Worse,
    /// 기준값 − tol 미만(기준값을 낮춰라).
    Improved,
    /// 한도 안으로 들어옴(지정을 지워라).
    Resolved,
}

fn ratchet(v: f64, lim: f64, base: Option<f64>, tol: f64) -> Ratchet {
    match (v <= lim, base) {
        (true, None) => Ratchet::Pass,
        (true, Some(_)) => Ratchet::Resolved,
        (false, None) => Ratchet::New,
        (false, Some(b)) if v > b + tol => Ratchet::Worse,
        (false, Some(b)) if v < b - tol => Ratchet::Improved,
        (false, Some(_)) => Ratchet::Known,
    }
}

/// 알려진 미달이 있을 수 있는 지표 판정. fails/known/resolved에 문구를 쌓는다.
fn judge_metric(i: usize, r: &PeerResult, m: Metric, seconds: f64, fails: &mut Vec<String>, known: &mut Vec<String>, resolved: &mut Vec<String>) -> Verdict {
    let (v, lim) = (r.value(m), r.limit(m));
    let k = r.known.iter().find(|k| k.metric == m && k.base(seconds).is_some());
    let base = k.and_then(|k| k.base(seconds));
    let tol = k.map_or(0.0, |k| k.tol);
    let name = m.name();
    let (u, tu) = m.unit();
    match ratchet(v, lim, base, tol) {
        Ratchet::Pass => Verdict::Pass,
        Ratchet::Known => {
            known.push(format!("#{i} {name} {v:.2}{u} > {lim:.2}{u} — {} (기준값 {:.2}{u} ± {tol:.1}{tu})", k.map_or("", |k| k.why), base.unwrap_or(f64::NAN)));
            Verdict::Known
        }
        Ratchet::New => {
            // 감쇠 시간은 끝까지 내려오지 않으면 무한대다.
            let vs = if v.is_finite() { format!("{v:.2}{u}") } else { "끝까지 내려오지 않음".to_string() };
            fails.push(format!("#{i} {name} {vs} > {lim:.2}{u}"));
            Verdict::Fail
        }
        Ratchet::Worse => {
            fails.push(format!("#{i} {name} {v:.2}{u} — 알려진 미달이 기준값 {:.2}{u} + {tol:.1}{tu}를 넘어 악화", base.unwrap_or(f64::NAN)));
            Verdict::Fail
        }
        Ratchet::Improved | Ratchet::Resolved => {
            let msg = if v <= lim {
                format!("#{i} {name} {v:.2}{u} ≤ {lim:.2}{u} — 알려진 미달 해소, 시나리오의 known 지정({m:?})을 지워라")
            } else {
                format!("#{i} {name} {v:.2}{u} — 기준값 {:.2}{u}보다 {tol:.1}{tu} 넘게 개선, known 기준값({m:?})을 새 측정값으로 낮춰라(래칫)", base.unwrap_or(f64::NAN))
            };
            resolved.push(msg.clone());
            fails.push(msg);
            Verdict::Fail
        }
    }
}

fn build_report(tag: &str, seconds: f64, specs: &[Spec], out: &MultiOut, res: &[PeerResult], wall_s: f64) -> Report {
    // fails: 테스트 실패(새 미달·악화·래칫). known: 알려진 미달(기준값 안). resolved: 알려진 미달이 풀리거나 크게 개선(실패에도 포함).
    let (mut fails, mut known, mut resolved) = (Vec::new(), Vec::new(), Vec::new());
    let mut peer_verdict = Vec::new();
    for (i, r) in res.iter().enumerate() {
        let mut vs: Vec<Verdict> = r.class.metrics().iter().map(|&m| judge_metric(i, r, m, seconds, &mut fails, &mut known, &mut resolved)).collect();
        if !r.band_ok() {
            fails.push(format!("#{i} ±{BAND_MS}ms 밖 {:.1}s 연속(@{:.0}s)", r.band_max_s, r.band_max_at));
            vs.push(Verdict::Fail);
        }
        if !r.recover_ok() {
            fails.push(format!("#{i} 단절 복구 {:?}", r.recoveries));
            vs.push(Verdict::Fail);
        }
        if !r.accum_ok() {
            fails.push(format!("#{i} 지연 누적 {:+.2}ms", r.accum()));
            vs.push(Verdict::Fail);
        }
        peer_verdict.push(if vs.contains(&Verdict::Fail) {
            "FAIL"
        } else if vs.contains(&Verdict::Known) {
            "FAIL(알려진)"
        } else {
            "PASS"
        });
    }
    if out.xruns > 0 {
        fails.push(format!("xrun {}회", out.xruns));
    }
    let worst = |f: &dyn Fn(&PeerResult) -> f64| res.iter().map(f).fold(f64::NEG_INFINITY, f64::max);
    let band_worst = worst(&|r| r.band_max_s);
    let band_pct_worst = worst(&|r| r.band_pct);
    let under_worst = worst(&|r| r.conceal_underrun_pct);
    let goal_conc: Vec<&PeerResult> = res.iter().filter(|r| r.rule == LossRule::Goal).collect();
    let conc_worst = goal_conc.iter().map(|r| r.conceal_pct).fold(0.0, f64::max);
    let other_ok = res.iter().filter(|r| r.rule == LossRule::ActualPlus).all(|r| r.ok(Metric::Conceal));
    let rec_all: Vec<Option<f64>> = res.iter().flat_map(|r| r.recoveries.iter().map(|x| x.1)).collect();
    let rec_worst = rec_all.iter().map(|r| r.unwrap_or(f64::INFINITY)).fold(0.0, f64::max);
    let accum_worst = worst(&|r| r.accum().abs() as f64);
    // 1절 지터 버퍼 지연: 한도 대비 여유가 가장 작은 피어(음수 = 초과).
    // 판정 대상 피어만 센다(1절은 안정 회선, 상한은 꼬리 지터 회선).
    let slack_worst = |m: Metric| res.iter().filter(|r| r.judged(m)).map(|r| r.limit(m) - r.value(m)).fold(f64::INFINITY, f64::min);
    let who = |c: LineClass| res.iter().enumerate().filter(|(_, r)| r.class == c).map(|(i, _)| format!("#{i}")).collect::<Vec<_>>().join("·");
    let overall = fails.is_empty();
    let goal_all = overall && known.is_empty();
    let all_ok = |m: Metric| res.iter().all(|r| r.ok_if_judged(m));
    let slack = |m: Metric| format!("여유 {:.2}ms", slack_worst(m));

    let mut md = String::new();
    let _ = writeln!(md, "# 장시간 시뮬레이션 리포트 — {tag}\n");
    let _ = writeln!(
        md,
        "가상 시간 {:.0}분, 6피어(이 엔진이 받는 원격 스트림 6개), 결정적(시드 고정), 실행 {:.0}초. 정착 {SETTLE_S:.0}초 뒤부터 측정.\n\n\
         - GOAL 전 항목: **{}**{}\n- 회귀 판정(테스트): **{}** — 실패 {}건(새 미달·악화·래칫), 알려진 미달 {}건(기준값 안)\n",
        seconds / 60.0,
        wall_s,
        pf(goal_all),
        if goal_all { String::new() } else { format!(" (미달 {}건)", fails.len() + known.len()) },
        pf(overall),
        fails.len(),
        known.len()
    );
    let _ = writeln!(md, "## GOAL 기준 판정\n");
    let _ = writeln!(md, "| 기준 (GOAL) | 목표 | 결과(최악 피어) | 판정 |\n|---|---|---|---|");
    let _ = writeln!(md, "| 1절 지연 누적: bufferMs가 목표 ±3ms 밖에 머문 최장 연속 시간(띠 안 복귀 ≤ {BAND_GAP_S:.0}s는 이어 셈) | < {BAND_MAX_S:.0}s | {band_worst:.1}s | {} |", pf(res.iter().all(|r| r.band_ok())));
    let _ = writeln!(md, "| 1절 지연 누적: ±3ms 밖 총 시간 비율 | ≤ {BAND_PCT_MAX:.0}% | {band_pct_worst:.1}% | {} |", pf(all_ok(Metric::BandPct)));
    let _ = writeln!(md, "| 1절 지연 누적: 처음/마지막 창 bufferMs 중앙값 차이(사건 구간 제외, 꼬리 지터 회선은 늦은 패킷 예산을 뺀 값) | ≤ {ACCUM_MAX_MS:.1}ms | {accum_worst:.2}ms | {} |", pf(res.iter().all(|r| r.accum_ok())));
    let (st, tl) = (who(LineClass::Stable), who(LineClass::Tail));
    let _ = writeln!(md, "| 1절 지터 버퍼 추가 지연(안정 회선 {st}): 첫 창 bufferMs 중앙값 ≤ 1F + p95 지터(사건 구간 제외) | 여유 ≥ 0ms | {} | {} |", slack(Metric::GoalDelay), pf(all_ok(Metric::GoalDelay)));
    let _ = writeln!(md, "| 1절 지터 버퍼 추가 지연(안정 회선 {st}): 마지막 창 bufferMs 중앙값 ≤ 1F + 마지막 창 p95 지터(사건 구간 제외) | 여유 ≥ 0ms | {} | {} |", slack(Metric::GoalDelayLast), pf(all_ok(Metric::GoalDelayLast)));
    if !tl.is_empty() {
        let _ = writeln!(md, "| 꼬리 지터 회선({tl}, 1절 지터 버퍼 추가 지연 대상 밖) 지연 상한: 첫 창 bufferMs − 늦은 패킷 예산 ≤ 1F + p95 지터 | 여유 ≥ 0ms | {} | {} |", slack(Metric::TailDelay), pf(all_ok(Metric::TailDelay)));
        let _ = writeln!(md, "| 꼬리 지터 회선({tl}) 지연 상한: 마지막 창 bufferMs − 늦은 패킷 예산 ≤ 1F + 마지막 창 p95 지터 | 여유 ≥ 0ms | {} | {} |", slack(Metric::TailDelayLast), pf(all_ok(Metric::TailDelayLast)));
        let _ = writeln!(md, "| 꼬리 지터 회선({tl}) 늦은 패킷 예산 최댓값(정착 뒤 전 구간) | ≤ 1F | {} | {} |", slack(Metric::TailCap), pf(all_ok(Metric::TailCap)));
        // 회귀 방지 래칫(GOAL 기준 아님): 지금 동작의 관측 분포 + 여유.
        let worst_of = |m: Metric| res.iter().filter(|r| r.judged(m)).map(|r| r.value(m)).fold(f64::NEG_INFINITY, f64::max);
        let decay = worst_of(Metric::TailDecay);
        let _ = writeln!(md, "| [회귀 래칫, GOAL 기준 아님] 꼬리 지터 회선({tl}) 늦은 패킷 예산 수준: 기본 조건 구간 중앙값 | ≤ {TAIL_LEVEL_MAX_F}F | {:.2}ms ({}) | {} |", worst_of(Metric::TailLevel), slack(Metric::TailLevel), pf(all_ok(Metric::TailLevel)));
        let _ = writeln!(md, "| [회귀 래칫] 꼬리 지터 회선({tl}) 늦은 패킷 예산 감쇠: 지터 계단이 풀린 뒤 {TAIL_LEVEL_MAX_F}F 이하까지 | ≤ {TAIL_DECAY_MAX_S:.0}s | {} | {} |", if decay.is_finite() { format!("{decay:.1}s") } else { "내려오지 않음".into() }, pf(all_ok(Metric::TailDecay)));
        let _ = writeln!(md, "| [회귀 래칫] 꼬리 지터 회선({tl}) 총 지연: 첫 창 bufferMs(예산 포함) ≤ 1F + p95 지터 + {TAIL_TOTAL_OVER_MS}ms | 여유 ≥ 0ms | {} | {} |", slack(Metric::TailTotal), pf(all_ok(Metric::TailTotal)));
        let _ = writeln!(md, "| [회귀 래칫] 꼬리 지터 회선({tl}) 총 지연: 마지막 창 bufferMs(예산 포함) ≤ 1F + 마지막 창 p95 지터 + {TAIL_TOTAL_OVER_MS}ms | 여유 ≥ 0ms | {} | {} |", slack(Metric::TailTotalLast), pf(all_ok(Metric::TailTotalLast)));
    }
    let _ = writeln!(md, "| 2절 은닉 비율(무작위 손실 ≤ 1% 회선, 평시) | ≤ {CONCEAL_GOAL_PCT}% | {conc_worst:.2}% | {} |", pf(goal_conc.iter().all(|r| r.ok(Metric::Conceal))));
    let _ = writeln!(md, "| 2절 은닉 비율(버스트·2% 회선, 평시): 실제 손실 + 0.5%p 이내 | 피어별 | 아래 표 | {} |", pf(other_ok));
    let _ = writeln!(md, "| 2절 언더런 은닉 비율(평시, 전 회선): 은닉 한도 − 손실 몫 | ≤ {UNDERRUN_MAX_PCT}% | {under_worst:.2}% | {} |", pf(all_ok(Metric::Underrun)));
    let _ = writeln!(md, "| 2절 UDP 5초 단절 후 복구 시간 ({}회) | ≤ {RECOVER_MAX_S:.0}s | {} | {} |", rec_all.len(), if rec_worst.is_finite() { format!("{rec_worst:.1}s") } else { "복구 안 됨".into() }, pf(res.iter().all(|r| r.recover_ok())));
    let _ = writeln!(md, "| 콜백 xrun(시뮬 워커 기상 지연 기준) | 0 | {} | {} |", out.xruns, pf(out.xruns == 0));
    let _ = writeln!(md, "\n## 피어별\n");
    let _ = writeln!(md, "| # | 회선 | 분류 | bufferMs 중앙값 / p95 | 목표 중앙값 | ±3ms 밖 최장 / 총 (비율) | 은닉 평시 (한도) = 손실 + 언더런 / 사건 포함 | 실제 손실 | 단절 복구 | 처음→마지막 창 bufferMs (목표 대비) | 판정 |\n|---|---|---|---|---|---|---|---|---|---|---|");
    for (i, r) in res.iter().enumerate() {
        let rec = if r.recoveries.is_empty() {
            "—".to_string()
        } else {
            r.recoveries.iter().map(|(_, x)| x.map_or("실패".into(), |s| format!("{s:.1}s"))).collect::<Vec<_>>().join(", ")
        };
        let _ = writeln!(
            md,
            "| {i} | {} | {} | {:.2} / {:.2} | {:.2} | {:.1}s / {:.1}s ({:.1}%) | {:.2}% ({:.2}%) = {:.2} + {:.2} / {:.2}% | {:.2}% | {rec} | {:.2} → {:.2} ({:+.2} → {:+.2}) | {} |",
            r.desc,
            r.class.label(),
            r.buf_med,
            r.buf_p95,
            r.tgt_med,
            r.band_max_s,
            r.band_total_s,
            r.band_pct,
            r.conceal_pct,
            r.conceal_limit,
            r.conceal_loss_pct,
            r.conceal_underrun_pct,
            r.conceal_all,
            r.loss_pct,
            r.buf_first,
            r.buf_last,
            r.off_first,
            r.off_last,
            peer_verdict[i]
        );
    }
    let _ = writeln!(md, "\n## 참고(판정 외)\n");
    let _ = writeln!(md, "| # | 첫 창 bufferMs vs 1F + p95 지터 (GOAL 1절 — 안정 회선만 위에서 판정) | 마지막 창 | 늦은 패킷 예산을 뺀 bufferMs 첫 / 마지막 창 (꼬리 지터 회선 판정) | 늦은 패킷 예산 첫 / 마지막 / 최대 | 첫 창 목표 = 지터 추정 + lead + 문턱·여유 + tail | tail 조정 / 초기화 | 재버퍼 | 늦은 패킷 버림 | 샘플 보정 횟수 | 최대 보정 피치 |\n|---|---|---|---|---|---|---|---|---|---|---|");
    for (i, r) in res.iter().enumerate() {
        let _ = writeln!(
            md,
            "| {i} | {:.2} vs {:.2} ({}, 여유 {:+.2}ms) | {:.2} vs {:.2} ({:+.2}ms) | {:.2} / {:.2} | {:.2} / {:.2} / {:.2} | {:.2} = {:.2} + {:.2} + {:.2} + {:.2} | {} / {} | {} | {} | {} | {:.1}센트 |",
            r.buf_first,
            r.goal_delay_limit(),
            match (r.class, r.ok(Metric::GoalDelay)) {
                (LineClass::Tail, _) => "대상 밖",
                (_, true) => "충족",
                (_, false) => "초과",
            },
            r.goal_delay_limit() - r.buf_first,
            r.buf_last,
            r.goal_delay_limit_last(),
            r.goal_delay_limit_last() - r.buf_last,
            r.base_first,
            r.base_last,
            r.tail_first,
            r.tail_last,
            r.tail_max,
            r.tgt_first,
            r.jit_first,
            r.lead_first,
            r.tgt_first - r.jit_first - r.lead_first - r.tail_first,
            r.tail_first,
            r.tail_adjusts,
            r.tail_resets,
            r.rebuffers,
            r.late_dropped,
            r.corrections,
            r.max_corr_cents
        );
    }
    let _ = writeln!(
        md,
        "\n- 정의: \"목표\" = 엔진 `PeerStats.targetMs`(지터 추정 + 소비자 여유 + 삽입 문턱). 재생이 멈춘 표본(단절 중 재버퍼)은 ±3ms 밖으로 센다.\n\
         - ±3ms 판정: GOAL 문구(\"1분 이상 머무르지 않음\")를 (1) 띠 안 복귀 {BAND_GAP_S:.0}s 이하를 무시한 최장 연속 < {BAND_MAX_S:.0}s와 (2) 띠 밖 총 비율 ≤ {BAND_PCT_MAX:.0}%(60분 중 6분)로 본다. 경계를 오가는 피어가 연속 판정을 피해 가지 않게 하기 위함.\n\
         - 회선 분류(GOAL 1절 \"안정 회선\"의 뜻, P1-6d): 망 지터 분포가 유계(균등)면 안정 회선 — 1절 지터 버퍼 추가 지연을 건다. 꼬리 긴 분포(지수·파레토)를 쓰는 회선은 1절 대상 밖이고, 대신 늦은 패킷 예산(`tail_extra`)을 뺀 bufferMs ≤ 1F + p95 지터, 예산 ≤ 1F, 예산을 뺀 bufferMs의 첫·마지막 창 차이 ≤ {ACCUM_MAX_MS:.1}ms를 건다(합치면 bufferMs ≤ 1절 한도 + 1F, 늘지 않음). 2절과 ±3ms 띠는 모든 회선에 같다.\n\
         - 꼬리 지터 회선 회귀 래칫(**GOAL 기준 아님** — 지금 엔진 동작의 관측 분포에 여유를 둔 회귀 방지 한도, PR #248 리뷰 M1): 위 판정은 예산을 지연에서 빼므로 예산이 커지거나 내려오지 않는 회귀를 못 본다. 그래서 (1) 기본 조건 구간(사건·계단 구간 밖) 예산 중앙값 ≤ {TAIL_LEVEL_MAX_F}F, (2) 지터 계단이 풀린 뒤 예산이 그 한도 이하로 내려오기까지 ≤ {TAIL_DECAY_MAX_S:.0}s, (3) 예산을 포함한 첫·마지막 창 bufferMs ≤ 1절 한도 + {TAIL_TOTAL_OVER_MS}ms를 건다.\n\
         - 은닉 평시: 단절·스파이크 구간(끝 + 10초까지)과 계단 직후 10초를 뺀 구간. 계단 뒤 새 조건의 정상 상태는 평시로 센다.\n\
         - 은닉 = 손실 몫(마감에 그 패킷이 없고 뒤 패킷은 와 있어 seq를 넘긴 은닉 — 실제 손실 + 너무 늦게 온 패킷, 실제 Opus는 FEC/PLC) + 언더런 몫(마감에 뒤 패킷도 없음, seq 유지·깊이 +1F). 손실 몫 − 실제 손실 ≈ 늦은 패킷 몫. 언더런 예산 {UNDERRUN_MAX_PCT}% = GOAL 2절 한도 1.5% − 손실 1%.\n\
         - 복구 시간: 단절 끝(패킷 재개)부터 재생 중 + 목표 ±3ms 안이 1초 이어지기 시작한 시각까지.\n\
         - 지연 누적: 처음 창과 마지막 창(각 {:.0}s)에서 사건 구간을 뺀 bufferMs 중앙값 차이(같은 망 조건).\n\
         - 알려진 미달: 기준값 ± 허용폭 안이면 테스트 통과(리포트는 FAIL(알려진)). 악화·크게 개선·해소는 모두 테스트 실패 — 개선되면 기준값을 낮추거나 지정을 지운다(래칫).\n\
         - 모델 한계: 코덱은 PCM16(Opus PLC/FEC 미모델 — 은닉 프레임 수는 같지만 FEC 복원은 반영 안 됨), CPU·실장비 xrun은 측정 대상 아님(GOAL 2절 CPU ≤ 15%는 [사람] 확인).\n\
         - 복구 시간은 지터 버퍼 복구만 잰다(0.1s는 표본 간격 하한). UDP 경로 재수립(`net::peer` Reconnecting → keepalive·홀펀칭·NAT 매핑 만료)은 미모델 — 실네트워크 [사람].\n\
         - WS 재접속(30초 이내, 세션 재개)은 엔진을 유지하고 descriptor만 재공지하므로 엔진 입력이 없다 → 시뮬레이션 대상 아님.\n",
        accum_win(seconds)
    );
    for (title, list) in [("테스트 실패(새 미달·악화·래칫)", &fails), ("알려진 미달(백로그, 기준값 안)", &known), ("알려진 미달 해소·개선(기준값 갱신 필요)", &resolved)] {
        if !list.is_empty() {
            let _ = writeln!(md, "\n## {title}\n");
            for f in list {
                let _ = writeln!(md, "- {f}");
            }
        }
    }

    let peers: Vec<Value> = res
        .iter()
        .zip(specs)
        .enumerate()
        .map(|(i, (r, s))| {
            json!({
                "index": i,
                "line": r.desc,
                "lineClass": r.class.key(),
                "frameMs": r.frame_ms,
                "driftPpm": s.np.drift_ppm,
                "bufferMs": { "median": r.buf_med, "p95": r.buf_p95 },
                "targetMsMedian": r.tgt_med,
                "outOfBand": { "maxRunS": r.band_max_s, "maxRunAtS": r.band_max_at, "totalS": r.band_total_s, "pct": r.band_pct, "pass": r.band_ok() && r.ok(Metric::BandPct) },
                "conceal": {
                    "pct": r.conceal_pct, "lossPct": r.conceal_loss_pct, "underrunPct": r.conceal_underrun_pct,
                    "pctWithEvents": r.conceal_all, "limitPct": r.conceal_limit, "underrunLimitPct": UNDERRUN_MAX_PCT,
                    "actualLossPct": r.loss_pct, "pass": r.ok(Metric::Conceal) && r.ok(Metric::Underrun),
                },
                "recoveries": r.recoveries.iter().map(|(e, x)| json!({ "outageEndS": e, "recoverS": x })).collect::<Vec<_>>(),
                "recoverPass": r.recover_ok(),
                "accumulation": { "firstWindowBufferMs": r.buf_first, "lastWindowBufferMs": r.buf_last, "deltaMs": r.buf_last - r.buf_first, "judgedDeltaMs": r.accum(), "offsetFirstMs": r.off_first, "offsetLastMs": r.off_last, "pass": r.accum_ok() },
                "goalDelay": { "bufferMs": r.buf_first, "oneFramePlusP95Ms": r.goal_delay_limit(), "within": r.ok(Metric::GoalDelay), "lastBufferMs": r.buf_last, "lastOneFramePlusP95Ms": r.goal_delay_limit_last(), "lastWithin": r.ok(Metric::GoalDelayLast), "judged": r.judged(Metric::GoalDelay) },
                "tailBound": {
                    "judged": r.judged(Metric::TailDelay),
                    "baseFirstMs": r.base_first, "baseLastMs": r.base_last, "tailFirstMs": r.tail_first, "tailLastMs": r.tail_last, "tailMaxMs": r.tail_max,
                    "within": r.ok(Metric::TailDelay), "lastWithin": r.ok(Metric::TailDelayLast), "capWithin": r.ok(Metric::TailCap),
                },
                // 회귀 방지 래칫(GOAL 기준 아님). 감쇠 시간은 잴 계단이 없으면 null, 내려오지 않으면 -1.
                "tailRatchet": {
                    "judged": r.judged(Metric::TailLevel),
                    "levelMs": r.tail_base, "levelLimitMs": tail_level_limit(r.frame_ms), "levelWithin": r.ok(Metric::TailLevel),
                    "decayS": r.tail_decay_s.map(|d| if d.is_finite() { d } else { -1.0 }), "decayLimitS": TAIL_DECAY_MAX_S, "decayWithin": r.ok(Metric::TailDecay),
                    "totalOverMs": r.buf_first - r.goal_delay_limit(), "totalOverLastMs": r.buf_last - r.goal_delay_limit_last(), "totalOverLimitMs": TAIL_TOTAL_OVER_MS,
                    "totalWithin": r.ok(Metric::TailTotal), "totalLastWithin": r.ok(Metric::TailTotalLast),
                },
                "targetFirst": { "targetMs": r.tgt_first, "jitterEstMs": r.jit_first, "leadMs": r.lead_first, "tailMs": r.tail_first },
                "tail": { "adjusts": r.tail_adjusts, "resets": r.tail_resets },
                "known": r.known.iter().filter(|k| k.base(seconds).is_some()).map(|k| json!({ "metric": format!("{:?}", k.metric), "base": k.base(seconds), "tol": k.tol, "why": k.why })).collect::<Vec<_>>(),
                "verdict": peer_verdict[i],
                "rebuffers": r.rebuffers,
                "lateDropped": r.late_dropped,
                "corrections": r.corrections,
                "maxCorrCents": r.max_corr_cents,
            })
        })
        .collect();
    let json = json!({
        "tag": tag,
        "simSeconds": seconds,
        "wallSeconds": wall_s,
        "pass": overall,
        "goalAllPass": goal_all,
        "xruns": out.xruns,
        "criteria": {
            "bandMs": BAND_MS, "bandMaxS": BAND_MAX_S, "bandGapS": BAND_GAP_S, "bandPctMax": BAND_PCT_MAX,
            "concealGoalPct": CONCEAL_GOAL_PCT, "underrunMaxPct": UNDERRUN_MAX_PCT,
            "recoverMaxS": RECOVER_MAX_S, "accumMaxMs": ACCUM_MAX_MS,
            "tailRatchet": { "levelMaxFrames": (TAIL_LEVEL_MAX_F as f64 * 100.0).round() / 100.0, "decayMaxS": TAIL_DECAY_MAX_S, "totalOverMs": TAIL_TOTAL_OVER_MS },
        },
        "fails": fails,
        "knownShortfalls": known,
        "resolved": resolved,
        "peers": peers,
    });
    Report { md, json, fails }
}

fn run_and_report(tag: &str, seconds: f64) -> Value {
    let (json, fails, md_path) = run_report_wake(tag, seconds, WakeModel::Uniform);
    assert!(fails.is_empty(), "판정 실패 {}건(GOAL 미달·회귀 래칫): {:?} (리포트 {})", fails.len(), fails, md_path.display());
    json
}

/// 시나리오를 워커 기상 모델을 골라 돌리고 리포트(Markdown·JSON)를 쓴다(P2-17f). 판정 실패 목록을 돌려줄 뿐 단언하지 않는다 —
/// fixture·야간 60분(`run_and_report`)은 `Uniform`으로 돌고 실패하면 단언하고, 실측 모델(`wake_measured_6peers_*`)은 측정만 한다.
fn run_report_wake(tag: &str, seconds: f64, wake: WakeModel) -> (Value, Vec<String>, std::path::PathBuf) {
    let t0 = std::time::Instant::now();
    let (specs, out, res) = run_seeded(seconds, wake, 0);
    let wall_s = t0.elapsed().as_secs_f64();
    let rep = build_report(tag, seconds, &specs, &out, &res, wall_s);
    let dir = std::env::var_os("SIM_REPORT_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("band-tauri-sim"));
    std::fs::create_dir_all(&dir).unwrap();
    let md_path = dir.join(format!("sim-report-{tag}.md"));
    std::fs::write(&md_path, &rep.md).unwrap();
    std::fs::write(dir.join(format!("sim-report-{tag}.json")), serde_json::to_string_pretty(&rep.json).unwrap()).unwrap();
    println!("{}\n리포트: {}", rep.md, md_path.display());
    let [np, nf, ns] = out.naps;
    let total = (np + nf + ns).max(1) as f64;
    println!("워커 기다림(P2-17g): POLL {np} ({:.2}%), FINE_POLL {nf} ({:.2}%), 돌기 {ns} ({:.3}%)", np as f64 * 100.0 / total, nf as f64 * 100.0 / total, ns as f64 * 100.0 / total);
    if !out.xrun_us.is_empty() {
        let at: Vec<String> = out.xrun_us.iter().map(|&t| format!("{:.3}", t as f64 / 1e6)).collect();
        println!("xrun 시각(s, 워커 사건 기준): {}", at.join(", "));
    }
    (rep.json, rep.fails, md_path)
}

/// 시나리오를 시드 변형 `k`로 돌린다. `k = 0`이 fixture·야간 60분의 시드(하네스 6, 피어 1001~1006)이고, `k > 0`은 하네스 시드
/// (워커 기상)와 피어 시드(망 지터·손실 실현)를 함께 옮긴 다른 실현이다 — 판정이 시드 하나에 기대지 않는지 볼 때 쓴다.
fn run_seeded(seconds: f64, wake: WakeModel, k: u64) -> (Vec<Spec>, MultiOut, Vec<PeerResult>) {
    let mut specs = scenario(seconds);
    for s in &mut specs {
        s.np.seed += 100 * k;
    }
    let mut sim = MultiSim::new(seconds, specs.iter().map(|s| s.np.clone()).collect());
    sim.settle_us = (SETTLE_S * 1e6) as u64;
    sim.seed = 6 + k;
    sim.wake = wake;
    let out = run_multi(&sim);
    let res: Vec<PeerResult> = specs.iter().zip(&out.peers).map(|(s, p)| analyze(&s.np, s.rule, s.known, p, seconds)).collect();
    (specs, out, res)
}

/// 동작 불변 기준값(engine-architecture.md 9.1절): 리포트 JSON에서 실행 시간(`wallSeconds`)을 뺀 나머지가
/// `tests/fixtures/sim-report-<tag>.json`과 바이트 단위로 같아야 한다. 하네스는 결정적이므로 구조 정리 PR은 이 값을
/// 바꾸지 않는다. 동작을 의도적으로 바꾸는 PR만 `SIM_UPDATE_FIXTURE=1`로 다시 쓰고, PR 본문에 차이를 적는다.
fn check_fixture(tag: &str, report: &Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(format!("sim-report-{tag}.json"));
    let mut v = report.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("wallSeconds");
    }
    let got = serde_json::to_string_pretty(&v).unwrap() + "\n";
    if std::env::var_os("SIM_UPDATE_FIXTURE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        println!("기준값 갱신: {}", path.display());
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("기준값 {} 읽기 실패({e}) — SIM_UPDATE_FIXTURE=1로 만든다", path.display()))
        .replace("\r\n", "\n");
    if want != got {
        let (n, (w, g)) = want.lines().zip(got.lines()).enumerate().find(|(_, (w, g))| w != g).unwrap_or((0, ("(길이 다름)", "")));
        panic!(
            "5분 하네스 결과가 기준값과 다르다(동작 변경). 첫 차이 {}행:\n  기준: {w}\n  실행: {g}\n의도한 변경이면 SIM_UPDATE_FIXTURE=1로 {}을 다시 쓰고 PR 본문에 차이를 적을 것",
            n + 1,
            path.display()
        );
    }
}

/// 6피어 5분(가상): 기본 `cargo test`에서 도는 짧은 버전. 60분 버전과 같은 시나리오 모양(사건 위치는 길이 비율).
#[test]
fn long_sim_6peers_5min() {
    check_fixture("short-5min", &run_and_report("short-5min", 300.0));
}

/// 6피어 60분(가상, 디버그 빌드 수 분). CI 야간 잡(`long-sim`)과 수동 실행용.
#[test]
#[ignore = "long: 6피어 60분 가상 시간(디버그 로컬 약 8분) — 야간 CI long-sim 잡에서 실행"]
fn long_sim_6peers_60min() {
    run_and_report("long-60min", 3600.0);
}

/// 측정 도구(P2-17f): 6피어 5분을 실측 워커 기상 모델(`WakeModel::Measured`)로 돌려 같은 판정표를 낸다(fixture·단언 없음).
/// 2026-11-18(시드 6~11): 1절 여유는 두 모델이 시드 흩어짐 안에서 같고, xrun은 실측 모델에서만 난다(시드 6개 중 3개에서 1~2회) —
/// 마감 근처가 아닌 반복의 `POLL` 수면이 꼬리(≥ 1.8ms)로 늦게 깨면 urgent 판정 창(여유 + `POLL`)을 넘는다(엔진 후속 후보).
/// `cargo test -p band-engine --release --test sim wake_measured_6peers_5min -- --ignored --nocapture`
#[test]
#[ignore = "측정: 6피어 5분, 실측 기상 모델 — 판정표만"]
fn wake_measured_6peers_5min() {
    let (_, fails, _) = run_report_wake("short-5min-measured-wake", 300.0, WakeModel::Measured);
    println!("실측 기상 모델 판정 실패 {}건: {fails:?}", fails.len());
}

/// 측정 도구(P2-17f): 6피어 60분, 실측 워커 기상 모델(판정표만). 2026-11-18: xrun 9회, 그 밖의 GOAL 항목은 PASS.
#[test]
#[ignore = "측정: 6피어 60분 가상 시간, 실측 기상 모델 — 판정표만"]
fn wake_measured_6peers_60min() {
    let (_, fails, _) = run_report_wake("long-60min-measured-wake", 3600.0, WakeModel::Measured);
    println!("실측 기상 모델 판정 실패 {}건: {fails:?}", fails.len());
}

/// 지연 민감도(엔진 ③-a3, 설계 6.4절 (4)): 5분 시나리오를 패킷이 도착하고도 `rx_hold_us`만큼 더 큐에 묵은 뒤에야
/// 워커에 보이게 해서(도착 시각은 그대로) 피어별 평시 은닉률·버퍼 중앙값을 잰다. 실제 설계는 0(= fixture)이다.
/// 수치 판정 없이 표만 출력한다(측정 도구): `cargo test --lib rx_hold_sensitivity -- --ignored --nocapture`.
#[test]
#[ignore = "측정: 5분 시나리오 4회(디버그 수 분)"]
fn rx_hold_sensitivity() {
    let seconds = 300.0;
    let specs = scenario(seconds);
    let mut rows = Vec::new();
    for hold in [0u64, 150, 500, 1500] {
        let mut sim = MultiSim::new(seconds, specs.iter().map(|s| s.np.clone()).collect());
        sim.settle_us = (SETTLE_S * 1e6) as u64;
        sim.seed = 6;
        sim.rx_hold_us = hold;
        let out = run_multi(&sim);
        let res: Vec<PeerResult> = specs.iter().zip(&out.peers).map(|(s, p)| analyze(&s.np, s.rule, s.known, p, seconds)).collect();
        let conceal: Vec<String> = res.iter().map(|r| format!("{:.2}", r.conceal_pct)).collect();
        let buf: Vec<String> = res.iter().map(|r| format!("{:.1}", r.buf_med)).collect();
        let late: Vec<String> = out.peers.iter().map(|p| p.js.late.to_string()).collect();
        rows.push(format!("| {hold} | {} | {} | {} |", conceal.join(" / "), buf.join(" / "), late.join(" / ")));
    }
    println!("| rx_hold_us | 피어 1~6 평시 은닉률(%) | 피어 1~6 bufferMs 중앙값 | 피어 1~6 late |");
    println!("|---|---|---|---|");
    for r in rows {
        println!("{r}");
    }
}

/// 시드 스윕(P1-6d·P2-16d): 시드 변형 `k = 0..n`으로 시나리오를 돌려 피어별 판정 수치를 표로 내고, 변형마다의 판정 실패
/// 목록을 돌려준다. "이전 판정" 열은 회선 분류를 넣기 전 규칙(1절 지터 버퍼 추가 지연과 bufferMs 누적을 모든 피어에)으로 같은
/// 수치를 다시 본 것이다 — 꼬리 지터 피어가 시드에 따라 갈리던 것을 나란히 보인다.
fn seed_sweep(seconds: f64, n: u64) -> Sweep {
    seed_sweep_range(seconds, 0..n)
}

/// 시드 스윕 결과: 변형마다의 판정 실패 목록과, 꼬리 지터 피어의 (피어 번호, 실행별 예산 수준 ÷ 프레임).
struct Sweep {
    runs: Vec<(u64, Vec<String>)>,
    tail_level_f: Vec<(usize, Vec<f32>)>,
}

fn seed_sweep_range(seconds: f64, ks: std::ops::Range<u64>) -> Sweep {
    println!("| 시드 변형 | # | 분류 | 1절 여유 첫 / 마지막 창(ms) | 이전 판정 | 예산 뺀 여유 첫 / 마지막 창(ms) | 늦은 패킷 예산 첫 / 마지막 / 최대(ms) | 예산 수준(ms) / 감쇠(s) | 은닉 평시 (한도) | 언더런 은닉 | 지연 누적(ms) | 새 판정 |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut all = Sweep { runs: Vec::new(), tail_level_f: Vec::new() };
    for k in ks {
        let (specs, out, res) = run_seeded(seconds, WakeModel::Uniform, k);
        let rep = build_report("sweep", seconds, &specs, &out, &res, 0.0);
        for (i, r) in res.iter().enumerate() {
            let (s1, s2) = (r.goal_delay_limit() - r.buf_first, r.goal_delay_limit_last() - r.buf_last);
            let old_ok = s1 >= 0.0 && s2 >= 0.0 && (r.buf_last - r.buf_first).abs() <= ACCUM_MAX_MS;
            let tag = format!("#{i} ");
            let new_ok = !rep.fails.iter().any(|f| f.starts_with(&tag));
            println!(
                "| {k} | {i} | {} | {s1:+.2} / {s2:+.2} | {} | {:+.2} / {:+.2} | {:.2} / {:.2} / {:.2} | {:.2} / {} | {:.2}% ({:.2}%) | {:.2}% | {:+.2} | {} |",
                r.class.label(),
                pf(old_ok),
                r.goal_delay_limit() - r.base_first,
                r.goal_delay_limit_last() - r.base_last,
                r.tail_first,
                r.tail_last,
                r.tail_max,
                r.tail_base,
                r.tail_decay_s.map_or("—".into(), |d| if d.is_finite() { format!("{d:.0}") } else { "∞".into() }),
                r.conceal_pct,
                r.conceal_limit,
                r.conceal_underrun_pct,
                r.accum(),
                pf(new_ok)
            );
        }
        for (i, r) in res.iter().enumerate().filter(|(_, r)| r.judged(Metric::TailLevel)) {
            match all.tail_level_f.iter_mut().find(|x| x.0 == i) {
                Some(x) => x.1.push(r.tail_base / r.frame_ms),
                None => all.tail_level_f.push((i, vec![r.tail_base / r.frame_ms])),
            }
        }
        all.runs.push((k, rep.fails));
    }
    for (i, v) in &all.tail_level_f {
        println!("꼬리 지터 피어 #{i} 예산 수준 시드 평균 {:.3}F ({}개, 시드 평균 래칫 ≤ {TAIL_LEVEL_MEAN_MAX_F}F는 {TAIL_MEAN_MIN_SEEDS}개 이상일 때)", mean(v), v.len());
    }
    all
}

fn mean(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len().max(1) as f32
}

/// 시드 평균 래칫 위반 목록(회귀 방지 — GOAL 기준 아님): 시드가 `TAIL_MEAN_MIN_SEEDS`개 이상일 때만 본다.
fn tail_mean_fails(levels_f: &[(usize, Vec<f32>)]) -> Vec<String> {
    levels_f
        .iter()
        .filter(|(_, v)| v.len() >= TAIL_MEAN_MIN_SEEDS && mean(v) > TAIL_LEVEL_MEAN_MAX_F)
        .map(|(i, v)| format!("#{i} 늦은 패킷 예산 수준 시드 평균 {:.3}F > {TAIL_LEVEL_MEAN_MAX_F}F({}개, 회귀 래칫)", mean(v), v.len()))
        .collect()
}

fn sweep_n(default: u64) -> u64 {
    std::env::var("SIM_SWEEP_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn assert_sweep(all: &Sweep) {
    let bad: Vec<_> = all.runs.iter().filter(|(_, f)| !f.is_empty()).collect();
    assert!(bad.is_empty(), "시드 변형 {}개 중 {}개에서 판정 실패: {bad:?}", all.runs.len(), bad.len());
    let mean_bad = tail_mean_fails(&all.tail_level_f);
    assert!(mean_bad.is_empty(), "시드 평균 래칫 실패: {mean_bad:?}");
}

/// 판정이 시드 하나에 기대지 않는다(P1-6d의 남은 일): 5분 시나리오를 fixture와 다른 시드(변형 1)로 돌려도 통과해야 한다. 변형 0은
/// `long_sim_6peers_5min`이 같은 판정으로 돌리므로 여기서 다시 돌리지 않는다 — PR마다 도는 5분 실행을 하나만 더한다(PR #248 리뷰
/// Low: 변형 0~2를 돌리던 때 CI Windows 잡이 약 1.5분 늘었다). 회선 분류 전에는 꼬리 지터 피어 #1의 1절 여유가 시드마다 한도
/// 양쪽으로 갈렸다(변형 2: 첫 창 −0.69ms). 더 많은 시드는 `long_sim_seed_sweep_5min`(야간 CI `long-sim` 잡과 수동).
#[test]
fn long_sim_judgement_is_seed_stable() {
    assert_sweep(&seed_sweep_range(300.0, 1..2));
}

/// 시드 스윕 5분(기본 10개, `SIM_SWEEP_SEEDS`로 바꾼다 — 2026-11-20 40개 전부 통과). 실행마다의 판정에 더해 꼬리 지터 피어의
/// 예산 수준 시드 평균 래칫(`TAIL_LEVEL_MEAN_MAX_F`)을 건다. 야간 CI `long-sim` 잡이 기본 10개를 돈다.
/// `cargo test -p band-engine --release --no-default-features --features test-support --test sim long_sim_seed_sweep_5min -- --ignored --nocapture`
#[test]
#[ignore = "측정·판정: 5분 시나리오 시드 변형 10개(release 권장)"]
fn long_sim_seed_sweep_5min() {
    assert_sweep(&seed_sweep(300.0, sweep_n(10)));
}

/// 시드 스윕 60분(기본 5개, release에서 변형 하나 약 1분).
#[test]
#[ignore = "측정·판정: 60분 시나리오 시드 변형 5개(release 권장)"]
fn long_sim_seed_sweep_60min() {
    assert_sweep(&seed_sweep(3600.0, sweep_n(5)));
}

/// 회선 분류(GOAL 1절 "안정 회선"의 뜻)가 시나리오와 지표에 맞게 걸린다: 꼬리 지터 피어는 #1뿐이고, 1절 지터 버퍼 추가 지연에서
/// 빠진 자리에 상한 판정 셋이 들어가며 2절(은닉·언더런)·띠 판정은 그대로 남는다(빼기만 하고 아무것도 안 보는 피어가 없다).
#[test]
fn long_sim_line_class_follows_goal_definition() {
    let specs = scenario(300.0);
    let classes: Vec<LineClass> = specs.iter().map(|s| LineClass::of(&s.np)).collect();
    assert_eq!(classes, [LineClass::Stable, LineClass::Tail, LineClass::Stable, LineClass::Stable, LineClass::Stable, LineClass::Stable]);
    assert!(specs.iter().all(|s| s.known.is_empty()), "알려진 미달 지정이 남았다");

    // 분포가 도중에 꼬리 긴 쪽으로 바뀌는 회선도 꼬리 지터다(한 번이라도 쓰면).
    let mut np = NetPeer::new(240, 1);
    assert_eq!(LineClass::of(&np), LineClass::Stable);
    np.dist_switch = Some((SEC, JitDist::Pareto));
    assert_eq!(LineClass::of(&np), LineClass::Tail);
    let mut np = NetPeer::new(240, 1);
    np.dist_steps = vec![(SEC, JitDist::Exp), (2 * SEC, JitDist::Uniform)];
    assert_eq!(LineClass::of(&np), LineClass::Tail);
    // 스파이크·손실 모양은 분류를 바꾸지 않는다(스파이크 구간은 사건으로 빼고 그 사이에 1절을 그대로 건다).
    let mut np = NetPeer::new(240, 1);
    np.spikes = vec![(SEC, 300_000, 30_000)];
    np.burst = Some((0.004, 0.5));
    assert_eq!(LineClass::of(&np), LineClass::Stable);

    let (st, tl) = (LineClass::Stable.metrics(), LineClass::Tail.metrics());
    for m in [Metric::Conceal, Metric::Underrun, Metric::BandPct] {
        assert!(st.contains(&m) && tl.contains(&m), "{m:?}는 모든 회선에 건다");
    }
    for m in [Metric::GoalDelay, Metric::GoalDelayLast] {
        assert!(st.contains(&m) && !tl.contains(&m), "{m:?}는 안정 회선에만 건다");
    }
    for m in [Metric::TailDelay, Metric::TailDelayLast, Metric::TailCap, Metric::TailLevel, Metric::TailDecay, Metric::TailTotal, Metric::TailTotalLast] {
        assert!(!st.contains(&m) && tl.contains(&m), "{m:?}는 꼬리 지터 회선에만 건다");
    }
    // 예산 감쇠 판정은 지터 계단 하강이 있어야 잴 것이 있다: 꼬리 지터 피어 #1의 60% 지점 계단(지터 4 → 2ms)이 그것이다.
    // 시나리오를 고치다 이 계단이 사라지면 감쇠 판정이 조용히 빈 판정이 되므로 여기서 막는다.
    assert_eq!(jitter_drop_at(&specs[1].np), Some(180.0), "#1의 지터 계단 하강(예산 감쇠 판정의 기준 시각)");
    assert!(specs.iter().enumerate().all(|(i, s)| i == 1 || LineClass::of(&s.np) == LineClass::Stable));
    // 오르기만 하는 계단은 하강이 아니다.
    let mut np = NetPeer::new(240, 1);
    np.net_jitter_us = 2000;
    np.steps = vec![(SEC, 0, 4000)];
    assert_eq!(jitter_drop_at(&np), None);
    np.steps = vec![(SEC, 0, 4000), (2 * SEC, 0, 3000), (3 * SEC, 8000, 3000)];
    assert_eq!(jitter_drop_at(&np), Some(2.0));
}

/// 꼬리 지터 회선의 상한 판정이 실제로 문다: 5분 #1의 수치에서 (가) 늦은 패킷 예산이 1프레임을 넘거나 (나) 예산을 뺀 지연이 1절
/// 한도를 넘거나 (다) 예산을 뺀 지연이 창 사이에 쌓이면 실패하고, 예산 안에서 bufferMs가 1절 한도를 넘는 것은 통과한다.
/// 회귀 방지 래칫(PR #248 리뷰 M1)도 문다: (라) 예산이 기본 조건 구간에서 높게 머물거나 (마) 계단이 풀린 뒤 내려오지 않거나
/// (바) 예산을 포함한 bufferMs가 1절 한도 + `TAIL_TOTAL_OVER_MS`를 넘으면 실패한다 — (가)~(다)는 통과하는 수치에서.
#[test]
fn long_sim_tail_bounds_bite() {
    // 5분 #1 시드 변형 0의 모양(F 5ms, p95 지터 6.68ms → 1절 한도 11.68ms, 기본 조건 예산 중앙값 2.25ms, 감쇠 18초).
    let tail = |buf_first: f32, buf_last: f32, tail_first: f32, tail_last: f32, tail_max: f32| {
        let mut np = NetPeer::new(240, 1);
        np.dist = JitDist::Exp;
        let mut r = analyze(&np, LossRule::Goal, &[], &PeerOut::default(), 300.0);
        r.p95_jit = 6.68;
        r.p95_last = 6.68;
        (r.buf_first, r.buf_last, r.tail_first, r.tail_last, r.tail_max) = (buf_first, buf_last, tail_first, tail_last, tail_max);
        (r.base_first, r.base_last) = (buf_first - tail_first, buf_last - tail_last);
        (r.tail_base, r.tail_decay_s) = (2.25, Some(18.0));
        r
    };
    const TAIL_METRICS: [Metric; 7] = [Metric::TailDelay, Metric::TailDelayLast, Metric::TailCap, Metric::TailLevel, Metric::TailDecay, Metric::TailTotal, Metric::TailTotalLast];
    let ok = |r: &PeerResult| r.class.metrics().iter().all(|m| !TAIL_METRICS.contains(m) || r.ok(*m)) && r.accum_ok();
    // 이전 판정 셋(예산을 지연에서 뺀 상한)만 본 결과 — 래칫이 없던 때 통과하던 것을 가린다.
    let old_ok = |r: &PeerResult| [Metric::TailDelay, Metric::TailDelayLast, Metric::TailCap].iter().all(|&m| r.ok(m)) && r.accum_ok();
    // 실제 수치: 마지막 창 bufferMs 12.17ms가 1절 한도(11.68ms)를 넘지만 예산 2.5ms를 빼면 9.67ms.
    let r = tail(11.27, 12.17, 2.0, 2.5, 5.0);
    assert_eq!(r.class, LineClass::Tail);
    assert!(!r.judged(Metric::GoalDelayLast) && r.value(Metric::GoalDelayLast) > r.limit(Metric::GoalDelayLast));
    assert!(ok(&r));
    // (가) 예산이 1프레임(5ms)을 넘는다.
    assert!(!tail(11.27, 12.17, 2.0, 2.5, 5.25).ok(Metric::TailCap));
    // (나) 예산 밖 지연이 1절 한도를 넘는다(첫 창 / 마지막 창).
    assert!(!tail(13.8, 12.17, 2.0, 2.5, 5.0).ok(Metric::TailDelay));
    assert!(!tail(11.27, 14.3, 2.0, 2.5, 5.0).ok(Metric::TailDelayLast));
    // (다) 예산을 뺀 지연이 첫 창 → 마지막 창에 1ms 넘게 쌓인다(한도 안이어도).
    assert!(!tail(9.5, 12.6, 1.0, 3.0, 5.0).accum_ok());
    // 예산이 움직인 만큼만 달라진 것은 누적이 아니다.
    assert!(tail(11.0, 12.5, 1.0, 2.5, 5.0).accum_ok());

    // ── 회귀 방지 래칫 ──
    // 리뷰 M1의 고착 변형(예산 내림을 끈 엔진, 5분 #1): 예산이 1F에 붙어 마지막 창 bufferMs가 1절 한도를 2.7ms 넘는다. 예산을
    // 빼면 9.4ms라 이전 판정 셋은 통과한다 — 래칫 셋이 각각 잡는다.
    let mut stuck = tail(11.27, 14.4, 2.0, 5.0, 5.0);
    (stuck.tail_base, stuck.tail_decay_s) = (4.0, Some(f64::INFINITY));
    assert!(old_ok(&stuck), "이전 판정은 고착을 통과시켰다");
    assert!(!stuck.ok(Metric::TailLevel) && !stuck.ok(Metric::TailDecay) && !stuck.ok(Metric::TailTotalLast) && !ok(&stuck));
    // (라) 예산 수준만 높다: 한도 0.65F = 3.25ms. 관측 최대(2.50ms)와 격자 3칸 위(3.25ms)는 통과, 그 한 칸 위는 실패.
    let level = |ms: f32| {
        let mut r = tail(11.27, 12.17, 2.0, 2.5, 5.0);
        r.tail_base = ms;
        r
    };
    assert!(ok(&level(2.5)) && ok(&level(3.25)));
    assert!(old_ok(&level(3.5)) && !level(3.5).ok(Metric::TailLevel));
    // (마) 감쇠만 느리다 / 내려오지 않는다. 계단이 없는 회선(None)은 잴 것이 없어 통과.
    let decay = |d: Option<f64>| {
        let mut r = tail(11.27, 12.17, 2.0, 2.5, 5.0);
        r.tail_decay_s = d;
        r
    };
    assert!(ok(&decay(Some(35.0))) && ok(&decay(None)));
    assert!(old_ok(&decay(Some(61.0))) && !decay(Some(61.0)).ok(Metric::TailDecay));
    assert!(!decay(Some(f64::INFINITY)).ok(Metric::TailDecay));
    // (바) 총 지연: 1절 한도 11.68 + 1.25 = 12.93ms. 관측 최대(한도 + 0.69ms)는 통과, 예산 안에서(≤ 1F) 그 위로 오르면 실패.
    assert!(ok(&tail(12.37, 12.17, 3.0, 2.5, 5.0)));
    let r = tail(13.0, 12.17, 3.75, 2.5, 5.0);
    assert!(old_ok(&r) && !r.ok(Metric::TailTotal) && r.ok(Metric::TailTotalLast));
    let r = tail(11.27, 13.0, 2.0, 3.75, 5.0);
    assert!(old_ok(&r) && r.ok(Metric::TailTotal) && !r.ok(Metric::TailTotalLast));
    // 시드 평균 래칫(스윕 전용): 실행마다 한도(0.65F) 안이어도 평균이 0.48F를 넘으면 실패. 시드가 5개 미만이면 보지 않는다.
    // 관측(5분 변형 0~4) 0.45·0.35·0.45·0.40·0.45F는 통과, 느린 내림을 끈 변형 0.45·0.50·0.60·0.50·0.60F는 실패.
    assert!(tail_mean_fails(&[(1, vec![0.45, 0.35, 0.45, 0.40, 0.45])]).is_empty());
    assert_eq!(tail_mean_fails(&[(1, vec![0.45, 0.50, 0.60, 0.50, 0.60])]).len(), 1);
    assert!(tail_mean_fails(&[(1, vec![0.60, 0.60, 0.60, 0.60])]).is_empty());
}

/// 래칫: 알려진 미달은 기준값 ± tol 안에서만 통과하고, 악화·크게 개선·해소는 모두 실패로 분류된다.
#[test]
fn long_sim_ratchet() {
    assert_eq!(ratchet(1.0, 1.5, None, 0.3), Ratchet::Pass);
    assert_eq!(ratchet(2.0, 1.5, None, 0.3), Ratchet::New);
    assert_eq!(ratchet(2.1, 1.5, Some(2.0), 0.3), Ratchet::Known);
    assert_eq!(ratchet(2.4, 1.5, Some(2.0), 0.3), Ratchet::Worse);
    assert_eq!(ratchet(1.6, 1.5, Some(2.0), 0.3), Ratchet::Improved);
    assert_eq!(ratchet(1.4, 1.5, Some(2.0), 0.3), Ratchet::Resolved);
}

/// 하네스 자체 검증: 버스트 손실 모델의 실제 손실률과 단절 패킷 집계가 설정과 맞는지(짧게).
#[test]
fn long_sim_harness_models() {
    let mut np = NetPeer::new(240, 9);
    np.burst = Some((0.01, 0.5));
    np.outages = vec![(5 * SEC, 2 * SEC)];
    let out = run_multi(&MultiSim::new(20.0, vec![np]));
    let p = &out.peers[0];
    // 나쁨 상태 비율 = 0.01 / (0.01 + 0.5) ≈ 1.96%
    let loss = p.tx_lost as f64 / (p.tx_sent - p.tx_outage) as f64;
    assert!((0.012..0.03).contains(&loss), "버스트 손실률 {loss}");
    // 2초 × 200패킷/s
    assert!((390..=410).contains(&p.tx_outage), "단절 패킷 {}", p.tx_outage);
    // 단절 뒤 다시 재생한다.
    assert!(p.samples.iter().any(|s| s.t > 10.0 && s.playing));
    assert!(p.samples.iter().any(|s| (5.5..7.0).contains(&s.t) && !s.playing), "단절 중 재버퍼(재생 멈춤)");
}

