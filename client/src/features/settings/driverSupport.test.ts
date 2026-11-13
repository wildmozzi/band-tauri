import { describe, expect, it } from 'vitest'
import type { DriverKind } from '../../native/engineApi'
import { DEFAULT_ENGINE_CONFIG } from '../../native/engineConfig'
import {
  coerceToSupportedDrivers,
  driverChoices,
  fallbackDriver,
  isDriverSupported,
  unsupportedDriverReason,
} from './driverSupport'

const NO_ASIO: DriverKind[] = ['wasapiExclusive', 'wasapiShared']
const ALL: DriverKind[] = ['asio', 'wasapiExclusive', 'wasapiShared']

describe('driverSupport', () => {
  it('목록이 없으면(옛 엔진) 모두 지원으로 본다', () => {
    expect(isDriverSupported(null, 'asio')).toBe(true)
    expect(driverChoices(null).every((c) => !c.disabled && c.reason === null)).toBe(true)
  })

  it('ASIO 없는 배포본: ASIO만 비활성 + 이유', () => {
    const choices = driverChoices(NO_ASIO)
    expect(choices.find((c) => c.id === 'asio')).toMatchObject({ disabled: true })
    expect(choices.filter((c) => c.disabled)).toHaveLength(1)
    expect(unsupportedDriverReason(NO_ASIO, 'asio')).toBe('이 배포본에는 ASIO가 포함되어 있지 않습니다 — WASAPI 독점 모드를 쓰세요')
    expect(unsupportedDriverReason(NO_ASIO, 'wasapiShared')).toBeNull()
    expect(driverChoices(ALL).some((c) => c.disabled)).toBe(false)
  })

  it('대체 드라이버: 독점 → 공유 순', () => {
    expect(fallbackDriver(NO_ASIO)).toBe('wasapiExclusive')
    expect(fallbackDriver(['wasapiShared'])).toBe('wasapiShared')
    expect(fallbackDriver([])).toBeNull()
  })

  it('저장된 ASIO 설정은 독점으로 되돌리고 장치를 비운다', () => {
    const cfg = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio' as const, outputDriver: 'asio' as const, inputDevice: 'asio::X', outputDevice: 'asio::X' }
    const r = coerceToSupportedDrivers(cfg, NO_ASIO)
    expect(r.config).toMatchObject({ inputDriver: 'wasapiExclusive', outputDriver: 'wasapiExclusive', inputDevice: null, outputDevice: null })
    expect(r.notice).toContain('ASIO')
    expect(r.notice).toContain('WASAPI 독점 모드로 바꿨습니다')
    expect(unsupportedDriverReason(['asio'], 'wasapiExclusive')).toBe('이 배포본에는 WASAPI 독점이 포함되어 있지 않습니다 — ASIO 모드를 쓰세요')
  })

  it('한쪽만 지원 안 하면 그쪽만 바꾼다', () => {
    const cfg = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio' as const, inputDevice: 'asio::X', outputDevice: 'wasapi::Y' }
    const r = coerceToSupportedDrivers(cfg, NO_ASIO)
    expect(r.config.inputDriver).toBe('wasapiExclusive')
    expect(r.config.inputDevice).toBeNull()
    expect(r.config.outputDriver).toBe('wasapiShared')
    expect(r.config.outputDevice).toBe('wasapi::Y')
  })

  it('지원하거나 목록을 모르면 그대로(같은 객체)', () => {
    const cfg = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio' as const }
    expect(coerceToSupportedDrivers(cfg, null)).toEqual({ config: cfg, notice: null })
    expect(coerceToSupportedDrivers(cfg, ALL).config).toBe(cfg)
    expect(coerceToSupportedDrivers(cfg, []).config).toBe(cfg)
  })
})
