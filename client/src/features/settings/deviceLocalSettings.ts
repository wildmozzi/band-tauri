/**
 * 기기별 오디오 설정 — 계정(profiles.audio_settings)으로 동기화하지 않고 이 PC의 localStorage에만 둔다.
 *
 * 지금은 엔진 스레드 정책(`EngineConfig.threadPolicy`, P1-21 2단계 실험 옵션) 하나다. CPU 구성(하이브리드 여부,
 * P 코어 수)에 따라 의미가 달라서 다른 PC에 켜진 채 내려오면 안 된다(#245 재리뷰 L4).
 * 이 키가 없으면(새 PC, 이 변경 전 설정) 둘 다 끔이다 — 옛 로컬 캐시(`band_engine_config_v2`)의 값은
 * 계정에서 내려받아 채워졌을 수 있어(다른 PC의 값) 옮기지 않는다.
 */
import type { ThreadPolicyConfig } from '../../native/engineApi'
import { normalizeThreadPolicy } from '../../native/engineConfig'

export const THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v1'

export function loadDeviceThreadPolicy(): ThreadPolicyConfig {
  try {
    const raw = localStorage.getItem(THREAD_POLICY_STORAGE_KEY)
    if (raw) return normalizeThreadPolicy(JSON.parse(raw))
  } catch {
    // 저장소 접근 불가/손상 → 끔
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
