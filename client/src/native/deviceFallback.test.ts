import { describe, expect, it } from 'vitest'
import type { DeviceInfo, DriverKind, EngineConfig } from './engineApi'
import { DEFAULT_ENGINE_CONFIG } from './engineConfig'
import { deviceNameFromId, fallbackNotice, needsDeviceRecheck, noDeviceMessage, resolveRunConfig, runInputDevice } from './deviceFallback'

const dev = (over: Partial<DeviceInfo> & Pick<DeviceInfo, 'id'>): DeviceInfo => ({
  name: deviceNameFromId(over.id),
  driver: 'wasapiShared',
  isInput: true,
  isDefault: false,
  channels: 2,
  sampleRates: [48000],
  ...over,
})

/** 이 PC: 내장 마이크(기본, 1채널)·스피커(기본)만 있다 */
const LAPTOP: DeviceInfo[] = [
  dev({ id: 'wasapi::마이크 배열', isDefault: true, channels: 1 }),
  dev({ id: 'wasapi::스피커', isInput: false, isDefault: true }),
]
/** 원래 PC: 4채널 인터페이스 */
const STUDIO: DeviceInfo[] = [
  ...LAPTOP,
  dev({ id: 'wasapi::Focusrite USB', channels: 4 }),
  dev({ id: 'wasapi::Focusrite USB Out', isInput: false }),
]
const NO_ASIO: DriverKind[] = ['wasapiExclusive', 'wasapiShared']

const saved: EngineConfig = {
  ...DEFAULT_ENGINE_CONFIG,
  inputDevice: 'wasapi::Focusrite USB',
  outputDevice: 'wasapi::Focusrite USB Out',
  channels: 2,
  inputChannels: [2, 3],
  bufferSize: 64,
}

describe('resolveRunConfig (P2-27)', () => {
  it('저장된 장치가 있으면 그대로(같은 객체, 알림 없음)', () => {
    const r = resolveRunConfig(saved, STUDIO, NO_ASIO)
    expect(r.config).toBe(saved)
    expect(r.fallbacks).toEqual([])
    expect(r.notice).toBeNull()
    expect(r.noDevices).toEqual([])
  })

  it('저장된 장치가 없으면 기본 장치로, 입력 채널은 새 장치에 맞춘다 — 저장값 객체는 바꾸지 않는다', () => {
    const before = structuredClone(saved)
    const r = resolveRunConfig(saved, LAPTOP, NO_ASIO)
    expect(r.config).toMatchObject({ inputDevice: null, outputDevice: null, inputChannels: [0, 0], bufferSize: 64 })
    expect(r.fallbacks.map((f) => f.kind)).toEqual(['device', 'device', 'inputChannels'])
    expect(r.notice).toBe(
      "저장된 장치 'Focusrite USB', 'Focusrite USB Out'를 찾을 수 없어 기본 장치로 시작했습니다. 입력 채널은 Ch1·Ch1로 맞춥니다. 저장된 설정은 그대로 둡니다(원래 장치에서는 그대로 동작).",
    )
    expect(saved).toEqual(before)
  })

  it('한쪽만 없으면 그쪽만 바꾼다(입력 채널이 맞으면 그대로)', () => {
    const cfg = { ...saved, outputDevice: 'wasapi::스피커', channels: 1 as const, inputChannels: [0] as [number] }
    const r = resolveRunConfig(cfg, LAPTOP, NO_ASIO)
    expect(r.config).toMatchObject({ inputDevice: null, outputDevice: 'wasapi::스피커', inputChannels: [0] })
    expect(r.fallbacks).toEqual([{ kind: 'device', dir: 'input', savedId: 'wasapi::Focusrite USB' }])
    expect(r.notice).toContain("'Focusrite USB'를 찾을 수 없어")
  })

  it('저장된 드라이버를 이 배포본이 지원하지 않으면 WASAPI 공유 기본 장치로', () => {
    const cfg: EngineConfig = { ...saved, inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::Focusrite USB ASIO', outputDevice: 'asio::Focusrite USB ASIO' }
    const r = resolveRunConfig(cfg, LAPTOP, NO_ASIO)
    expect(r.config).toMatchObject({ inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: null, outputDevice: null, inputChannels: [0, 0] })
    expect(r.fallbacks.filter((f) => f.kind === 'driver')).toHaveLength(2)
    expect(r.notice).toContain('저장된 드라이버(ASIO)는 이 배포본에서 쓸 수 없어 WASAPI 공유 기본 장치로 시작했습니다.')
    // 공유가 없는 배포본이면 지원하는 것 중 다음(독점)
    expect(resolveRunConfig(cfg, LAPTOP, ['wasapiExclusive']).config.inputDriver).toBe('wasapiExclusive')
  })

  it('지원 목록을 모르면(옛 엔진) 드라이버는 그대로 두고 장치만 본다', () => {
    const cfg: EngineConfig = { ...saved, inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::X', outputDevice: 'asio::X' }
    const asio = [dev({ id: 'asio::X', driver: 'asio', channels: 8 }), dev({ id: 'asio::X', driver: 'asio', isInput: false })]
    expect(resolveRunConfig(cfg, asio, null).config).toBe(cfg)
    const r = resolveRunConfig(cfg, LAPTOP, null)
    expect(r.config).toMatchObject({ inputDriver: 'asio', inputDevice: null })
    expect(r.noDevices).toEqual(['input', 'output'])
  })

  it('장치 목록을 모르면(조회 실패) 장치는 그대로 둔다 — 엔진이 직접 찾는다', () => {
    const r = resolveRunConfig(saved, null, NO_ASIO)
    expect(r.config).toBe(saved)
    expect(r.noDevices).toEqual([])
  })

  it('버퍼가 범위 밖이면 기본값(128), 범위 안이면 장치가 바뀌어도 그대로', () => {
    const r = resolveRunConfig({ ...saved, bufferSize: 4096 }, STUDIO, NO_ASIO)
    expect(r.config.bufferSize).toBe(128)
    expect(r.notice).toContain('버퍼 크기는 기본값 128로 시작했습니다.')
    expect(resolveRunConfig({ ...saved, bufferSize: 100.5 }, STUDIO, NO_ASIO).config.bufferSize).toBe(128)
    expect(resolveRunConfig({ ...saved, bufferSize: 32 }, LAPTOP, NO_ASIO).config.bufferSize).toBe(32)
  })

  it('장치가 하나도 없으면 noDevices로 알린다(시작 전 원인 문구)', () => {
    const r = resolveRunConfig({ ...saved, inputDevice: null, outputDevice: null }, LAPTOP.filter((d) => !d.isInput), NO_ASIO)
    expect(r.noDevices).toEqual(['input'])
    expect(noDeviceMessage(r.noDevices, r.config)).toBe(
      "이 PC에서 사용할 수 있는 입력 장치(WASAPI 공유)를 찾지 못했습니다. 마이크·오디오 인터페이스(또는 스피커·헤드폰)를 연결한 뒤 오디오 설정에서 '장치 새로고침'을 누르고 다시 시도하세요.",
    )
    expect(noDeviceMessage([], r.config)).toBeNull()
  })
})

describe('보조 함수', () => {
  it('needsDeviceRecheck: 저장된 장치가 목록에 없거나 그 드라이버 장치가 없을 때만', () => {
    expect(needsDeviceRecheck(saved, STUDIO)).toBe(false)
    expect(needsDeviceRecheck(saved, LAPTOP)).toBe(true)
    expect(needsDeviceRecheck({ ...saved, inputDevice: null, outputDevice: null }, LAPTOP)).toBe(false)
    expect(needsDeviceRecheck({ ...saved, inputDevice: null, outputDevice: null }, [])).toBe(true)
    expect(needsDeviceRecheck(saved, null)).toBe(false)
  })

  it('runInputDevice: 고른 장치, 없으면 그 드라이버의 기본 장치', () => {
    expect(runInputDevice(saved, STUDIO)?.channels).toBe(4)
    expect(runInputDevice({ ...saved, inputDevice: null }, STUDIO)?.id).toBe('wasapi::마이크 배열')
    expect(runInputDevice(saved, null)).toBeNull()
  })

  it('deviceNameFromId: 엔진 id 접두사를 뗀다', () => {
    expect(deviceNameFromId('wasapi::Focusrite USB')).toBe('Focusrite USB')
    expect(deviceNameFromId('asio::A::B')).toBe('A::B')
    expect(deviceNameFromId('이름만')).toBe('이름만')
  })

  it('fallbackNotice: 시작 전 안내는 미래형, 받침 있는 이름은 "을"', () => {
    expect(fallbackNotice([{ kind: 'device', dir: 'input', savedId: 'wasapi::헤드셋 마이크' }], 'future')).toBe(
      "저장된 장치 '헤드셋 마이크'를 찾을 수 없어 기본 장치로 시작합니다. 저장된 설정은 그대로 둡니다(원래 장치에서는 그대로 동작).",
    )
    expect(fallbackNotice([{ kind: 'device', dir: 'input', savedId: 'wasapi::내장 마이크 입력' }])).toContain("'내장 마이크 입력'을 찾을 수 없어")
    expect(fallbackNotice([])).toBeNull()
  })
})
