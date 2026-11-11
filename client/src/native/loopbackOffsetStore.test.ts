import { describe, expect, it, vi } from 'vitest'
import type { DeviceInfo, EngineConfig, LoopbackReport } from './engineApi'
import {
  clearLoopbackOffset,
  loadLoopbackOffset,
  loopbackDeviceKey,
  outputLatencyView,
  reapplyLoopbackOffset,
  rememberLoopbackOffset,
  resolveDeviceIds,
  saveLoopbackOffset,
  storableOffset,
  type LoopbackOffsetDeps,
  type OffsetStorage,
} from './loopbackOffsetStore'

function memStorage(): OffsetStorage & { data: Map<string, string> } {
  const data = new Map<string, string>()
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, v),
    removeItem: (k) => void data.delete(k),
  }
}

const config: EngineConfig = {
  inputDriver: 'wasapiShared',
  outputDriver: 'wasapiShared',
  inputDevice: null,
  outputDevice: 'out-1',
  bufferSize: 256,
  frameMs: 5,
  codec: 'opus',
  channels: 1,
  inputChannels: [0],
}

const dev = (id: string, isInput: boolean, isDefault: boolean): DeviceInfo => ({
  id, name: id, driver: 'wasapiShared', isInput, isDefault, channels: 2, sampleRates: [48000],
})

function deps(over: Partial<LoopbackOffsetDeps> = {}): LoopbackOffsetDeps & { setOffset: ReturnType<typeof vi.fn> } {
  return {
    listDevices: async () => [dev('in-a', true, false), dev('in-b', true, true), dev('out-1', false, true)],
    stats: async () => ({ backend: 'cpalWasapi', sampleRate: 48000 }),
    setOffset: vi.fn(async () => null),
    storage: memStorage(),
    ...over,
  } as LoopbackOffsetDeps & { setOffset: ReturnType<typeof vi.fn> }
}

const report = (verdict: LoopbackReport['verdict'], offsetMs: number | null) => ({ verdict, offsetMs, backend: 'cpalWasapi', sampleRate: 48000 }) as LoopbackReport

describe('loopbackOffsetStore', () => {
  it('기본 장치(null)를 현재 OS 기본 장치 id로 푼다', () => {
    expect(resolveDeviceIds(config, [dev('in-a', true, false), dev('in-b', true, true)])).toEqual({ inputDevice: 'in-b', outputDevice: 'out-1' })
    expect(resolveDeviceIds(config, null)).toEqual({ inputDevice: null, outputDevice: 'out-1' })
  })

  it('키는 드라이버·백엔드·장치·버퍼·레이트가 다르면 달라진다', () => {
    const base = { inputDriver: 'asio' as const, outputDriver: 'asio' as const, inputDevice: 'a', outputDevice: 'a', bufferSize: 128, backend: 'cpalAsio', sampleRate: 48000 }
    const k = loopbackDeviceKey(base)
    expect(loopbackDeviceKey({ ...base, bufferSize: 256 })).not.toBe(k)
    expect(loopbackDeviceKey({ ...base, sampleRate: 44100 })).not.toBe(k)
    expect(loopbackDeviceKey({ ...base, backend: 'cpalWasapi' })).not.toBe(k)
    expect(loopbackDeviceKey({ ...base, inputDevice: 'b' })).not.toBe(k)
  })

  it('ok 판정이고 -50..200ms일 때만 저장 대상', () => {
    expect(storableOffset(report('ok', 12.5))).toBe(12.5)
    expect(storableOffset(report('unstable', 12.5))).toBeNull()
    expect(storableOffset(report('ok', null))).toBeNull()
    expect(storableOffset(report('ok', 250))).toBeNull()
    expect(storableOffset(report('ok', -60))).toBeNull()
  })

  it('저장값이 깨졌거나 범위 밖이면 무시', () => {
    const s = memStorage()
    s.setItem('band.loopbackOffset.k', '{bad')
    expect(loadLoopbackOffset(s, 'k')).toBeNull()
    s.setItem('band.loopbackOffset.k', JSON.stringify({ offsetMs: 999 }))
    expect(loadLoopbackOffset(s, 'k')).toBeNull()
    expect(saveLoopbackOffset(s, 'k', { offsetMs: 8, at: 't' })).toBe(true)
    expect(loadLoopbackOffset(s, 'k')).toEqual({ offsetMs: 8, at: 't' })
    clearLoopbackOffset(s, 'k')
    expect(loadLoopbackOffset(s, 'k')).toBeNull()
    expect(loadLoopbackOffset(null, 'k')).toBeNull()
  })

  it('측정 저장 → 앱 재시작(새 엔진) 뒤 같은 장치 구성이면 엔진에 다시 넣는다', async () => {
    const d = deps()
    const key = await rememberLoopbackOffset(config, report('ok', 14), '2026-11-11T00:00:00Z', d)
    expect(key).toContain('in-b')
    expect(await reapplyLoopbackOffset(config, d)).toBe(14)
    expect(d.setOffset).toHaveBeenCalledWith(14)
  })

  it('정상 판정이 아니면 저장하지 않고 이전 값을 지킨다', async () => {
    const d = deps()
    await rememberLoopbackOffset(config, report('ok', 10), 't1', d)
    expect(await rememberLoopbackOffset(config, report('unstable', 40), 't2', d)).toBeNull()
    expect(await reapplyLoopbackOffset(config, d)).toBe(10)
  })

  it('장치 구성이 다르면(버퍼·OS 기본 장치·백엔드) 다시 넣지 않는다', async () => {
    const d = deps()
    await rememberLoopbackOffset(config, report('ok', 10), 't', d)
    expect(await reapplyLoopbackOffset({ ...config, bufferSize: 512 }, d)).toBeNull()
    const otherDefault = deps({ storage: d.storage, listDevices: async () => [dev('in-a', true, true), dev('out-1', false, true)] })
    expect(await reapplyLoopbackOffset(config, otherDefault)).toBeNull()
    const fallback = deps({ storage: d.storage, stats: async () => ({ backend: 'wasapiShared3', sampleRate: 48000 }) })
    expect(await reapplyLoopbackOffset(config, fallback)).toBeNull()
    expect(d.setOffset).not.toHaveBeenCalled()
  })

  it('엔진 오류(옛 엔진·정지 경합)는 조용히 무시', async () => {
    const d = deps()
    await rememberLoopbackOffset(config, report('ok', 10), 't', d)
    d.setOffset.mockRejectedValueOnce(new Error('no engine'))
    expect(await reapplyLoopbackOffset(config, d)).toBeNull()
    expect(await reapplyLoopbackOffset(config, deps({ stats: async () => { throw new Error('x') } }))).toBeNull()
    expect(await reapplyLoopbackOffset(config, deps({ stats: async () => ({ backend: '', sampleRate: 48000 }) }))).toBeNull()
  })

  it('메트로놈 패널 표시: 출처별 문구, 없음이면 측정 안내', () => {
    expect(outputLatencyView({ outputLatencyMs: 7.25, outputLatencySource: 'driver' })).toEqual({ source: 'driver', text: '출력 지연 보정: 드라이버 보고 7.3ms', needsMeasure: false })
    expect(outputLatencyView({ outputLatencyMs: 6, outputLatencySource: 'loopback' })?.text).toBe('출력 지연 보정: 루프백 측정 6.0ms')
    expect(outputLatencyView({ outputLatencyMs: null, outputLatencySource: null })).toEqual({ source: null, text: '출력 지연 보정: 없음', needsMeasure: true })
    expect(outputLatencyView(null)).toBeNull()
  })
})
