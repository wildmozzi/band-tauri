// @vitest-environment jsdom
// CreateRoom에서 분리한 입력 상태 훅: 태그·악기 슬롯 조작, 제출 검사·요청·이동
import { act, renderHook } from '@testing-library/react'
import type { FormEvent } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

const navigate = vi.hoisted(() => vi.fn())
vi.mock('react-router-dom', () => ({ useNavigate: () => navigate }))

const api = vi.hoisted(() => ({ createRoom: vi.fn<(input: unknown) => Promise<{ id: string }>>(async () => ({ id: 'room-1' })) }))
vi.mock('./roomApi', () => api)

import { useCreateRoomForm } from './useCreateRoomForm'

const submitEvent = () => ({ preventDefault: vi.fn() }) as unknown as FormEvent<HTMLFormElement>

afterEach(() => vi.clearAllMocks())

describe('useCreateRoomForm', () => {
  it('기본 악기 5슬롯, 추가·삭제·개수 조절', () => {
    const { result } = renderHook(() => useCreateRoomForm())
    expect(result.current.totalSlots).toBe(5)
    expect(result.current.availableToAdd.map(i => i.id)).toEqual(['other'])
    act(() => result.current.handleAddInstrument('other'))
    act(() => result.current.handleSlotCountChange('other', 99))
    expect(result.current.totalSlots).toBe(15)
    act(() => result.current.handleRemoveInstrument('vocal'))
    expect(result.current.instrumentSlots.map(s => s.instrument)).toEqual(['guitar', 'bass', 'keyboard', 'drums', 'other'])
  })

  it('커스텀 태그는 추가되면 입력을 비우고, 중복이면 그대로 둔다', () => {
    const { result } = renderHook(() => useCreateRoomForm())
    act(() => result.current.setCustomTag(' 재즈 '))
    act(() => result.current.handleAddCustomTag())
    expect(result.current.selectedTags).toEqual(['재즈'])
    expect(result.current.customTag).toBe('')
    act(() => result.current.setCustomTag('재즈'))
    act(() => result.current.handleAddCustomTag())
    expect(result.current.selectedTags).toEqual(['재즈'])
    expect(result.current.customTag).toBe('재즈')
  })

  it('제목이 없으면 요청하지 않고 오류를 보인다', async () => {
    const { result } = renderHook(() => useCreateRoomForm())
    await act(() => result.current.handleSubmit(submitEvent()))
    expect(result.current.error).toBe('방 제목을 입력하세요.')
    expect(api.createRoom).not.toHaveBeenCalled()
  })

  it('성공하면 만든 방으로 이동, 실패하면 오류 문구', async () => {
    const { result } = renderHook(() => useCreateRoomForm())
    act(() => result.current.setTitle(' 세션 '))
    await act(() => result.current.handleSubmit(submitEvent()))
    expect(api.createRoom).toHaveBeenCalledWith(expect.objectContaining({ title: '세션', password: null }))
    expect(navigate).toHaveBeenCalledWith('/rooms/room-1')

    api.createRoom.mockRejectedValueOnce(new Error('서버 오류'))
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})
    await act(() => result.current.handleSubmit(submitEvent()))
    expect(result.current.error).toBe('서버 오류')
    expect(result.current.loading).toBe(false)
    spy.mockRestore()
  })
})
