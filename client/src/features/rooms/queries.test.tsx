// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock } from '../../test/supabaseMock'
import { createQueryWrapper } from '../../test/queryWrapper'
import type { DbRoom } from '../../pages/rooms/roomModel'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../../lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../../test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

import { useMyRooms } from '../../pages/rooms/useMyRooms'
import { roomKeys, useDeleteRoomMutation, useMyRoomsQuery, useRoomsQuery, useUpdateRoomMutation } from './queries'

const room = (id: string, over: Partial<DbRoom> = {}): DbRoom => ({
  id, title: id, description: null, host_id: 'u1', max_participants: 4, current_participants: 0,
  status: 'open', genre: null, tags: [], created_at: '', ...over,
})

afterEach(() => {
  sb.mock!.reset()
  vi.clearAllMocks()
})

describe('rooms queries', () => {
  it('목록을 최신순으로 조회하고, 실시간 변경 시 다시 조회한다', async () => {
    let version = 1
    sb.mock!.setResult('rooms', () => ({ data: version === 1 ? [room('a')] : [room('a'), room('b')] }))
    const { wrapper } = createQueryWrapper()
    const { result, unmount } = renderHook(() => useRoomsQuery(), { wrapper })

    await waitFor(() => expect(result.current.data?.map(r => r.id)).toEqual(['a']))
    expect(argsOf(sb.mock!.calls[0], 'order')[0]).toEqual(['created_at', { ascending: false }])

    version = 2
    act(() => sb.mock!.emitChange())
    await waitFor(() => expect(result.current.data?.map(r => r.id)).toEqual(['a', 'b']))

    unmount()
    expect(sb.mock!.client.removeChannel).toHaveBeenCalledTimes(1)
  })

  it('실시간 변경은 비활성인 내 방 캐시도 stale로 만든다(재요청은 없음)', async () => {
    sb.mock!.setResult('rooms', { data: [room('a')] })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.mine('u1'), [room('m1')])
    const { result } = renderHook(() => useRoomsQuery(), { wrapper })
    await waitFor(() => expect(result.current.data).toHaveLength(1))
    const before = sb.mock!.calls.length

    act(() => sb.mock!.emitChange())

    expect(client.getQueryState(roomKeys.mine('u1'))?.isInvalidated).toBe(true)
    await waitFor(() => expect(sb.mock!.calls.length).toBe(before + 1)) // 목록만 다시 읽음
  })

  it('내 방 모달을 처음 열 때 캐시가 fresh여도 서버에서 다시 읽는다', async () => {
    sb.mock!.setResult('rooms', { data: [room('server')] })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.mine('u1'), [room('cached')])
    const { result } = renderHook(() => useMyRooms('u1'), { wrapper })
    expect(result.current.myRooms.map(r => r.id)).toEqual(['cached'])

    await act(() => result.current.fetchMyRooms())

    await waitFor(() => expect(result.current.myRooms.map(r => r.id)).toEqual(['server']))
  })

  it('내 방은 enabled가 될 때 host_id로 조회한다', async () => {
    sb.mock!.setResult('rooms', { data: [room('m1')] })
    const { wrapper } = createQueryWrapper()
    const { result, rerender } = renderHook(({ on }) => useMyRoomsQuery('u1', on), { wrapper, initialProps: { on: false } })

    expect(sb.mock!.client.from).not.toHaveBeenCalled()
    rerender({ on: true })
    await waitFor(() => expect(result.current.data?.map(r => r.id)).toEqual(['m1']))
    expect(argsOf(sb.mock!.calls[0], 'eq')).toContainEqual(['host_id', 'u1'])
  })

  it('삭제는 방장 조건으로 지우고 내 방 캐시에서 빼며 목록을 무효화한다', async () => {
    sb.mock!.setResult('rooms', { data: null })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.mine('u1'), [room('m1'), room('m2')])
    client.setQueryData(roomKeys.list(), [room('m1')])
    const { result } = renderHook(() => useDeleteRoomMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ roomId: 'm1', userId: 'u1' }))

    const del = sb.mock!.calls.find(c => c.chain.some(([m]) => m === 'delete'))!
    expect(argsOf(del, 'eq')).toEqual([['id', 'm1'], ['host_id', 'u1']])
    expect((client.getQueryData(roomKeys.mine('u1')) as DbRoom[]).map(r => r.id)).toEqual(['m2'])
    expect(client.getQueryState(roomKeys.list())?.isInvalidated).toBe(true)
  })

  it('삭제 실패면 캐시를 건드리지 않고 오류를 던진다', async () => {
    sb.mock!.setResult('rooms', { error: { message: 'denied' } })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.mine('u1'), [room('m1')])
    const { result } = renderHook(() => useDeleteRoomMutation(), { wrapper })

    await expect(result.current.mutateAsync({ roomId: 'm1', userId: 'u1' })).rejects.toEqual({ message: 'denied' })
    expect((client.getQueryData(roomKeys.mine('u1')) as DbRoom[]).map(r => r.id)).toEqual(['m1'])
  })

  it('수정은 내 방 캐시에 패치를 적용한다', async () => {
    sb.mock!.setResult('rooms', { data: null })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.mine('u1'), [room('m1')])
    const { result } = renderHook(() => useUpdateRoomMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ roomId: 'm1', userId: 'u1', update: { title: '새 제목' } }))

    expect((client.getQueryData(roomKeys.mine('u1')) as DbRoom[])[0].title).toBe('새 제목')
  })
})
