/**
 * 기기별 오디오 설정 — 계정(profiles.audio_settings)으로 동기화하지 않고 이 PC의 localStorage에만 둔다.
 *
 * 1) 엔진 스레드 정책(`EngineConfig.threadPolicy`, P1-21). CPU 구성(하이브리드 여부, P 코어 수)에 따라 의미가 달라서
 *    다른 PC의 값이 내려오면 안 된다(#245 재리뷰 L4).
 *    저장값이 없으면(새 PC) 기본값이다: 절전 끄기(HighQoS) **켬**, P 코어 묶기 끔(P1-21 3단계). 옛 로컬 캐시
 *    (`band_engine_config_v2`)의 값은 계정에서 내려받아 채워졌을 수 있어(다른 PC의 값) 옮기지 않는다.
 *
 *    키 버전(v1 → v2, P1-21 3단계): v1은 HighQoS 기본값이 끔이던 때의 값이고, 설정을 저장할 때마다 토글을 건드리지 않아도
 *    `highQos: false`가 함께 적혔다 — "사용자가 끈 것"과 "옛 기본값"을 가를 수 없다. 그래서 v1의 `highQos: false`는
 *    **저장값 없음**으로 보고 새 기본값(켬)을 쓴다. v1의 `highQos: true`와 `preferPCores`는 사용자가 고른 값이라 옮긴다.
 *    v2부터는 기본값이 켬이므로 `highQos: false`는 사용자가 명시적으로 끈 값이고 그대로 존중한다.
 * 2) 장치 쪽 값(드라이버·장치 id·입력 채널·버퍼 크기, `DEVICE_LOCAL_KEYS` — P2-27 후속, 사용자 결정 2026-11-21).
 *    이 키가 없을 때만(이 변경 전 설정, 새 PC) 한 번 초기값을 가져온다 — 옛 로컬 캐시의 장치 값(이 PC에서 마지막으로 쓴 값)이
 *    있으면 그것을, 없으면(새로 설치한 PC) 계정에 남은 옛 장치 값(그것도 없으면 지금 값)을 계정을 읽을 때 이 PC 값으로 저장한다(AudioSettingsContext). 그 뒤로는
 *    계정 값으로 덮어쓰지 않는다 — 같은 PC에서 계정을 바꿔 로그인해도 장치 설정은 그대로다(PC의 것).
 */
import type { EngineConfig, ThreadPolicyConfig } from '../../native/engineApi'
import {
  DEFAULT_ENGINE_CONFIG,
  DEVICE_LOCAL_KEYS,
  STORED_SETTINGS_VERSION,
  normalizeThreadPolicy,
  parseStoredSettings,
  pickDeviceSettings,
  type DeviceSettings,
} from '../../native/engineConfig'

export const THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v2'
/** HighQoS 기본값이 끔이던 때의 키(#249). 읽기만 한다(위 설명) */
export const LEGACY_THREAD_POLICY_STORAGE_KEY = 'band_thread_policy_v1'
export const DEVICE_SETTINGS_STORAGE_KEY = 'band_device_settings_v1'

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
    // 장치 필드가 하나도 없으면(`{}` 등 손상) 저장값 없음으로 본다 → 이행 경로로 돌아간다(#256 리뷰 L1)
    if (!DEVICE_LOCAL_KEYS.some((k) => k in obj)) return null
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
