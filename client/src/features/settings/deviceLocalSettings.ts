/**
 * 기기별 오디오 설정 — 계정(profiles.audio_settings)으로 동기화하지 않고 이 PC의 localStorage에만 둔다.
 *
 * 지금은 엔진 스레드 정책(`EngineConfig.threadPolicy`, P1-21) 하나다. CPU 구성(하이브리드 여부,
 * P 코어 수)에 따라 의미가 달라서 다른 PC의 값이 내려오면 안 된다(#245 재리뷰 L4).
 * 저장값이 없으면(새 PC) 기본값이다: 절전 끄기(HighQoS) **켬**, P 코어 묶기 끔(P1-21 3단계). 옛 로컬 캐시
 * (`band_engine_config_v2`)의 값은 계정에서 내려받아 채워졌을 수 있어(다른 PC의 값) 옮기지 않는다.
 *
 * 키 버전(v1 → v2, P1-21 3단계): v1은 HighQoS 기본값이 끔이던 때의 값이고, 설정을 저장할 때마다 토글을 건드리지 않아도
 * `highQos: false`가 함께 적혔다 — "사용자가 끈 것"과 "옛 기본값"을 가를 수 없다. 그래서 v1의 `highQos: false`는 **저장값 없음**
 * 으로 보고 새 기본값(켬)을 쓴다. v1의 `highQos: true`와 `preferPCores`는 사용자가 고른 값이라 옮긴다.
 * v2부터는 기본값이 켬이므로 `highQos: false`는 사용자가 명시적으로 끈 값이고 그대로 존중한다.
 */
import type { ThreadPolicyConfig } from '../../native/engineApi'
import { normalizeThreadPolicy } from '../../native/engineConfig'

export const THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v2'
/** HighQoS 기본값이 끔이던 때의 키(#249). 읽기만 한다(위 설명) */
export const LEGACY_THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v1'

export function loadDeviceThreadPolicy(): ThreadPolicyConfig {
  try {
    const raw = localStorage.getItem(THREAD_POLICY_STORAGE_KEY)
    if (raw) return normalizeThreadPolicy(JSON.parse(raw))
    const legacy = localStorage.getItem(LEGACY_THREAD_POLICY_STORAGE_KEY)
    if (legacy) {
      const v1 = normalizeThreadPolicy(JSON.parse(legacy))
      // v1의 highQos: false는 옛 기본값과 구별되지 않는다 → 새 기본값(켬)
      return { ...v1, highQos: true }
    }
  } catch {
    // 저장소 접근 불가/손상 → 기본값
  }
  return normalizeThreadPolicy(null)
}

export function saveDeviceThreadPolicy(policy: ThreadPolicyConfig | undefined): void {
  try {
    localStorage.setItem(THREAD_POLICY_STORAGE_KEY, JSON.stringify(normalizeThreadPolicy(policy)))
  } catch {
    // 무시
  }
}
