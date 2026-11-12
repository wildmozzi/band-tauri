// @vitest-environment jsdom
// 방 상세 query 훅(features/rooms)과 RoomDetail 페이지 훅(pages/room) — 직접 Supabase 호출을 옮긴 부분
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock } from '../../../shared/test/supabaseMock'
import { createQueryWrapper } from '../../../shared/test/queryWrapper'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../../../shared/lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../../../shared/test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

const navigate = vi.hoisted(() => vi.fn())
vi.mock('react-router-dom', () => ({ useNavigate: () => navigate }))

const setRoomPassword = vi.hoisted(() => vi.fn(async (_roomId: string, pw: string | null) => pw !== null))
vi.mock('../../rooms/roomApi', async (importOriginal) => {
  const original = await importOriginal<typeof import('../../rooms/roomApi')>()
  return { ...original, setRoomPassword }
})

import { forgetRoomDetail, roomKeys, useRoomDetailQuery, useSyncParticipantsMutation } from '../../rooms/queries'
import type { DetailRoom } from '../roomDetailModel'
import { useRoomDetailData } from './useRoomDetailData'
import { useRoomHostActions } from './useRoomHostActions'
import { useRoomSettingsForm } from './useRoomSettingsForm'
import { useResetQueryCacheOnUserChange } from '../../auth/queries'

const dbRow = (over: Record<string, unknown> = {}) => ({
  id: 'r1', title: '방', description: null, host_id: 'h1', max_participants: 8, current_participants: 1,
  status: 'open', genre: '록', tags: ['초보환영'], free_join: null, instrument_slots: null, ...over,
})

const detail = (over: Partial<DetailRoom> = {}): DetailRoom => ({
  id: 'r1', title: '방', description: null, host_id: 'h1', max_participants: 8, current_participants: 1,
  status: 'open', genre: '록', tags: [], free_join: true, instrument_slots: [{ instrument: 'vocal', count: 1 }],
  is_private: false, has_password: false, ...over,
})

beforeEach(() => {
  vi.spyOn(window, 'alert').mockImplementation(() => {})
  vi.spyOn(console, 'error').mockImplementation(() => {})
  vi.spyOn(console, 'log').mockImplementation(() => {})
})

afterEach(() => {
  sb.mock!.reset()
  vi.clearAllMocks()
  vi.restoreAllMocks()
})

describe('방 상세 query', () => {
  it('get_room RPC로 읽고 기본값(자유참여, 기본 5슬롯)을 채운다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow()] })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomDetailQuery('r1'), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(sb.mock!.client.rpc).toHaveBeenCalledWith('get_room', { p_room_id: 'r1' })
    expect(result.current.data?.free_join).toBe(true)
    expect(result.current.data?.instrument_slots.map(s => s.instrument)).toEqual(['vocal', 'guitar', 'bass', 'keyboard', 'drums'])
  })

  it('없는 방이면 재시도 없이 오류', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [] })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomDetailQuery('nope'), { wrapper })

    await waitFor(() => expect(result.current.isError).toBe(true))
    expect((result.current.error as Error).message).toBe('room not found')
    expect(sb.mock!.client.rpc).toHaveBeenCalledTimes(1)
  })

  it('roomId가 없으면 조회하지 않는다', () => {
    const { wrapper } = createQueryWrapper()
    renderHook(() => useRoomDetailQuery(undefined), { wrapper })
    expect(sb.mock!.client.rpc).not.toHaveBeenCalled()
  })

  it('참여자 수 동기화: 서버가 기록한 값을 다시 읽어(쓰지 않음) 상세 캐시를 고치며 목록을 무효화한다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow({ current_participants: 3 })] })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.detail('r1'), detail())
    client.setQueryData(roomKeys.list(), [])
    const { result } = renderHook(() => useSyncParticipantsMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ roomId: 'r1' }))

    expect(sb.mock!.client.rpc).toHaveBeenCalledWith('get_room', { p_room_id: 'r1' })
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
    expect(client.getQueryData<DetailRoom>(roomKeys.detail('r1'))?.current_participants).toBe(3)
    expect(client.getQueryState(roomKeys.list())?.isInvalidated).toBe(true)
  })

  it('forgetRoomDetail은 그 방 상세 캐시만 지운다', () => {
    const { client } = createQueryWrapper()
    client.setQueryData(roomKeys.detail('r1'), detail())
    client.setQueryData(roomKeys.detail('r2'), detail({ id: 'r2' }))
    forgetRoomDetail(client, 'r1')
    expect(client.getQueryData(roomKeys.detail('r1'))).toBeUndefined()
    expect(client.getQueryData(roomKeys.detail('r2'))).toBeDefined()
  })
})

describe('useRoomDetailData', () => {
  it('방과 방장 닉네임(없으면 알 수 없음)을 준다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow()] })
    sb.mock!.setResult('profiles', { data: null })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomDetailData('r1'), { wrapper })

    expect(result.current.loading).toBe(true)
    expect(result.current.hostNickname).toBeNull()
    await waitFor(() => expect(result.current.hostNickname).toBe('알 수 없음'))
    expect(result.current.room?.id).toBe('r1')
    expect(result.current.loading).toBe(false)
    const profileCall = sb.mock!.calls.find(c => c.table === 'profiles')!
    expect(argsOf(profileCall, 'eq')).toEqual([['id', 'h1']])
  })

  it('방장 닉네임이 있으면 그대로', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow()] })
    sb.mock!.setResult('profiles', { data: { nickname: '방장님' } })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomDetailData('r1'), { wrapper })
    await waitFor(() => expect(result.current.hostNickname).toBe('방장님'))
  })

  it('방을 못 찾으면 목록으로 이동한다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [] })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomDetailData('nope'), { wrapper })

    await waitFor(() => expect(navigate).toHaveBeenCalledWith('/rooms'))
    expect(result.current.room).toBeNull()
    expect(result.current.loading).toBe(false)
  })
})

describe('합주실에 있는 동안 사용자가 바뀔 때 (리뷰 M1)', () => {
  it('다른 탭 로그아웃·세션 만료·계정 전환에도 방 화면이 로딩으로 바뀌거나 목록으로 이동하지 않는다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow()] })
    sb.mock!.setResult('profiles', { data: { nickname: '방장님' } })
    const { wrapper } = createQueryWrapper()
    const seen: { loading: boolean; room: boolean }[] = []
    const { result, rerender } = renderHook(({ userId }) => {
      useResetQueryCacheOnUserChange(userId, false)
      const data = useRoomDetailData('r1')
      seen.push({ loading: data.loading, room: data.room !== null })
      return data
    }, { wrapper, initialProps: { userId: 'u1' as string | null } })
    await waitFor(() => expect(result.current.hostNickname).toBe('방장님'))
    const rpcBefore = sb.mock!.client.rpc.mock.calls.length
    seen.length = 0

    rerender({ userId: null }) // 다른 탭에서 로그아웃 / refresh token 만료
    rerender({ userId: 'u2' }) // 다른 계정으로 로그인
    await act(async () => {})

    expect(seen.every(s => s.room && !s.loading)).toBe(true)
    expect(result.current.room?.id).toBe('r1')
    expect(result.current.hostNickname).toBe('방장님')
    expect(navigate).not.toHaveBeenCalled()
    expect(sb.mock!.client.rpc.mock.calls.length).toBe(rpcBefore) // 방 상세는 다시 읽지도 않는다
  })
})

describe('방 상세 재입장 (리뷰 L1)', () => {
  it('캐시가 있어도 마운트하면 다시 확인하고, 그 사이 삭제됐으면 목록으로 이동한다', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [] })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.detail('r1'), detail())
    const { result } = renderHook(() => useRoomDetailData('r1'), { wrapper })

    expect(result.current.room?.id).toBe('r1') // 캐시를 바로 보여주고
    await waitFor(() => expect(navigate).toHaveBeenCalledWith('/rooms'))
    expect(sb.mock!.client.rpc).toHaveBeenCalledTimes(1)
  })

  it('일시적 조회 오류면 보던 방을 유지한다', async () => {
    sb.mock!.setResult('rpc:get_room', { error: { message: 'no fn' } })
    sb.mock!.setResult('rooms', { error: { message: 'network' } })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.detail('r1'), detail())
    const { result } = renderHook(() => useRoomDetailData('r1'), { wrapper })

    await waitFor(() => expect(sb.mock!.calls.some(c => c.table === 'rooms')).toBe(true))
    await act(async () => {})
    expect(navigate).not.toHaveBeenCalled()
    expect(result.current.room?.id).toBe('r1')
  })

  it('방장 닉네임 조회가 실패하면 재시도 없이 바로 알 수 없음 (리뷰 L3)', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow()] })
    sb.mock!.setResult('profiles', { error: { message: 'rls' } })
    const { wrapper, client } = createQueryWrapper()
    client.setDefaultOptions({ queries: { ...client.getDefaultOptions().queries, retry: 1, retryDelay: 1000 } })
    const { result } = renderHook(() => useRoomDetailData('r1'), { wrapper })

    await waitFor(() => expect(result.current.hostNickname).toBe('알 수 없음'), { timeout: 500 })
    expect(sb.mock!.calls.filter(c => c.table === 'profiles')).toHaveLength(1)
  })
})

describe('useRoomSettingsForm', () => {
  it('저장: 방장 조건으로 rooms를 고치고, 비밀번호를 서버로 보낸 뒤 상세 캐시를 갱신한다', async () => {
    sb.mock!.setResult('rooms', { data: null })
    const { wrapper, client } = createQueryWrapper()
    const room = detail({ instrument_slots: [{ instrument: 'vocal', count: 1 }, { instrument: 'drums', count: 1 }] })
    client.setQueryData(roomKeys.detail('r1'), room)
    const { result } = renderHook(() => useRoomSettingsForm(room, 'r1', 'h1', true), { wrapper })

    act(() => result.current.openRoomSettings())
    act(() => {
      result.current.setEditTitle('  새 제목 ')
      result.current.handleSlotCountChange('drums', 0)
      result.current.setEditIsPrivate(true)
      result.current.setEditPasswordMode('set')
      result.current.setEditPassword('secret12')
    })
    await act(() => result.current.handleSaveRoomSettings())

    const update = sb.mock!.calls.find(c => c.chain.some(([m]) => m === 'update'))!
    expect(argsOf(update, 'eq')).toEqual([['id', 'r1'], ['host_id', 'h1']])
    const payload = argsOf(update, 'update')[0][0] as Record<string, unknown>
    expect(payload.title).toBe('새 제목')
    expect(payload.instrument_slots).toEqual([{ instrument: 'vocal', count: 1 }])
    expect(payload.is_private).toBe(true)
    expect(setRoomPassword).toHaveBeenCalledWith('r1', 'secret12')

    const cached = client.getQueryData<DetailRoom>(roomKeys.detail('r1'))!
    expect(cached.title).toBe('새 제목')
    expect(cached.is_private).toBe(true)
    expect(cached.has_password).toBe(true)
    expect(result.current.showRoomSettings).toBe(false)
  })

  it('마이그레이션 008 전 방(is_private 없음)에는 is_private를 보내지 않는다', async () => {
    sb.mock!.setResult('rooms', { data: null })
    const { wrapper } = createQueryWrapper()
    const room = detail({ is_private: undefined, has_password: undefined })
    const { result } = renderHook(() => useRoomSettingsForm(room, 'r1', 'h1', true), { wrapper })

    act(() => result.current.openRoomSettings())
    await act(() => result.current.handleSaveRoomSettings())

    const update = sb.mock!.calls.find(c => c.chain.some(([m]) => m === 'update'))!
    expect(argsOf(update, 'update')[0][0]).not.toHaveProperty('is_private')
    expect(setRoomPassword).not.toHaveBeenCalled()
  })

  it('잘못된 비밀번호면 저장하지 않고 알린다', async () => {
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomSettingsForm(detail(), 'r1', 'h1', true), { wrapper })

    act(() => result.current.openRoomSettings())
    act(() => {
      result.current.setEditPasswordMode('set')
      result.current.setEditPassword('1')
    })
    await act(() => result.current.handleSaveRoomSettings())

    expect(window.alert).toHaveBeenCalled()
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
    expect(result.current.showRoomSettings).toBe(true)
  })

  it('DB 저장이 실패하면 캐시를 바꾸지 않고 모달을 연 채 알린다', async () => {
    sb.mock!.setResult('rooms', { error: new Error('denied') })
    const { wrapper, client } = createQueryWrapper()
    const room = detail()
    client.setQueryData(roomKeys.detail('r1'), room)
    const { result } = renderHook(() => useRoomSettingsForm(room, 'r1', 'h1', true), { wrapper })

    act(() => result.current.openRoomSettings())
    act(() => result.current.setEditTitle('바뀐 제목'))
    await act(() => result.current.handleSaveRoomSettings())

    expect(window.alert).toHaveBeenCalledWith('설정 저장에 실패했습니다: denied')
    expect(client.getQueryData<DetailRoom>(roomKeys.detail('r1'))?.title).toBe('방')
    expect(result.current.showRoomSettings).toBe(true)
  })

  it('방장이 아니면 아무것도 하지 않는다', async () => {
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useRoomSettingsForm(detail(), 'r1', 'u2', false), { wrapper })
    act(() => result.current.openRoomSettings())
    await act(() => result.current.handleSaveRoomSettings())
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
  })
})

describe('useRoomHostActions', () => {
  it('삭제: 확인 후 방장 조건으로 지우고, 퇴장·이동한 뒤 상세 캐시를 지운다', async () => {
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    sb.mock!.setResult('rooms', { data: null })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(roomKeys.detail('r1'), detail())
    const leaveRoom = vi.fn()
    const { result } = renderHook(
      () => useRoomHostActions({ roomId: 'r1', userId: 'h1', isHost: true, peerCount: 2, leaveRoom }),
      { wrapper },
    )

    await act(() => result.current.handleDeleteRoom())

    const del = sb.mock!.calls.find(c => c.chain.some(([m]) => m === 'delete'))!
    expect(argsOf(del, 'eq')).toEqual([['id', 'r1'], ['host_id', 'h1']])
    expect(leaveRoom).toHaveBeenCalled()
    expect(navigate).toHaveBeenCalledWith('/rooms')
    expect(client.getQueryData(roomKeys.detail('r1'))).toBeUndefined()
  })

  it('삭제 취소면 아무것도 하지 않는다', async () => {
    vi.spyOn(window, 'confirm').mockReturnValue(false)
    const { wrapper } = createQueryWrapper()
    const leaveRoom = vi.fn()
    const { result } = renderHook(
      () => useRoomHostActions({ roomId: 'r1', userId: 'h1', isHost: true, peerCount: 0, leaveRoom }),
      { wrapper },
    )
    await act(() => result.current.handleDeleteRoom())
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
    expect(leaveRoom).not.toHaveBeenCalled()
  })

  it('동기화: 서버가 기록한 참여자 수를 보여준다 (클라이언트는 쓰지 않는다)', async () => {
    sb.mock!.setResult('rpc:get_room', { data: [dbRow({ current_participants: 4 })] })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(
      () => useRoomHostActions({ roomId: 'r1', userId: 'h1', isHost: true, peerCount: 2, leaveRoom: vi.fn() }),
      { wrapper },
    )
    await act(() => result.current.handleSyncParticipants())
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
    expect(window.alert).toHaveBeenCalledWith('서버 기준 참여자 수: 4명')
  })
})
