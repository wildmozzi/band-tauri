// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest'
import { LEGACY_THREAD_POLICY_STORAGE_KEY, THREAD_POLICY_STORAGE_KEY, loadDeviceThreadPolicy, saveDeviceThreadPolicy } from './deviceLocalSettings'

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
  it('P 코어 정책(선호·조건부 고정)도 이 PC에 저장·복원된다. 2단계 저장값(preferPCores)은 고정으로 읽힌다', () => {
    saveDeviceThreadPolicy({ highQos: true, preferPCores: false, pCoreMode: 'adaptive' })
    expect(JSON.parse(localStorage.getItem(THREAD_POLICY_STORAGE_KEY)!)).toEqual({ highQos: true, preferPCores: false, pCoreMode: 'adaptive' })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: false, pCoreMode: 'adaptive' })
    localStorage.setItem(THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: true, preferPCores: true }))
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: true })
    // 옛 키(v1)의 고정도 옮긴다
    localStorage.clear()
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: false, preferPCores: true }))
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: true })
  })
  it('새 키(v2)가 있으면 옛 키는 보지 않는다 — v2에서 끈 값은 사용자가 끈 것', () => {
    localStorage.setItem(LEGACY_THREAD_POLICY_STORAGE_KEY, JSON.stringify({ highQos: true, preferPCores: true }))
    saveDeviceThreadPolicy({ highQos: false, preferPCores: false })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: false, preferPCores: false })
    // 옛 키는 건드리지 않는다
    expect(JSON.parse(localStorage.getItem(LEGACY_THREAD_POLICY_STORAGE_KEY)!)).toEqual({ highQos: true, preferPCores: true })
  })
})
