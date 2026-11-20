// @vitest-environment jsdom
// useRoomSelector 캐시: 스냅샷이 같아도 선택자가 바뀌면 다시 고른다 (#78 L2)
import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { RoomSessionStore, type RoomSnapshot } from './roomSessionStore'
import { useRoomSelector } from './useRoomSelector'

describe('useRoomSelector', () => {
  it('스토어 갱신 없이 선택자만 바뀌어도 새 선택자의 결과를 돌려준다', () => {
    const store = new RoomSessionStore()
    const byNickname = (s: RoomSnapshot) => s.connection.nickname
    const byRoom = (s: RoomSnapshot) => s.connection.currentRoomId ?? 'none'
    const { result, rerender } = renderHook(({ sel }) => useRoomSelector(store, sel), { initialProps: { sel: byNickname } })
    expect(result.current).toBe('')
    rerender({ sel: byRoom })
    expect(result.current).toBe('none')
  })

  it('선택 결과가 얕은 비교로 같으면 이전 참조를 유지한다', () => {
    const store = new RoomSessionStore()
    const pick = (s: RoomSnapshot) => ({ peers: s.connection.peers })
    const { result } = renderHook(() => useRoomSelector(store, pick))
    const first = result.current
    act(() => store.update({ chat: [] }))
    expect(result.current).toBe(first)
  })
})
