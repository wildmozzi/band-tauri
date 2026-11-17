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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    pub(crate) group: u16,
    pub(crate) number: u8,
    pub(crate) efficiency_class: u8,
    pub(crate) llc: u8,
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

static CORE_MAP: OnceLock<Option<CoreMap>> = OnceLock::new();

/// 2단계 실험 옵션(P1-21): 워커·수신 스레드의 EcoQoS(전원 조절)를 끈다(`ThreadPowerThrottling`, 실행 속도 조절 해제 = HighQoS).
/// **기본 꺼짐** — 기본값 변경은 실장비 근거를 본 뒤 결정한다(이 PR은 보고만). 엔진 시작 전에 정한다(스레드 시작 때 적용).
static HIGH_QOS: AtomicBool = AtomicBool::new(false);

/// 워커·수신 스레드 HighQoS(EcoQoS 끄기) 옵션. 다음에 시작하는 엔진부터 적용된다. 측정 도구(`tests/cpu_profile.rs`)가 켜서 비교한다.
/// 아직 앱 설정·커맨드에 연결하지 않은 실험 옵션이라 측정 빌드(`test-support`)에만 있다 — 제품 빌드에서는 늘 꺼져 있다.
#[cfg(any(test, feature = "test-support"))]
pub fn set_high_qos(enabled: bool) {
    HIGH_QOS.store(enabled, Ordering::Relaxed);
}

/// 지금 스레드에 HighQoS 옵션을 적용한다(옵션이 꺼져 있으면 아무것도 안 함). 스레드 시작 때 한 번, 루프 앞에서. 반환: 적용했는가.
pub(crate) fn apply_thread_qos() -> bool {
    if !HIGH_QOS.load(Ordering::Relaxed) {
        return false;
    }
    let ok = os::disable_power_throttling();
    if !ok {
        log::warn!("스레드 EcoQoS 끄기 실패(HighQoS 옵션)");
    }
    ok
}

/// 코어 표를 만든다(처음 한 번, 이후는 아무것도 안 함). 엔진 시작·워커/수신 스레드가 루프 **앞**에서 부른다 — 표본 함수는
/// 표가 없으면 세지 않을 뿐 만들지 않는다(반복 안 할당 0, 엔진 ⑧ 후속).
pub(crate) fn init() {
    CORE_MAP.get_or_init(|| {
        let map = CoreMap::from_entries(&os::cpu_sets());
        if let Some(m) = &map {
            let count = |k| m.kinds.iter().filter(|x| **x == Some(k)).count();
            log::info!(
                "코어 표: P {} · E {} · LP-E {}",
                count(CoreKind::Performance),
                count(CoreKind::Efficiency),
                count(CoreKind::LowPowerEfficiency)
            );
        }
        map
    });
}

fn map() -> Option<&'static CoreMap> {
    CORE_MAP.get().and_then(|m| m.as_ref())
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
                    out.push(CpuSetEntry { group: c.Group, number: c.LogicalProcessorIndex, efficiency_class: c.EfficiencyClass, llc: c.LastLevelCacheIndex });
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

    #[inline]
    pub(super) fn current_processor() -> Option<(u16, u8)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(number: u8, class: u8, llc: u8) -> CpuSetEntry {
        CpuSetEntry { group: 0, number, efficiency_class: class, llc }
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
