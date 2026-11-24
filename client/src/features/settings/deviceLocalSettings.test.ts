// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest'
import { DEFAULT_ENGINE_CONFIG } from '../../native/engineConfig'
import {
  DEVICE_SETTINGS_STORAGE_KEY,
  LEGACY_THREAD_POLICY_STORAGE_KEY,
  THREAD_POLICY_STORAGE_KEY,
  hasDeviceSettings,
  loadDeviceSettings,
  loadDeviceThreadPolicy,
  saveDeviceSettings,
  saveDeviceThreadPolicy,
} from './deviceLocalSettings'

/** 기본값(P1-21 3단계): 절전 끄기 켬, P 코어 묶기 끔 */
const DEFAULT = { highQos: true, preferPCores: false }

beforeEach(() => localStorage.clear())

describe('기기별 스레드 정책 저장(#245 재리뷰 L4, 기본값 P1-21 3단계)', () => {
  it('저장값이 없으면(새 PC) 기본값 — 옛 로컬 캐시의 값은 옮기지 않는다', () => {
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { threadPolicy: { highQos: false, preferPCores: true } } }))
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
  })
  it('저장한 값을 다시 읽는다 — 명시적으로 끈 절전 끄기도 그대로', () => {
    expect(THREAD_POLICY_STORAGE_KEY).toBe('band_thread_policy_v2')
    saveDeviceThreadPolicy({ highQos: false, preferPCores: false })
    expect(JSON.parse(localStorage.getItem(THREAD_POLICY_STORAGE_KEY)!)).toEqual({ highQos: false, preferPCores: false })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: false, preferPCores: false })
    saveDeviceThreadPolicy({ highQos: true, preferPCores: true })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: true })
    saveDeviceThreadPolicy(undefined)
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
  })
  it('손상·이상한 값은 기본값', () => {
    for (const bad of ['{', '"both"', '1', '{"highQos":"yes"}']) {
      localStorage.setItem(THREAD_POLICY_STORAGE_KEY, bad)
      expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
    }
  })
  it('옛 키(v1, 기본값이 끔이던 때): highQos false는 옛 기본값과 구별되지 않아 새 기본값(켬), 나머지는 옮긴다', () => {
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: false, preferPCores: false }))
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: false, preferPCores: true }))
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: true })
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: true, preferPCores: false }))
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, '{')
    expect(loadDeviceThreadPolicy()).toEqual(DEFAULT)
  })
  it('새 키(v2)가 있으면 옛 키는 보지 않는다 — v2에서 끈 값은 사용자가 끈 것', () => {
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: true, preferPCores: true }))
    saveDeviceThreadPolicy({ highQos: false, preferPCores: false })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: false, preferPCores: false })
    // 옛 키는 건드리지 않는다
    expect(JSON.parse(localStorage.getItem(LEGACY_THREAD_POLICY_STORAGE_KEY)!)).toEqual({ highQos: true, preferPCores: true })
  })
})

describe('기기별 장치 설정 저장(P2-27 후속)', () => {
  it('키가 없으면 null(부르는 쪽이 이행값을 정한다)', () => {
    expect(loadDeviceSettings()).toBeNull()
    expect(hasDeviceSettings()).toBe(false)
  })
  it('장치 쪽 필드만 저장하고 다시 읽는다 — 코덱·지터 등은 저장하지 않는다', () => {
    saveDeviceSettings({ ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio', inputDevice: 'asio::X', inputChannels: [2, 3], channels: 2, bufferSize: 64, codec: 'pcm16', jitterMaxMs: 40 })
    const raw = JSON.parse(localStorage.getItem(DEVICE_SETTINGS_STORAGE_KEY)!) as Record<string, unknown>
    expect(Object.keys(raw).sort()).toEqual(['bufferSize', 'inputChannels', 'inputDevice', 'inputDriver', 'outputDevice', 'outputDriver'])
    expect(loadDeviceSettings()).toEqual({ inputDriver: 'asio', outputDriver: 'wasapiShared', inputDevice: 'asio::X', outputDevice: null, inputChannels: [2, 3], bufferSize: 64 })
    expect(hasDeviceSettings()).toBe(true)
  })
  it('손상·이상한 값은 null 또는 정리된 값', () => {
    for (const bad of ['{', '"x"', '1', '[]', 'null']) {
      localStorage.setItem(DEVICE_SETTINGS_STORAGE_KEY, bad)
      expect(loadDeviceSettings()).toBeNull()
    }
    // 장치 필드가 하나도 없는 객체도 저장값 없음(이행 경로로) — #256 리뷰 L1
    for (const empty of [{}, { codec: 'pcm16', foo: 1 }]) {
      localStorage.setItem(DEVICE_SETTINGS_STORAGE_KEY, JSON.stringify(empty))
      expect(loadDeviceSettings()).toBeNull()
      expect(hasDeviceSettings()).toBe(false)
    }
    localStorage.setItem(DEVICE_SETTINGS_STORAGE_KEY, JSON.stringify({ inputDriver: 'nope', bufferSize: 99999, inputChannels: [-1] }))
    expect(loadDeviceSettings()).toMatchObject({ inputDriver: 'wasapiShared', bufferSize: 1024, inputChannels: [0] })
  })
})
