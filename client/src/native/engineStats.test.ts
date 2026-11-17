// @vitest-environment jsdom
// engine_stats 공유 호출(#214 리뷰 L4): 앞 요청이 끝나기 전에 부른 쪽은 새 IPC 없이 같은 응답을 받는다.
import { afterEach, describe, expect, it, vi } from 'vitest'

const core = vi.hoisted(() => {
  const pending: { resolve: (v: unknown) => void; reject: (e: unknown) => void }[] = []
  return {
    pending,
    invoke: vi.fn(
      () =>
        new Promise((resolve, reject) => {
          pending.push({ resolve, reject })
        }),
    ),
  }
})
vi.mock('@tauri-apps/api/core', () => ({ invoke: core.invoke, isTauri: () => true, Channel: class {} }))

import { engineStats } from './engineApi'

afterEach(() => {
  core.pending.length = 0
  core.invoke.mockClear()
})

describe('engineStats', () => {
  it('진행 중이면 새로 보내지 않고 같은 응답을 나눠 받는다, 끝나면 다음 호출은 새로 보낸다', async () => {
    const a = engineStats()
    const b = engineStats()
    const c = engineStats()
    expect(core.invoke).toHaveBeenCalledTimes(1)
    expect(core.invoke).toHaveBeenCalledWith('engine_stats', undefined)
    core.pending[0].resolve({ running: true })
    await expect(Promise.all([a, b, c])).resolves.toEqual([{ running: true }, { running: true }, { running: true }])
    const d = engineStats()
    expect(core.invoke).toHaveBeenCalledTimes(2)
    core.pending[1].resolve({ running: false })
    await expect(d).resolves.toEqual({ running: false })
  })

  it('실패해도 다음 호출은 새로 보낸다(실패를 붙잡아 두지 않음)', async () => {
    const a = engineStats()
    const b = engineStats()
    core.pending[0].reject('엔진 없음')
    await expect(a).rejects.toBe('엔진 없음')
    await expect(b).rejects.toBe('엔진 없음')
    const c = engineStats()
    expect(core.invoke).toHaveBeenCalledTimes(2)
    core.pending[1].resolve({ running: true })
    await expect(c).resolves.toEqual({ running: true })
  })
})
