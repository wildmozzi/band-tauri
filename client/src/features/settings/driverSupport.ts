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

export interface DriverCoercion {
  config: EngineConfig
  /** 바꿨으면 안내 문구, 아니면 null(config는 같은 객체) */
  notice: string | null
}

/**
 * 저장된 설정의 드라이버를 지원 드라이버로 되돌린다. 바뀐 쪽의 장치 id는 다른 드라이버 것이므로 비운다.
 * 지원 목록을 모르거나(null) 대체할 드라이버가 없으면 그대로 둔다.
 */
export function coerceToSupportedDrivers(config: EngineConfig, supported: readonly DriverKind[] | null): DriverCoercion {
  const alt = fallbackDriver(supported)
  if (!alt) return { config, notice: null }
  const inBad = !isDriverSupported(supported, config.inputDriver)
  const outBad = !isDriverSupported(supported, config.outputDriver)
  if (!inBad && !outBad) return { config, notice: null }
  const next: EngineConfig = { ...config }
  const replaced = new Set<DriverKind>()
  if (inBad) {
    replaced.add(config.inputDriver)
    next.inputDriver = alt
    next.inputDevice = null
  }
  if (outBad) {
    replaced.add(config.outputDriver)
    next.outputDriver = alt
    next.outputDevice = null
  }
  const names = [...replaced].map(driverLabel).join(', ')
  return {
    config: next,
    notice: `저장된 드라이버(${names})는 이 배포본에 포함되어 있지 않아 ${driverLabel(alt)} 모드로 바꿨습니다. 장치 선택을 확인해주세요.`,
  }
}
