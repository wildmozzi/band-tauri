import { describe, expect, it } from 'vitest'
import type { DriverKind } from '../../native/engineApi'
import { DEFAULT_ENGINE_CONFIG } from '../../native/engineConfig'
import {
  coerceToSupportedDrivers,
  driverChoices,
  EXCLUSIVE_CAUTION_TEXT,
  EXCLUSIVE_RECOMMEND_TEXT,
  exclusiveModeHint,
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

describe('exclusiveModeHint (P1-19)', () => {
  it('기본값(WASAPI 공유)이면 눈에 띄게 독점을 권하고 지연 이득과 다른 앱 소리 막힘을 함께 말한다', () => {
    const h = exclusiveModeHint(DEFAULT_ENGINE_CONFIG, ALL)
    expect(DEFAULT_ENGINE_CONFIG.inputDriver).toBe('wasapiShared')
    expect(h).not.toBeNull()
    expect(h!.emphasized).toBe(true)
    expect(h!.canSwitch).toBe(true)
    expect(h!.text).toContain('지금은 WASAPI 공유 모드입니다')
    expect(h!.text).toContain('WASAPI 독점 모드를 권장합니다')
    expect(h!.text).toContain('독점 15ms 이하, 공유 30ms 이하')
    expect(h!.text).toContain('다른 앱의 소리가 이 장치로 나오지 않습니다')
  })

  it('한쪽만 공유여도 권한다, ASIO가 섞이면 한 번에 바꾸기 버튼은 없다', () => {
    expect(exclusiveModeHint({ inputDriver: 'wasapiExclusive', outputDriver: 'wasapiShared' }, ALL)?.canSwitch).toBe(true)
    const mixed = exclusiveModeHint({ inputDriver: 'asio', outputDriver: 'wasapiShared' }, ALL)
    expect(mixed?.emphasized).toBe(true)
    expect(mixed?.canSwitch).toBe(false)
  })

  it('독점이면 강조 없이 다른 앱 소리 막힘만 확인시킨다', () => {
    const h = exclusiveModeHint({ inputDriver: 'wasapiExclusive', outputDriver: 'wasapiExclusive' }, NO_ASIO)
    expect(h).toEqual({ emphasized: false, canSwitch: false, text: `WASAPI 독점 모드(권장)입니다. ${EXCLUSIVE_CAUTION_TEXT}` })
  })

  it('ASIO만 쓰거나 배포본에 독점이 없으면 안내하지 않는다', () => {
    expect(exclusiveModeHint({ inputDriver: 'asio', outputDriver: 'asio' }, ALL)).toBeNull()
    expect(exclusiveModeHint(DEFAULT_ENGINE_CONFIG, ['wasapiShared'])).toBeNull()
    expect(exclusiveModeHint(DEFAULT_ENGINE_CONFIG, null)?.emphasized).toBe(true)
  })

  it('권장 문구는 공유를 기본값으로 둔 이유와 어긋나지 않는다(권장일 뿐 강제 아님)', () => {
    expect(EXCLUSIVE_RECOMMEND_TEXT).toContain('권장')
    expect(EXCLUSIVE_CAUTION_TEXT).toContain('다른 앱')
  })
})
