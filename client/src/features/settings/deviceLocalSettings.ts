/**
 * 기기별 오디오 설정 — 계정(profiles.audio_settings)으로 동기화하지 않고 이 PC의 localStorage에만 둔다.
 *
 * 1) 엔진 스레드 정책(`EngineConfig.threadPolicy`, P1-21 2단계 실험 옵션). CPU 구성(하이브리드 여부,
 *    P 코어 수)에 따라 의미가 달라서 다른 PC에 켜진 채 내려오면 안 된다(#245 재리뷰 L4).
 *    이 키가 없으면(새 PC, 이 변경 전 설정) 둘 다 끔이다 — 옛 로컬 캐시(`band_engine_config_v2`)의 값은
 *    계정에서 내려받아 채워졌을 수 있어(다른 PC의 값) 옮기지 않는다.
 * 2) 장치 쪽 값(드라이버·장치 id·입력 채널·버퍼 크기, `DEVICE_LOCAL_KEYS` — P2-27 후속, 사용자 결정 2026-11-21).
 *    이 키가 없을 때만(이 변경 전 설정, 새 PC) 한 번 초기값을 가져온다 — 계정을 읽기 전에는 옛 로컬 캐시의 값으로 임시로 쓰고,
 *    계정을 읽으면 계정에 남은 옛 장치 값(없으면 지금 값)을 이 PC 값으로 저장한다(AudioSettingsContext). 그 뒤로는
 *    계정 값으로 덮어쓰지 않는다 — 같은 PC에서 계정을 바꿔 로그인해도 장치 설정은 그대로다(PC의 것).
 */
import type { EngineConfig, ThreadPolicyConfig } from '../../native/engineApi'
import {
  DEFAULT_ENGINE_CONFIG,
  STORED_SETTINGS_VERSION,
  normalizeThreadPolicy,
  parseStoredSettings,
  pickDeviceSettings,
  type DeviceSettings,
} from '../../native/engineConfig'

export const THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v1'
export const DEVICE_SETTINGS_STORAGE_KEY = 'band_device_settings_v1'

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

/**
 * 이 PC에 저장한 장치 쪽 값. 없거나(새 PC·이 변경 전) 손상됐으면 null — 부르는 쪽이 초기값을 정한다.
 * 값 검사는 계정 형식과 같은 정리(`parseStoredSettings`)를 거친다(모르는 드라이버 → 기본, 버퍼 범위 등).
 */
export function loadDeviceSettings(): DeviceSettings | null {
  try {
    const raw = localStorage.getItem(DEVICE_SETTINGS_STORAGE_KEY)
    if (!raw) return null
    const obj: unknown = JSON.parse(raw)
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return null
    const { inputChannels } = obj as { inputChannels?: unknown }
    // 입력 채널 개수는 계정의 channels에 맞춰 합치므로(withDeviceSettings) 여기서는 저장된 개수를 그대로 읽는다
    const channels = Array.isArray(inputChannels) && inputChannels.length > 1 ? 2 : 1
    return pickDeviceSettings(parseStoredSettings({ version: STORED_SETTINGS_VERSION, engine: { ...(obj as object), channels } }).config)
  } catch {
    return null
  }
}

export function hasDeviceSettings(): boolean {
  return loadDeviceSettings() !== null
}

export function saveDeviceSettings(config: EngineConfig | DeviceSettings): void {
  try {
    localStorage.setItem(DEVICE_SETTINGS_STORAGE_KEY, JSON.stringify(pickDeviceSettings({ ...DEFAULT_ENGINE_CONFIG, ...config })))
  } catch {
    // 무시
  }
}
