// @vitest-environment jsdom
// AudioSettings에서 분리한 훅: 지터 프로필 디바운스 적용, 합주 중 피어 통계 폴링
import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const engine = vi.hoisted(() => ({
  engineStats: vi.fn(async () => ({ peers: [{ peerId: 'peer-1' }] })),
  engineSetJitterProfile: vi.fn(async () => undefined),
}))
vi.mock('../../../native/engineApi', () => engine)

import { PROFILE_APPLY_DEBOUNCE_MS, useDebouncedJitterProfile } from './useDebouncedJitterProfile'
import { PEER_STATS_POLL_MS, useLivePeerStats } from './useLivePeerStats'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.clearAllMocks()
})

describe('useDebouncedJitterProfile', () => {
  it('연속 호출은 마지막 값으로 한 번만 적용한다', () => {
    const apply = vi.fn(async () => undefined)
    const { result } = renderHook(() => useDebouncedJitterProfile(apply))
    result.current(0.2)
    result.current(0.4)
    vi.advanceTimersByTime(PROFILE_APPLY_DEBOUNCE_MS - 1)
    expect(apply).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(apply).toHaveBeenCalledTimes(1)
    expect(apply).toHaveBeenCalledWith(0.4)
  })

  it('기본은 엔진 API로 적용한다', () => {
    const { result } = renderHook(() => useDebouncedJitterProfile())
    result.current(0.7)
    vi.advanceTimersByTime(PROFILE_APPLY_DEBOUNCE_MS)
    expect(engine.engineSetJitterProfile).toHaveBeenCalledWith(0.7)
  })

  it('언마운트하면 대기 중인 적용을 버린다', () => {
    const apply = vi.fn(async () => undefined)
    const { result, unmount } = renderHook(() => useDebouncedJitterProfile(apply))
    result.current(0.5)
    unmount()
    vi.advanceTimersByTime(PROFILE_APPLY_DEBOUNCE_MS)
    expect(apply).not.toHaveBeenCalled()
  })
})

describe('useLivePeerStats', () => {
  it('꺼져 있으면 읽지 않는다', () => {
    const { result } = renderHook(() => useLivePeerStats(false))
    vi.advanceTimersByTime(PEER_STATS_POLL_MS * 2)
    expect(engine.engineStats).not.toHaveBeenCalled()
    expect(result.current).toEqual([])
  })

  it('켜지면 바로 읽고 주기마다 다시 읽으며, 꺼지면 비운다', async () => {
    const { result, rerender } = renderHook(({ on }) => useLivePeerStats(on), { initialProps: { on: true } })
    await act(async () => {})
    expect(result.current).toEqual([{ peerId: 'peer-1' }])
    await act(async () => {
      vi.advanceTimersByTime(PEER_STATS_POLL_MS)
    })
    expect(engine.engineStats).toHaveBeenCalledTimes(2)
    rerender({ on: false })
    expect(result.current).toEqual([])
    vi.advanceTimersByTime(PEER_STATS_POLL_MS * 2)
    expect(engine.engineStats).toHaveBeenCalledTimes(2)
  })
})
