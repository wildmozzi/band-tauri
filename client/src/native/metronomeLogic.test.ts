import { describe, expect, it } from 'vitest'
import {
  DEFAULT_METRONOME_VOLUME,
  clampBeats,
  clampBpm,
  followConfig,
  loadMetronomeGain,
  metronomeErrorFromCode,
  metronomeGainKey,
  parseMetronomeSignal,
  saveMetronomeGain,
  stoppedConfig,
} from './metronomeLogic'

describe('metronomeLogic', () => {
  const state = { running: true, bpm: 90, beatsPerBar: 4, startUs: 12 }

  it('메시지 검증', () => {
    expect(parseMetronomeSignal({ type: 'metronome', from: 'h', state })).toEqual({ from: 'h', state })
    expect(parseMetronomeSignal({ type: 'metronome', state })).toBeNull()
    expect(parseMetronomeSignal({ type: 'metronome', from: 'h', state: { ...state, bpm: 301 } })).toBeNull()
    expect(parseMetronomeSignal({ type: 'metronome', from: 'h', state: { ...state, beatsPerBar: 2.5 } })).toBeNull()
    expect(parseMetronomeSignal({ type: 'metronome', from: 'h', state: { ...state, startUs: -1 } })).toBeNull()
    expect(parseMetronomeSignal({ type: 'metronome', from: 'h', state: [] })).toBeNull()
    expect(parseMetronomeSignal({ type: 'chat', from: 'h', state })).toBeNull()
  })

  it('범위 고정과 엔진 설정', () => {
    expect(clampBpm(10)).toBe(20)
    expect(clampBpm(Number.NaN)).toBe(120)
    expect(clampBpm(140.6)).toBe(141)
    expect(clampBeats(40)).toBe(16)
    expect(clampBeats(0)).toBe(1)
    expect(followConfig('h', state)).toEqual({ ...state, refPeerId: 'h' })
    expect(stoppedConfig(500, 3)).toEqual({ running: false, bpm: 300, beatsPerBar: 3 })
  })

  it('에러 코드', () => {
    expect(metronomeErrorFromCode('metronome-host-only')).toContain('방장')
    expect(metronomeErrorFromCode('metronome-invalid', '서버 문구')).toBe('서버 문구')
    expect(metronomeErrorFromCode('other')).toBeNull()
  })
})

describe('클릭 볼륨·뮤트 영속 (PR #36 L5)', () => {
  const memStorage = () => {
    const m = new Map<string, string>()
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m }
  }

  it('사용자별로 저장하고 다시 읽는다', () => {
    const s = memStorage()
    expect(loadMetronomeGain(s, 'u1')).toEqual({ volume: DEFAULT_METRONOME_VOLUME, muted: false })
    expect(saveMetronomeGain(s, 'u1', { volume: 0.3, muted: true })).toBe(true)
    expect(loadMetronomeGain(s, 'u1')).toEqual({ volume: 0.3, muted: true })
    expect(loadMetronomeGain(s, 'u2')).toEqual({ volume: DEFAULT_METRONOME_VOLUME, muted: false })
    saveMetronomeGain(s, null, { volume: 0.5, muted: false })
    expect(s.m.has(metronomeGainKey(null))).toBe(true)
    expect(metronomeGainKey(undefined)).toBe(metronomeGainKey(null))
  })

  it('범위 밖·깨진 값은 기본값/범위로 고친다', () => {
    const s = memStorage()
    s.setItem(metronomeGainKey('u'), JSON.stringify({ volume: 7, muted: 'yes' }))
    expect(loadMetronomeGain(s, 'u')).toEqual({ volume: 1, muted: false })
    s.setItem(metronomeGainKey('u'), '{not json')
    expect(loadMetronomeGain(s, 'u')).toEqual({ volume: DEFAULT_METRONOME_VOLUME, muted: false })
    s.setItem(metronomeGainKey('u'), JSON.stringify([1]))
    expect(loadMetronomeGain(s, 'u').volume).toBe(DEFAULT_METRONOME_VOLUME)
    saveMetronomeGain(s, 'u', { volume: -1, muted: false })
    expect(loadMetronomeGain(s, 'u').volume).toBe(0)
  })

  it('저장소가 없거나 막혀도 동작한다', () => {
    expect(loadMetronomeGain(null, 'u')).toEqual({ volume: DEFAULT_METRONOME_VOLUME, muted: false })
    expect(saveMetronomeGain(null, 'u', { volume: 0.2, muted: false })).toBe(false)
    const broken = {
      getItem: () => { throw new Error('SecurityError') },
      setItem: () => { throw new Error('QuotaExceededError') },
    }
    expect(loadMetronomeGain(broken, 'u')).toEqual({ volume: DEFAULT_METRONOME_VOLUME, muted: false })
    expect(saveMetronomeGain(broken, 'u', { volume: 0.2, muted: false })).toBe(false)
  })
})
