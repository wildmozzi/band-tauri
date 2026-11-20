// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest'
import { THREAD_POLICY_STORAGE_KEY, loadDeviceThreadPolicy, saveDeviceThreadPolicy } from './deviceLocalSettings'

const OFF = { highQos: false, preferPCores: false }

beforeEach(() => localStorage.clear())

describe('기기별 스레드 정책 저장(#245 재리뷰 L4)', () => {
  it('키가 없으면(새 PC·이 변경 전) 끔 — 옛 로컬 캐시의 값은 옮기지 않는다', () => {
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { threadPolicy: { highQos: true, preferPCores: true } } }))
    expect(loadDeviceThreadPolicy()).toEqual(OFF)
  })
  it('저장한 값을 다시 읽는다', () => {
    saveDeviceThreadPolicy({ highQos: true, preferPCores: false })
    expect(loadDeviceThreadPolicy()).toEqual({ highQos: true, preferPCores: false })
    saveDeviceThreadPolicy(undefined)
    expect(loadDeviceThreadPolicy()).toEqual(OFF)
  })
  it('손상·이상한 값은 끔', () => {
    for (const bad of ['{', '"both"', '1', '{"highQos":"yes"}']) {
      localStorage.setItem(THREAD_POLICY_STORAGE_KEY, bad)
      expect(loadDeviceThreadPolicy()).toEqual(OFF)
    }
  })
})
