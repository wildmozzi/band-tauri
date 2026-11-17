// 웹 믹서 레벨 미터(66ms): 값이 그대로면 스토어 구독자를 깨우지 않는다
import { describe, expect, it, vi } from 'vitest'
import { RoomSessionStore } from './roomSessionStore'
import { levelsPatch } from './useWebAudioMixer'

describe('levelsPatch', () => {
  it('무음(같은 레벨)이 이어지면 66ms 틱마다 구독자를 다시 부르지 않는다', () => {
    const store = new RoomSessionStore()
    const listener = vi.fn()
    store.subscribe(listener)
    const tick = (levels: Record<string, number>, max: number) => store.patch('levels', (cur) => levelsPatch(cur.audioLevels, levels, max))

    tick({ a: 0, b: 0 }, 0)
    expect(listener).toHaveBeenCalledTimes(1) // {} → {a,b}
    const first = store.getSnapshot().levels.audioLevels
    for (let i = 0; i < 10; i++) tick({ a: 0, b: 0 }, 0)
    expect(listener).toHaveBeenCalledTimes(1)
    expect(store.getSnapshot().levels.audioLevels).toBe(first)

    tick({ a: 12, b: 0 }, 12)
    expect(listener).toHaveBeenCalledTimes(2)
    expect(store.getSnapshot().levels).toEqual({ audioLevels: { a: 12, b: 0 }, masterLevel: 12 })
  })
})
