import { describe, expect, it } from 'vitest'
import { clampBeats, clampBpm, followConfig, metronomeErrorFromCode, parseMetronomeSignal, stoppedConfig } from './metronomeLogic'

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
