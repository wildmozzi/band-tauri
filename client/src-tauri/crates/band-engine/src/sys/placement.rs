//! 하이브리드 CPU(P/E/LP-E) 코어 배치 기록(P1-21 1단계, 근거: `docs/overhaul/cpu-quiet-2026-11-17.md` 4절 2번).
//!
//! 코어 고정 없이 실행하면 같은 코드가 워커 CPU 1.6~2배로 갈렸다(워커·수신 스레드가 느린 코어에 놓인 모양). 어느 스레드가
//! 어느 종류 코어에서 도는지 먼저 기록한다:
//! - 엔진 시작(제어 평면) 때 한 번 `GetSystemCpuSetInformation`으로 논리 프로세서 → 코어 종류 표를 만든다(`init`, 할당 있음).
//! - 워커 반복·수신 루프 깨어남마다 `GetCurrentProcessorNumberEx`(사용자 모드, 시스템콜·할당 없음)로 지금 코어를 읽어 종류별
//!   카운터(원자값)를 올린다(`PlacementCounts::sample`). **RT 콜백에서는 부르지 않는다.**
//! - 결과는 `EngineStats.corePlacement`(스레드별 표본 수·종류별 비율). 비Windows·표를 못 만든 기계는 `None`.
//!
//! 종류 판정(`classify`): 효율 등급(`EfficiencyClass`)이 가장 높은 것 = P. 나머지 중 P와 마지막 단계 캐시(LLC)를 함께 쓰는 것 = E,
//! 따로 쓰는 것 = LP-E(Intel Core Ultra의 저전력 섬 — 225H에서 LP 12·13, 효율 등급은 E와 같은 0). 등급이 하나뿐이면(하이브리드
//! 아님) 모두 P다. 단 LP-E는 **P와 LLC를 함께 쓰는 E 클러스터가 있거나, P와 따로 쓰는 섬이 둘 이상일 때만** 나온다 — 비P 코어가
//! P와 다른 LLC 섬 하나에만 있는 구성(예: Lunar Lake 4P + 4LP-E, P/E가 CCX로 갈린 AMD Zen5/Zen5c)은 그 섬을 E로 센다.
//! LLC·효율 등급만으로는 "저전력 섬"과 "따로 떨어진 E 클러스터"를 가를 수 없어서다(#191 리뷰 L3).
//!
//! 스레드 정책(P1-21 2단계): 워커·수신 스레드가 시작할 때 한 번(`apply_thread_policy`, 루프 앞) 아래 둘을 옵션에 따라 건다.
//! - HighQoS: `SetThreadInformation(ThreadPowerThrottling)`으로 실행 속도 전원 조절(EcoQoS)을 끈다. 스케줄러는 여전히 코어를
//!   자유롭게 고른다.
//! - P 코어 CPU set: `SetThreadSelectedCpuSets`로 효율 등급이 가장 높은 코어 집합을 고른다. **"선호"가 아니다** — 이 OS에서
//!   스레드는 고른 집합 안에 머문다(P 코어가 가득 차도 빈 E 코어로 넘어가지 않는다. 확인: PR #245 리뷰 H1, 바쁜 스레드 8개가
//!   P 4개에 묶여 P 비율 100%). affinity 마스크와 API가 다를 뿐 효과는 고정에 가깝다. 그래서 P 코어를 같거나 높은 우선순위
//!   부하(DAW 등 다른 MMCSS "Pro Audio" 스레드)가 채우면 정책이 없을 때보다 밀릴 수 있다 — 그 상황은 재지 않았다.
//!   하이브리드가 아니거나 물리 P 코어가 4개 미만이면 걸지 않는다(`skipped`).
//!
//! 둘 다 **기본 꺼짐**인 실험 옵션이다. 켜는 길은 둘: 엔진 설정 `EngineConfig.threadPolicy`(오디오 설정 "실험적" 토글)와
//! 환경변수 `BAND_THREAD_POLICY` = `off` | `highqos` | `pcores` | `both`(있으면 설정보다 우선 — 제품 빌드에서도 읽는다).
//! 측정과 기본값을 바꾸지 않은 이유: `docs/overhaul/core-placement-2026-11-20.md`. 실패는 무시하고 결과만
//! `EngineStats.threadPolicy`에 남긴다. RT 콜백 스레드에는 걸지 않는다.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::Serialize;

/// 코어 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreKind {
    Performance,
    Efficiency,
    LowPowerEfficiency,
}

/// `GetSystemCpuSetInformation` 한 항목에서 쓰는 값(시험이 OS 없이 만들 수 있게 따로 둔다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuSetEntry {
    /// CPU set ID(`SetThreadSelectedCpuSets`에 넘기는 값).
    pub(crate) id: u32,
    pub(crate) group: u16,
    pub(crate) number: u8,
    pub(crate) efficiency_class: u8,
    pub(crate) llc: u8,
    /// 물리 코어 번호(`CoreIndex` — 하이퍼스레딩 형제끼리 같다).
    pub(crate) core: u8,
    /// 다른 프로세스가 독점한 집합이다(`Allocated`이고 `AllocatedToTargetProcess`가 아님) — 골라도 쓸 수 없다.
    pub(crate) allocated_elsewhere: bool,
}

/// 항목마다 코어 종류(같은 순서).
/// - 효율 등급이 가장 높은 것 = P.
/// - 나머지 중 P와 LLC를 함께 쓰는 것 = E.
/// - P와 LLC를 함께 쓰지 않는 섬: E 클러스터가 따로 있거나(P LLC를 쓰는 비P가 있음) 그런 섬이 둘 이상이면 그중 가장 작은
///   섬 = LP-E, 나머지 = E. 섬이 하나뿐이고 P LLC를 쓰는 비P도 없으면 그 섬이 E 클러스터다.
pub(crate) fn classify(cpus: &[CpuSetEntry]) -> Vec<CoreKind> {
    let Some(top) = cpus.iter().map(|c| c.efficiency_class).max() else {
        return Vec::new();
    };
    let is_p = |c: &CpuSetEntry| c.efficiency_class == top;
    let p_llc: Vec<u8> = cpus.iter().filter(|c| is_p(c)).map(|c| c.llc).collect();
    let island_size = |llc: u8| cpus.iter().filter(|c| !is_p(c) && c.llc == llc).count();
    let mut islands: Vec<u8> = cpus.iter().filter(|c| !is_p(c) && !p_llc.contains(&c.llc)).map(|c| c.llc).collect();
    islands.sort_unstable();
    islands.dedup();
    let e_with_p = cpus.iter().any(|c| !is_p(c) && p_llc.contains(&c.llc));
    let smallest = islands.iter().map(|&l| island_size(l)).min().unwrap_or(0);
    let has_lpe = e_with_p || islands.len() >= 2;
    cpus.iter()
        .map(|c| {
            if is_p(c) {
                CoreKind::Performance
            } else if p_llc.contains(&c.llc) || !has_lpe {
                CoreKind::Efficiency
            } else if island_size(c.llc) == smallest && (e_with_p || islands.iter().any(|&l| island_size(l) > smallest)) {
                CoreKind::LowPowerEfficiency
            } else {
                CoreKind::Efficiency
            }
        })
        .collect()
}

/// 논리 프로세서(그룹, 번호) → 코어 종류. 엔진 시작 때 한 번 만든다.
#[derive(Debug)]
pub(crate) struct CoreMap {
    /// 그룹 × 64 + 번호 → 종류(없는 칸은 None).
    kinds: Vec<Option<CoreKind>>,
}

impl CoreMap {
    pub(crate) fn from_entries(cpus: &[CpuSetEntry]) -> Option<Self> {
        if cpus.is_empty() {
            return None;
        }
        let kinds = classify(cpus);
        let len = cpus.iter().map(|c| c.group as usize * 64 + c.number as usize + 1).max()?;
        let mut table = vec![None; len];
        for (c, k) in cpus.iter().zip(kinds) {
            table[c.group as usize * 64 + c.number as usize] = Some(k);
        }
        Some(Self { kinds: table })
    }

    /// 할당 없음.
    pub(crate) fn kind(&self, group: u16, number: u8) -> Option<CoreKind> {
        self.kinds.get(group as usize * 64 + number as usize).copied().flatten()
    }
}

/// 엔진 시작 때 한 번 만드는 표: 코어 종류 표(없으면 None)와 P 코어 CPU set ID(하이브리드가 아니면 빈 목록).
struct Tables {
    map: Option<CoreMap>,
    p_set_ids: Vec<u32>,
}

static TABLES: OnceLock<Tables> = OnceLock::new();

/// P 코어 CPU set을 거는 최소 **물리** P 코어 수(PR #245 리뷰 M1). 스레드가 고른 집합 안에 머물기 때문에, P가 적은 기계에서는
/// 워커·수신 스레드가 RT 콜백·다른 앱과 좁은 집합을 다투게 된다. 하이퍼스레딩이 있는 2P + 8E(i5-1235U 등)는 P 논리 프로세서가
/// 4개지만 물리 코어는 2개라 걸지 않는다. 측정 기계(225H)는 물리 P 4개다.
pub(crate) const MIN_P_CORES: usize = 4;

/// 효율 등급(`EfficiencyClass`)이 가장 높은 코어들의 CPU set ID(입력 순서). E·LP-E는 등급이 낮아 빠지고, 다른 프로세스가
/// 독점한 집합도 뺀다. 빈 목록이면 정책을 걸지 않는다(`skipped`): 등급이 하나뿐이거나(하이브리드 아님 — 고를 것이 없다) 고를 수
/// 있는 물리 P 코어가 `MIN_P_CORES`개 미만일 때. 주차(`Parked`) 여부는 보지 않는다 — 코어 주차는 실행 중에 바뀌는 값이다.
pub(crate) fn performance_set_ids(cpus: &[CpuSetEntry]) -> Vec<u32> {
    let (Some(top), Some(low)) = (cpus.iter().map(|c| c.efficiency_class).max(), cpus.iter().map(|c| c.efficiency_class).min()) else {
        return Vec::new();
    };
    if top == low {
        return Vec::new();
    }
    let usable: Vec<&CpuSetEntry> = cpus.iter().filter(|c| c.efficiency_class == top && !c.allocated_elsewhere).collect();
    let mut cores: Vec<(u16, u8)> = usable.iter().map(|c| (c.group, c.core)).collect();
    cores.sort_unstable();
    cores.dedup();
    if cores.len() < MIN_P_CORES {
        return Vec::new();
    }
    usable.iter().map(|c| c.id).collect()
}

/// 엔진 하나가 워커·수신 스레드에 걸 정책(P1-21 2단계). 기본은 둘 다 꺼짐 — 이때 스레드는 OS 호출 없이 이 PR 전과 똑같이 돈다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PolicyChoice {
    pub(crate) high_qos: bool,
    pub(crate) p_cores: bool,
}

/// 환경변수 `BAND_THREAD_POLICY` 이름(제품 빌드에서도 읽는다 — 설정 화면 없이 켜고 끄는 수단, PR #245 리뷰 H2).
pub(crate) const POLICY_ENV: &str = "BAND_THREAD_POLICY";

/// `BAND_THREAD_POLICY` 값 → 정책. `off` | `highqos` | `pcores` | `both`(대소문자·앞뒤 공백 무시). 측정 도구가 쓰던
/// `qos`·`qos+pcores`도 받는다. 모르는 값·빈 값은 None(설정을 따른다).
pub(crate) fn parse_policy(v: &str) -> Option<PolicyChoice> {
    let (high_qos, p_cores) = match v.trim().to_ascii_lowercase().as_str() {
        "off" => (false, false),
        "highqos" | "qos" => (true, false),
        "pcores" => (false, true),
        "both" | "qos+pcores" | "highqos+pcores" => (true, true),
        _ => return None,
    };
    Some(PolicyChoice { high_qos, p_cores })
}

/// 엔진 설정(`EngineConfig.threadPolicy`)과 환경변수로 정책을 정한다: 환경변수가 유효하면 그것이 이기고, 아니면 설정,
/// 설정도 없으면 둘 다 꺼짐. 엔진 시작(제어 평면)에서 한 번 부른다.
pub(crate) fn resolve_policy(config: PolicyChoice, env: Option<&str>) -> PolicyChoice {
    env.and_then(parse_policy).unwrap_or(config)
}

/// 엔진 시작 때의 정책: 설정 + 실제 환경변수. 환경변수가 설정을 바꿨으면 로그에 남긴다.
pub(crate) fn policy_for_engine(config: PolicyChoice) -> PolicyChoice {
    let env = std::env::var(POLICY_ENV).ok();
    let p = resolve_policy(config, env.as_deref());
    match env.as_deref() {
        Some(v) if parse_policy(v).is_none() && !v.trim().is_empty() => log::warn!("{POLICY_ENV}={v}: 모르는 값이라 설정을 따른다(off | highqos | pcores | both)"),
        Some(v) if p != config => log::info!("{POLICY_ENV}={v}: 엔진 설정의 스레드 정책을 덮어쓴다 → {p:?}"),
        _ => {}
    }
    p
}

/// 정책 한 항목의 적용 결과(`EngineStats.threadPolicy.*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum PolicyResult {
    /// 스레드가 아직 정책을 적용하기 전이다(시작 직후 잠깐).
    Pending,
    /// 옵션이 꺼져 있다(기본). OS 호출을 하지 않았다.
    Off,
    /// OS 호출 성공.
    Applied,
    /// OS 호출 실패(무시하고 기본 스케줄링으로 돈다).
    Failed,
    /// P 코어 CPU set만: 걸 집합이 없어 걸지 않았다(하이브리드 아님·표 없음·물리 P 코어 4개 미만).
    Skipped,
}

impl PolicyResult {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Off,
            2 => Self::Applied,
            3 => Self::Failed,
            4 => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

/// 스레드 하나에 적용한 정책(`EngineStats.threadPolicy.worker` / `.netRecv`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadPolicy {
    pub(crate) high_qos: PolicyResult,
    pub(crate) p_cores: PolicyResult,
}

/// `EngineStats.threadPolicy`: 워커와 네트워크 수신 스레드(네트워크 없이 시작했으면 None).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadPolicyStats {
    pub(crate) worker: ThreadPolicy,
    pub(crate) net_recv: Option<ThreadPolicy>,
}

/// 스레드 하나의 정책: 엔진이 스레드를 만들기 전에 정한 요청(`requested`)과, 그 스레드가 시작 때 한 번 적는 적용 결과(원자값).
#[derive(Debug, Default)]
pub(crate) struct PolicySlot {
    want_high_qos: AtomicBool,
    want_p_cores: AtomicBool,
    high_qos: AtomicU8,
    p_cores: AtomicU8,
}

impl PolicySlot {
    /// 이 정책을 걸 슬롯(스레드를 만들기 전에). `Default`는 둘 다 꺼짐이다.
    pub(crate) fn requested(choice: PolicyChoice) -> Self {
        Self { want_high_qos: AtomicBool::new(choice.high_qos), want_p_cores: AtomicBool::new(choice.p_cores), ..Default::default() }
    }

    fn choice(&self) -> PolicyChoice {
        PolicyChoice { high_qos: self.want_high_qos.load(Ordering::Relaxed), p_cores: self.want_p_cores.load(Ordering::Relaxed) }
    }

    fn store(&self, p: ThreadPolicy) {
        self.high_qos.store(p.high_qos as u8, Ordering::Relaxed);
        self.p_cores.store(p.p_cores as u8, Ordering::Relaxed);
    }

    /// 보고값. 비Windows는 None(정책이 Windows 전용).
    pub(crate) fn stats(&self) -> Option<ThreadPolicy> {
        cfg!(windows).then(|| ThreadPolicy {
            high_qos: PolicyResult::from_u8(self.high_qos.load(Ordering::Relaxed)),
            p_cores: PolicyResult::from_u8(self.p_cores.load(Ordering::Relaxed)),
        })
    }
}

/// 정책과 P 코어 목록으로 적용하고 결과를 낸다(OS 호출은 인자 — 시험이 실패 경로를 넣는다). 꺼진 항목은 OS를 부르지 않는다.
fn apply_with(choice: PolicyChoice, p_ids: &[u32], disable_throttling: impl FnOnce() -> bool, select_sets: impl FnOnce(&[u32]) -> bool) -> ThreadPolicy {
    let done = |ok: bool| if ok { PolicyResult::Applied } else { PolicyResult::Failed };
    ThreadPolicy {
        high_qos: if choice.high_qos { done(disable_throttling()) } else { PolicyResult::Off },
        p_cores: match (choice.p_cores, p_ids.is_empty()) {
            (false, _) => PolicyResult::Off,
            (true, true) => PolicyResult::Skipped,
            (true, false) => done(select_sets(p_ids)),
        },
    }
}

/// LP-E를 뺀 논리 프로세서(P+E)의 친화성 마스크(그룹 0). 그룹이 둘 이상이거나 표가 비면 None.
/// 하이브리드가 아니면 전부(= 고정해도 바뀌는 것 없음).
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn mask_without_lpe(cpus: &[CpuSetEntry]) -> Option<u64> {
    if cpus.is_empty() || cpus.iter().any(|c| c.group != 0 || c.number >= 64) {
        return None;
    }
    let mask = cpus.iter().zip(classify(cpus)).filter(|(_, k)| *k != CoreKind::LowPowerEfficiency).fold(0u64, |m, (c, _)| m | 1 << c.number);
    (mask != 0).then_some(mask)
}

/// 측정 도구용 코어 고정(P1-21b, ADR D6): 이 프로세스(이후 만드는 스레드 포함)를 친화성 마스크 `mask`(그룹 0)에 묶는다.
/// `None`이면 LP-E를 뺀 P+E(`mask_without_lpe`) — `cpu-quiet-2026-11-17.md`의 `start /affinity FFF`(225H)를 기계마다 자동으로.
/// 반환: 적용한 마스크. 제품 빌드에는 없다(측정 빌드 `test-support`).
#[cfg(any(test, feature = "test-support"))]
pub fn pin_process(mask: Option<u64>) -> Result<u64, String> {
    let mask = match mask {
        Some(m) => m,
        None => mask_without_lpe(&os::cpu_sets()).ok_or("코어 표를 만들 수 없다(비Windows·프로세서 그룹 둘 이상)")?,
    };
    os::set_process_affinity(mask)?;
    Ok(mask)
}

/// 지금 스레드에 `slot`이 요청한 정책을 적용하고 결과를 `slot`에 적는다. 스레드 시작 때 한 번, 루프 앞에서(시스템콜·로그
/// 있음 — RT 콜백에서 부르지 않는다). 둘 다 꺼져 있으면(기본) OS 호출이 없다 — 스레드는 이 정책이 없던 때와 똑같이 돈다.
/// 실패해도 스레드는 그대로 돈다(스레드당 한 번 경고 로그).
pub(crate) fn apply_thread_policy(slot: &PolicySlot) {
    init();
    // 정책은 Windows 전용이다 — 다른 OS에서는 걸지도, 실패로 적지도 않는다(보고는 null).
    if !cfg!(windows) {
        return;
    }
    let ids = TABLES.get().map(|t| t.p_set_ids.as_slice()).unwrap_or(&[]);
    let p = apply_with(slot.choice(), ids, os::disable_power_throttling, os::select_cpu_sets);
    if p.high_qos == PolicyResult::Failed || p.p_cores == PolicyResult::Failed {
        log::warn!("스레드 정책 적용 실패(무시): {p:?}");
    }
    slot.store(p);
}

/// 코어 표를 만든다(처음 한 번, 이후는 아무것도 안 함). 엔진 시작·워커/수신 스레드가 루프 **앞**에서 부른다 — 표본 함수는
/// 표가 없으면 세지 않을 뿐 만들지 않는다(반복 안 할당 0, 엔진 ⑧ 후속).
pub(crate) fn init() {
    TABLES.get_or_init(|| {
        let cpus = os::cpu_sets();
        let map = CoreMap::from_entries(&cpus);
        let p_set_ids = performance_set_ids(&cpus);
        if let Some(m) = &map {
            let count = |k| m.kinds.iter().filter(|x| **x == Some(k)).count();
            log::info!(
                "코어 표: P {} · E {} · LP-E {} (P 코어 CPU set {}개)",
                count(CoreKind::Performance),
                count(CoreKind::Efficiency),
                count(CoreKind::LowPowerEfficiency),
                p_set_ids.len()
            );
        }
        Tables { map, p_set_ids }
    });
}

fn map() -> Option<&'static CoreMap> {
    TABLES.get().and_then(|t| t.map.as_ref())
}

/// 이 기계의 코어 구성 한 줄(측정 도구 출력용): 종류별 논리 프로세서 수와 P 코어 CPU set 수(0이면 P 코어 CPU set 정책은 `skipped`).
#[cfg(any(test, feature = "test-support"))]
pub fn describe_cores() -> String {
    let cpus = os::cpu_sets();
    let kinds = classify(&cpus);
    let count = |k| kinds.iter().filter(|x| **x == k).count();
    let mut classes: Vec<u8> = cpus.iter().map(|c| c.efficiency_class).collect();
    classes.sort_unstable();
    classes.dedup();
    format!(
        "논리 {} = P {} · E {} · LP-E {} (효율 등급 {:?}, P 코어 CPU set {}개)",
        cpus.len(),
        count(CoreKind::Performance),
        count(CoreKind::Efficiency),
        count(CoreKind::LowPowerEfficiency),
        classes,
        performance_set_ids(&cpus).len()
    )
}

/// 스레드 하나의 코어 종류별 표본 수(원자값, 작성자는 그 스레드 하나).
#[derive(Debug, Default)]
pub(crate) struct PlacementCounts {
    p: AtomicU64,
    e: AtomicU64,
    lp_e: AtomicU64,
}

impl PlacementCounts {
    /// 지금 이 스레드가 도는 코어를 한 번 센다. 표가 없거나(비Windows·`init` 전) 모르는 코어면 세지 않는다. 할당·락 없음.
    #[inline]
    pub(crate) fn sample(&self) {
        let Some(m) = map() else { return };
        let Some((group, number)) = os::current_processor() else { return };
        self.add(m.kind(group, number));
    }

    fn add(&self, k: Option<CoreKind>) {
        let c = match k {
            Some(CoreKind::Performance) => &self.p,
            Some(CoreKind::Efficiency) => &self.e,
            Some(CoreKind::LowPowerEfficiency) => &self.lp_e,
            None => return,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// 보고값. 표가 없으면 None(비Windows).
    pub(crate) fn stats(&self) -> Option<ThreadPlacement> {
        map()?;
        Some(ThreadPlacement::from_counts(self.p.load(Ordering::Relaxed), self.e.load(Ordering::Relaxed), self.lp_e.load(Ordering::Relaxed)))
    }
}

/// 스레드 하나의 코어 배치(`EngineStats.corePlacement.*`): 시작부터 누적 표본 수와 종류별 비율(%).
///
/// 표본은 **깨어남 가중**이다(시간 가중 아님): 워커는 반복마다, 수신 스레드는 패킷마다 한 번 센다. 그래서 비율은
/// "깨어난 순간 그 종류 코어에 있던 빈도"이고 실행 시간 비율이 아니다 — 반복이 잦은 구간(촘촘한 폴링·패킷 몰림)의 코어로
/// 치우칠 수 있다. 실행 시간으로 읽으려면 같은 구간의 `cpu.workerPct`와 함께 본다(#191 리뷰 L2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadPlacement {
    pub(crate) samples: u64,
    pub(crate) p_pct: f32,
    pub(crate) e_pct: f32,
    pub(crate) lp_e_pct: f32,
    /// 원시 표본 수 [P, E, LP-E](누적). 계약 JSON에는 넣지 않는다 — 진단 타임라인의 구간 차분(`PlacementWindow`)에만 쓴다.
    #[serde(skip)]
    pub(crate) counts: [u64; 3],
}

impl ThreadPlacement {
    pub(crate) fn from_counts(p: u64, e: u64, lp_e: u64) -> Self {
        let n = p + e + lp_e;
        let pct = |x: u64| if n == 0 { 0.0 } else { (x as f64 * 100.0 / n as f64) as f32 };
        Self { samples: n, p_pct: pct(p), e_pct: pct(e), lp_e_pct: pct(lp_e), counts: [p, e, lp_e] }
    }
}

/// 진단 타임라인 샘플의 `corePlacement`(P1-21 후속, #191 리뷰 L1·L4): 스레드별 원시 표본 수 `[P, E, LP-E]`.
/// 기록기(`diag`)가 같은 엔진 세션의 직전 샘플과 차분해 **구간 값**(직전 샘플 뒤로 센 수)으로 넣는다 — 누적 비율은 긴 연주
/// 중간의 배치 변화를 희석하기 때문이다. 정수 배열이라 줄당 크기도 비율 객체보다 작다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlacementWindow {
    pub(crate) worker: [u64; 3],
    pub(crate) net_recv: Option<[u64; 3]>,
}

impl PlacementWindow {
    /// 엔진 시작부터 누적한 수.
    pub(crate) fn cumulative(s: &CorePlacementStats) -> Self {
        Self { worker: s.worker.counts, net_recv: s.net_recv.map(|r| r.counts) }
    }

    /// 두 구간의 합(진단 링 넘침 합치기·세션 누적, P1-21a). 수신 스레드는 한쪽에만 있으면 그쪽 값.
    pub(crate) fn plus(&self, o: &Self) -> Self {
        let a = |x: [u64; 3], y: [u64; 3]| [x[0] + y[0], x[1] + y[1], x[2] + y[2]];
        let net_recv = match (self.net_recv, o.net_recv) {
            (Some(x), Some(y)) => Some(a(x, y)),
            (x, y) => x.or(y),
        };
        Self { worker: a(self.worker, o.worker), net_recv }
    }

    /// `prev`(같은 세션의 직전 누적) 뒤로 늘어난 수. 카운터는 세션 안에서 줄지 않지만 0에서 멈춘다.
    pub(crate) fn since(&self, prev: &Self) -> Self {
        let d = |a: [u64; 3], b: [u64; 3]| [a[0].saturating_sub(b[0]), a[1].saturating_sub(b[1]), a[2].saturating_sub(b[2])];
        Self { worker: d(self.worker, prev.worker), net_recv: self.net_recv.map(|r| d(r, prev.net_recv.unwrap_or([0; 3]))) }
    }
}

/// `EngineStats.corePlacement`: 워커와 네트워크 수신 스레드(네트워크 없이 시작했으면 None).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CorePlacementStats {
    pub(crate) worker: ThreadPlacement,
    pub(crate) net_recv: Option<ThreadPlacement>,
}

#[cfg(windows)]
mod os {
    use super::CpuSetEntry;
    use windows_sys::Win32::System::Kernel::PROCESSOR_NUMBER;
    use windows_sys::Win32::System::SystemInformation::{CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION};
    use windows_sys::Win32::System::Threading::GetCurrentProcessorNumberEx;

    /// 시스템의 CPU set 목록(실패하면 빈 목록). 할당 있음 — 엔진 시작 때만.
    pub(super) fn cpu_sets() -> Vec<CpuSetEntry> {
        let mut len = 0u32;
        // SAFETY: 길이만 묻는 호출(버퍼 null, 길이 0). 실패(버퍼 부족)가 정상이고 `len`에 필요한 바이트가 온다.
        unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut len, std::ptr::null_mut(), 0) };
        if len == 0 {
            return Vec::new();
        }
        // 항목은 가변 길이(`Size`)라 바이트 버퍼로 받는다. u64 정렬을 위해 u64 벡터로 잡는다.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf`는 `len` 바이트 이상이다.
        let ok = unsafe { GetSystemCpuSetInformation(buf.as_mut_ptr().cast(), len, &mut len, std::ptr::null_mut(), 0) };
        if ok == 0 {
            return Vec::new();
        }
        let bytes = len as usize;
        let base = buf.as_ptr().cast::<u8>();
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + std::mem::size_of::<u32>() * 2 <= bytes {
            // SAFETY: `off`는 OS가 채운 `bytes` 안의 항목 시작이다. 머리(Size·Type)를 먼저 읽고, 전체 구조체가 들어갈 때만 본문을 읽는다.
            let p = unsafe { base.add(off) }.cast::<SYSTEM_CPU_SET_INFORMATION>();
            let size = unsafe { std::ptr::addr_of!((*p).Size).read_unaligned() } as usize;
            if size == 0 {
                break;
            }
            if off + std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>() <= bytes {
                // SAFETY: 위에서 크기를 확인했다. `Type == CpuSetInformation`이면 공용체의 `CpuSet`이 유효하다.
                let info = unsafe { p.read_unaligned() };
                if info.Type == CpuSetInformation {
                    let c = unsafe { info.Anonymous.CpuSet };
                    // SAFETY: 플래그 공용체는 어느 쪽으로 읽어도 유효한 u8이다. 비트 1 = Allocated, 비트 2 = AllocatedToTargetProcess.
                    let flags = unsafe { c.Anonymous1.AllFlags };
                    out.push(CpuSetEntry {
                        id: c.Id,
                        group: c.Group,
                        number: c.LogicalProcessorIndex,
                        efficiency_class: c.EfficiencyClass,
                        llc: c.LastLevelCacheIndex,
                        core: c.CoreIndex,
                        allocated_elsewhere: flags & 0b10 != 0 && flags & 0b100 == 0,
                    });
                }
            }
            off += size;
        }
        out
    }

    /// 지금 스레드의 실행 속도 전원 조절(EcoQoS)을 끈다. 성공하면 true.
    pub(super) fn disable_power_throttling() -> bool {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadInformation, ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE};
        // ControlMask에 실행 속도 비트를 켜고 StateMask에서 끈다 = "이 스레드는 전원 조절하지 않음"(HighQoS).
        let st = THREAD_POWER_THROTTLING_STATE { Version: THREAD_POWER_THROTTLING_CURRENT_VERSION, ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED, StateMask: 0 };
        // SAFETY: 현재 스레드 의사 핸들과 크기가 맞는 입력 구조체.
        unsafe { SetThreadInformation(GetCurrentThread(), ThreadPowerThrottling, std::ptr::addr_of!(st).cast(), std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32) != 0 }
    }

    /// 지금 스레드가 고른 CPU set을 `ids`로 정한다. 스레드는 그 집합 안에 머문다(affinity 마스크와 API만 다르다). 성공하면 true.
    pub(super) fn select_cpu_sets(ids: &[u32]) -> bool {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadSelectedCpuSets};
        // SAFETY: 현재 스레드 의사 핸들과 `ids.len()`개의 유효한 u32 배열.
        unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), ids.as_ptr(), ids.len() as u32) != 0 }
    }

    /// 프로세스 친화성 마스크(측정 도구용 코어 고정).
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn set_process_affinity(mask: u64) -> Result<(), String> {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessAffinityMask};
        // SAFETY: 현재 프로세스 의사 핸들과 마스크 값 하나.
        if unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask as usize) } != 0 {
            Ok(())
        } else {
            Err(format!("SetProcessAffinityMask(0x{mask:X}) 실패: {}", std::io::Error::last_os_error()))
        }
    }

    /// 지금 스레드가 도는 논리 프로세서(그룹, 번호). 사용자 모드 읽기(시스템콜·할당 없음).
    #[inline]
    pub(super) fn current_processor() -> Option<(u16, u8)> {
        let mut n = PROCESSOR_NUMBER { Group: 0, Number: 0, Reserved: 0 };
        // SAFETY: 출력 구조체 하나를 채우는 호출.
        unsafe { GetCurrentProcessorNumberEx(&mut n) };
        Some((n.Group, n.Number))
    }
}

#[cfg(not(windows))]
mod os {
    use super::CpuSetEntry;

    pub(super) fn cpu_sets() -> Vec<CpuSetEntry> {
        Vec::new()
    }

    pub(super) fn disable_power_throttling() -> bool {
        false
    }

    pub(super) fn select_cpu_sets(_ids: &[u32]) -> bool {
        false
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn set_process_affinity(_mask: u64) -> Result<(), String> {
        Err("코어 고정은 Windows만 지원한다(taskset 등을 쓸 것)".into())
    }

    #[inline]
    pub(super) fn current_processor() -> Option<(u16, u8)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(number: u8, class: u8, llc: u8) -> CpuSetEntry {
        CpuSetEntry { id: 0x100 + number as u32, group: 0, number, efficiency_class: class, llc, core: number, allocated_elsewhere: false }
    }

    /// Intel Core Ultra 5 225H(측정 기계, `cpu-quiet-2026-11-17.md` 1절): P = LP 0·1·10·11(등급 1), E = LP 2~9(LLC 공유), LP-E = 12·13(별도 LLC).
    fn core_ultra_225h() -> Vec<CpuSetEntry> {
        let mut v: Vec<CpuSetEntry> = (0..14u8).map(|n| e(n, 0, 0)).collect();
        for n in [0, 1, 10, 11] {
            v[n as usize].efficiency_class = 1;
        }
        v[12].llc = 1;
        v[13].llc = 1;
        v
    }

    #[test]
    fn classifies_core_ultra_p_e_lpe() {
        let k = classify(&core_ultra_225h());
        use CoreKind::*;
        let want = [Performance, Performance, Efficiency, Efficiency, Efficiency, Efficiency, Efficiency, Efficiency, Efficiency, Efficiency, Performance, Performance, LowPowerEfficiency, LowPowerEfficiency];
        assert_eq!(k, want);
    }

    /// 코어 고정 마스크(P1-21b): 225H는 LP-E 12·13을 뺀 0xFFF(`cpu-quiet-2026-11-17.md`의 `start /affinity FFF`와 같다).
    #[test]
    fn affinity_mask_excludes_lpe() {
        assert_eq!(mask_without_lpe(&core_ultra_225h()), Some(0xFFF));
        let sym: Vec<_> = (0..8u8).map(|n| e(n, 0, 0)).collect();
        assert_eq!(mask_without_lpe(&sym), Some(0xFF), "하이브리드 아님 → 전부");
        assert_eq!(mask_without_lpe(&[]), None);
        let mut two_groups = sym.clone();
        two_groups[7].group = 1;
        assert_eq!(mask_without_lpe(&two_groups), None, "그룹 둘 이상은 마스크로 고정 불가");
    }

    /// P 코어 CPU set(P1-21 2단계): 효율 등급이 가장 높은 코어의 CPU set ID만. 하이브리드가 아니면 빈 목록.
    #[test]
    fn performance_set_is_top_efficiency_class_only() {
        // 225H: P = LP 0·1·10·11 → ID 0x100·0x101·0x10A·0x10B. E(2~9)·LP-E(12·13)는 빠진다.
        assert_eq!(performance_set_ids(&core_ultra_225h()), vec![0x100, 0x101, 0x10A, 0x10B]);
        // 대칭 CPU·빈 표: 고를 것이 없다
        let sym: Vec<_> = (0..8u8).map(|n| e(n, 0, 0)).collect();
        assert!(performance_set_ids(&sym).is_empty());
        assert!(performance_set_ids(&[]).is_empty());
        // 등급이 셋(0·1·2)이면 가장 높은 것만
        let three: Vec<_> = (0..12u8).map(|n| e(n, n / 4, 0)).collect();
        assert_eq!(performance_set_ids(&three), vec![0x108, 0x109, 0x10A, 0x10B]);
        // 프로세서 그룹이 달라도 ID는 시스템 전체 값이라 그대로 고른다(물리 코어는 (그룹, 코어)로 센다)
        let mut groups = core_ultra_225h();
        groups[10].group = 1;
        assert_eq!(performance_set_ids(&groups).len(), 4);
    }

    /// 문턱은 **물리** P 코어 수다(PR #245 리뷰 M1): 하이퍼스레딩 2P4T + 8E는 P 논리 프로세서가 4개여도 걸지 않는다.
    #[test]
    fn performance_set_needs_four_physical_p_cores() {
        // 2P(HT 없음) + 8E
        let two_p: Vec<_> = (0..10u8).map(|n| e(n, u8::from(n < 2), 0)).collect();
        assert!(performance_set_ids(&two_p).is_empty());
        // 2P4T + 8E(i5-1235U형): 논리 0·1 = 코어 0, 논리 2·3 = 코어 1, E는 코어당 논리 하나
        let mut ht: Vec<_> = (0..12u8).map(|n| e(n, u8::from(n < 4), 0)).collect();
        for c in ht.iter_mut() {
            c.core = if c.number < 4 { c.number / 2 } else { c.number - 2 };
        }
        assert!(performance_set_ids(&ht).is_empty(), "물리 P 코어 2개");
        // 4P8T + 8E(i7-12700H형 축소): 물리 4개 → 논리 8개를 모두 고른다
        let mut four: Vec<_> = (0..16u8).map(|n| e(n, u8::from(n < 8), 0)).collect();
        for c in four.iter_mut() {
            c.core = if c.number < 8 { c.number / 2 } else { c.number - 4 };
        }
        assert_eq!(performance_set_ids(&four).len(), 8);
        // 4P(HT 없음) + 8E: 딱 문턱
        let four_p: Vec<_> = (0..12u8).map(|n| e(n, u8::from(n < MIN_P_CORES as u8), 0)).collect();
        assert_eq!(performance_set_ids(&four_p).len(), MIN_P_CORES);
        // 다른 프로세스가 독점한 P 코어는 고르지 않고, 남은 물리 코어가 문턱 아래면 걸지 않는다(리뷰 L2)
        let mut taken = core_ultra_225h();
        taken[0].allocated_elsewhere = true;
        assert!(performance_set_ids(&taken).is_empty(), "쓸 수 있는 물리 P 코어 3개");
        let mut five: Vec<_> = (0..13u8).map(|n| e(n, u8::from(n < 5), 0)).collect();
        five[4].allocated_elsewhere = true;
        assert_eq!(performance_set_ids(&five), vec![0x100, 0x101, 0x102, 0x103]);
    }

    /// 환경변수 값과 우선순위(PR #245 리뷰 H2): 유효한 환경변수 > 엔진 설정 > 꺼짐.
    #[test]
    fn policy_env_parses_and_overrides_config() {
        let c = |high_qos, p_cores| PolicyChoice { high_qos, p_cores };
        assert_eq!(parse_policy("off"), Some(c(false, false)));
        assert_eq!(parse_policy(" HighQoS "), Some(c(true, false)));
        assert_eq!(parse_policy("pcores"), Some(c(false, true)));
        assert_eq!(parse_policy("both"), Some(c(true, true)));
        // 측정 도구의 옛 이름
        assert_eq!(parse_policy("qos"), Some(c(true, false)));
        assert_eq!(parse_policy("qos+pcores"), Some(c(true, true)));
        for bad in ["", "on", "1", "default", "p"] {
            assert_eq!(parse_policy(bad), None, "{bad}");
        }
        // 기본(설정 없음·환경변수 없음)은 둘 다 꺼짐
        assert_eq!(resolve_policy(PolicyChoice::default(), None), c(false, false));
        assert_eq!(resolve_policy(c(true, false), None), c(true, false));
        assert_eq!(resolve_policy(c(true, true), Some("off")), c(false, false), "환경변수로 끈다");
        assert_eq!(resolve_policy(c(false, false), Some("both")), c(true, true), "환경변수로 켠다");
        assert_eq!(resolve_policy(c(true, false), Some("이상한 값")), c(true, false), "모르는 값은 설정을 따른다");
    }

    /// 정책 적용 결과: 꺼짐이면 OS 호출 없음, 걸 집합이 없으면 P 코어 CPU set은 걸지 않음, 실패는 `failed`로만 남는다.
    #[test]
    fn policy_results_cover_off_applied_failed_skipped() {
        use PolicyResult::*;
        let c = |high_qos, p_cores| PolicyChoice { high_qos, p_cores };
        let never = || -> bool { panic!("꺼진 옵션은 OS를 부르지 않는다") };
        let never_set = |_: &[u32]| -> bool { panic!("꺼졌거나 걸 집합이 없으면 CPU set을 부르지 않는다") };
        // 기본(둘 다 꺼짐): OS 호출이 하나도 없다 — 이 PR 전과 같은 경로
        assert_eq!(apply_with(PolicyChoice::default(), &[1, 2], never, never_set), ThreadPolicy { high_qos: Off, p_cores: Off });
        assert_eq!(apply_with(c(true, true), &[], || true, never_set), ThreadPolicy { high_qos: Applied, p_cores: Skipped });
        let mut got = Vec::new();
        let p = apply_with(c(false, true), &[7, 9], never, |ids| {
            got = ids.to_vec();
            true
        });
        assert_eq!((p, got), (ThreadPolicy { high_qos: Off, p_cores: Applied }, vec![7, 9]));
        assert_eq!(apply_with(c(true, true), &[7], || false, |_| false), ThreadPolicy { high_qos: Failed, p_cores: Failed });

        // 슬롯: 적용 전은 pending, 요청은 만들 때 정해진다. 계약 JSON
        let slot = PolicySlot::requested(c(true, false));
        assert_eq!(slot.choice(), c(true, false));
        assert_eq!(PolicySlot::default().choice(), PolicyChoice::default());
        if cfg!(windows) {
            assert_eq!(slot.stats().unwrap(), ThreadPolicy { high_qos: Pending, p_cores: Pending });
            slot.store(ThreadPolicy { high_qos: Applied, p_cores: Skipped });
            let s = slot.stats().unwrap();
            assert_eq!(s, ThreadPolicy { high_qos: Applied, p_cores: Skipped });
            assert_eq!(serde_json::to_string(&s).unwrap(), r#"{"highQos":"applied","pCores":"skipped"}"#);
            assert_eq!(serde_json::to_string(&Pending).unwrap(), r#""pending""#);
        } else {
            assert!(slot.stats().is_none());
        }
    }

    /// 기본 슬롯(둘 다 꺼짐)으로 실제 적용 함수를 부르면 결과는 `off`·`off`다(OS 호출 없이 — 위 시험이 호출 없음을 고정한다).
    #[test]
    fn default_slot_applies_nothing() {
        std::thread::spawn(|| {
            let slot = PolicySlot::default();
            apply_thread_policy(&slot);
            if cfg!(windows) {
                assert_eq!(slot.stats().unwrap(), ThreadPolicy { high_qos: PolicyResult::Off, p_cores: PolicyResult::Off });
            } else {
                assert!(slot.stats().is_none());
            }
        })
        .join()
        .unwrap();
    }

    /// 실제 OS(Windows): 새 스레드에 두 정책을 걸면 HighQoS는 성공하고, P 코어 CPU set은 걸 집합이 있으면 성공·없으면 `skipped`다.
    /// 시험 전용 스레드에서만 건다(스레드가 끝나면 사라진다).
    #[test]
    fn os_policy_applies_on_this_thread() {
        std::thread::spawn(|| {
            init();
            let ids = &TABLES.get().unwrap().p_set_ids;
            let p = apply_with(PolicyChoice { high_qos: true, p_cores: true }, ids, os::disable_power_throttling, os::select_cpu_sets);
            if cfg!(windows) {
                assert_eq!(p.high_qos, PolicyResult::Applied);
                assert_eq!(p.p_cores, if ids.is_empty() { PolicyResult::Skipped } else { PolicyResult::Applied });
            } else {
                assert_eq!(p, ThreadPolicy { high_qos: PolicyResult::Failed, p_cores: PolicyResult::Skipped });
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn non_hybrid_is_all_p_and_hybrid_without_island_has_no_lpe() {
        // 대칭 CPU: 등급 하나 → 모두 P
        let sym: Vec<_> = (0..8u8).map(|n| e(n, 0, 0)).collect();
        assert!(classify(&sym).iter().all(|k| *k == CoreKind::Performance));
        // 12세대(Alder Lake)형: P(등급 1)·E(등급 0), E 클러스터가 P와 LLC를 함께 쓴다 → LP-E 없음
        let adl: Vec<_> = (0..12u8).map(|n| e(n, u8::from(n < 4), 0)).collect();
        let k = classify(&adl);
        assert_eq!(k.iter().filter(|k| **k == CoreKind::Performance).count(), 4);
        assert_eq!(k.iter().filter(|k| **k == CoreKind::Efficiency).count(), 8);
        // 비P 코어가 P와 다른 LLC 하나에만 있으면(섬이 하나) 그것은 E 클러스터다
        let one_island: Vec<_> = (0..8u8).map(|n| e(n, u8::from(n < 2), u8::from(n >= 2))).collect();
        assert!(!classify(&one_island).contains(&CoreKind::LowPowerEfficiency));
        assert!(classify(&[]).is_empty());
    }

    #[test]
    fn map_lookup_and_counts() {
        let m = CoreMap::from_entries(&core_ultra_225h()).unwrap();
        assert_eq!(m.kind(0, 0), Some(CoreKind::Performance));
        assert_eq!(m.kind(0, 5), Some(CoreKind::Efficiency));
        assert_eq!(m.kind(0, 13), Some(CoreKind::LowPowerEfficiency));
        assert_eq!(m.kind(0, 14), None);
        assert_eq!(m.kind(1, 0), None);
        assert!(CoreMap::from_entries(&[]).is_none());

        let c = PlacementCounts::default();
        for k in [m.kind(0, 0), m.kind(0, 1), m.kind(0, 5), m.kind(0, 12), m.kind(0, 99)] {
            c.add(k);
        }
        let s = ThreadPlacement::from_counts(c.p.load(Ordering::Relaxed), c.e.load(Ordering::Relaxed), c.lp_e.load(Ordering::Relaxed));
        assert_eq!(s, ThreadPlacement { samples: 4, p_pct: 50.0, e_pct: 25.0, lp_e_pct: 25.0, counts: [2, 1, 1] });
        // 원시 수는 계약 JSON에 나가지 않는다
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("counts"), "{j}");
        assert_eq!(ThreadPlacement::from_counts(0, 0, 0).p_pct, 0.0);
    }

    /// 실제 OS(Windows): 표를 만들고 이 스레드 표본이 하나 이상 세진다. 비Windows는 표가 없어 None.
    #[test]
    fn os_table_and_sample() {
        init();
        let c = PlacementCounts::default();
        for _ in 0..10 {
            c.sample();
        }
        if cfg!(windows) {
            let s = c.stats().expect("Windows는 코어 표가 있다");
            assert_eq!(s.samples, 10);
            assert!((s.p_pct + s.e_pct + s.lp_e_pct - 100.0).abs() < 0.01);
        } else {
            assert!(c.stats().is_none());
        }
    }
}
