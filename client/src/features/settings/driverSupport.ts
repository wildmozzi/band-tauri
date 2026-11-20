/**
 * 이 배포본이 지원하는 드라이버에 따른 설정 화면 판단(P2-7, 순수 함수).
 * `supported`가 null이면 엔진이 목록을 주지 않은 것(옛 엔진) — 예전처럼 모두 지원으로 본다.
 */
import type { DriverKind, EngineConfig } from '../../native/engineApi'
import { DRIVER_OPTIONS } from '../../native/engineConfig'

/** 지원 안 하는 드라이버를 대신할 순서(독점 → 공유) */
const FALLBACK_ORDER: DriverKind[] = ['wasapiExclusive', 'wasapiShared', 'asio']

export function isDriverSupported(supported: readonly DriverKind[] | null, driver: DriverKind): boolean {
  return supported === null || supported.includes(driver)
}

/** 받침 있으면 이, 없으면(라틴 문자 포함) 가 */
function subjectJosa(word: string): string {
  const c = word.charCodeAt(word.length - 1)
  return c >= 0xac00 && c <= 0xd7a3 && (c - 0xac00) % 28 !== 0 ? '이' : '가'
}

export function driverLabel(driver: DriverKind): string {
  return DRIVER_OPTIONS.find((d) => d.id === driver)?.label ?? driver
}

/** 선택지 비활성화 이유(짧게). 지원하면 null */
export function unsupportedDriverReason(supported: readonly DriverKind[] | null, driver: DriverKind): string | null {
  if (isDriverSupported(supported, driver)) return null
  const alt = fallbackDriver(supported)
  const tail = alt && alt !== driver ? ` — ${driverLabel(alt)} 모드를 쓰세요` : ''
  const name = driverLabel(driver)
  return `이 배포본에는 ${name}${subjectJosa(name)} 포함되어 있지 않습니다${tail}`
}

/** 지원 드라이버 중 대신 쓸 것. 지원 목록이 비었으면 null */
export function fallbackDriver(supported: readonly DriverKind[] | null): DriverKind | null {
  return FALLBACK_ORDER.find((d) => isDriverSupported(supported, d)) ?? null
}

/** 설정 화면 드라이버 선택지: 지원 안 하면 disabled + 이유 */
export function driverChoices(supported: readonly DriverKind[] | null) {
  return DRIVER_OPTIONS.map((d) => ({ ...d, disabled: !isDriverSupported(supported, d.id), reason: unsupportedDriverReason(supported, d.id) }))
}

export interface ExclusiveModeHint {
  /** 공유 모드라 눈에 띄게 보일 안내인지(독점이면 확인 문구만) */
  emphasized: boolean
  text: string
  /** 한 번에 독점으로 바꾸는 버튼을 보일지(WASAPI 공유일 때만) */
  canSwitch: boolean
}

/** 독점 모드 권장 문구(지연 이득) — GOAL 1절의 WASAPI 로컬 지연 목표(독점 ≤15ms, 공유 ≤30ms) */
export const EXCLUSIVE_RECOMMEND_TEXT =
  '합주에는 WASAPI 독점 모드를 권장합니다. 공유 모드보다 지연이 짧습니다(로컬 처리 지연 목표: 독점 15ms 이하, 공유 30ms 이하).'
/** 독점 모드 주의(다른 앱 소리 막힘) */
export const EXCLUSIVE_CAUTION_TEXT =
  '단, 독점 모드에서는 이 앱이 장치를 혼자 쓰므로 그동안 브라우저·메신저 등 다른 앱의 소리가 이 장치로 나오지 않습니다.'

/**
 * 오디오 설정 화면의 WASAPI 독점 모드 권장 안내(P1-19, 순수 함수).
 * 기본값은 공유(다른 앱 소리를 막지 않기 위해)이고, 독점은 권장 설정이다.
 * - 입력·출력 중 하나라도 WASAPI 공유 → 눈에 띄는 권장 안내 + 바꾸기 버튼
 * - WASAPI 독점만 씀 → 다른 앱 소리 막힘 확인 문구
 * - ASIO만 씀, 또는 이 배포본에 독점이 없음 → 안내 없음(null)
 */
export function exclusiveModeHint(
  config: Pick<EngineConfig, 'inputDriver' | 'outputDriver'>,
  supported: readonly DriverKind[] | null,
): ExclusiveModeHint | null {
  if (!isDriverSupported(supported, 'wasapiExclusive')) return null
  const drivers = [config.inputDriver, config.outputDriver]
  if (drivers.includes('wasapiShared')) {
    return { emphasized: true, text: `지금은 WASAPI 공유 모드입니다. ${EXCLUSIVE_RECOMMEND_TEXT} ${EXCLUSIVE_CAUTION_TEXT}`, canSwitch: !drivers.includes('asio') }
  }
  if (drivers.includes('wasapiExclusive')) {
    return { emphasized: false, text: `WASAPI 독점 모드(권장)입니다. ${EXCLUSIVE_CAUTION_TEXT}`, canSwitch: false }
  }
  return null
}
