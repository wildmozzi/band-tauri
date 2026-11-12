// @vitest-environment jsdom
// RoomDetail에서 분리한 상태 훅: 연주 전환 흐름, 거절 사유 입력, 말풍선 위치
import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

const navigate = vi.hoisted(() => vi.fn())
vi.mock('react-router-dom', () => ({ useNavigate: () => navigate }))

const auth = vi.hoisted(() => ({ user: { id: 'u1' } as { id: string } | null }))
vi.mock('../../auth/useAuth', () => ({ useAuth: () => ({ user: auth.user, loading: false }) }))

const room = vi.hoisted(() => ({
  canPerform: true,
  startPerforming: vi.fn(async () => true),
  stopPerforming: vi.fn(async () => {}),
  myRequestStatus: 'none' as string,
  myRequestInstrument: null as string | null,
  requestPerform: vi.fn(),
  cancelRequest: vi.fn(),
}))
vi.mock('../state/RoomContext', () => ({ useRoom: () => room }))

import { usePerformControls } from './usePerformControls'
import { useRejectPicker } from './useRejectPicker'
import { useBubblePositions } from './useBubblePositions'

afterEach(() => {
  vi.clearAllMocks()
  auth.user = { id: 'u1' }
  room.canPerform = true
  room.myRequestStatus = 'none'
  room.myRequestInstrument = null
})

describe('usePerformControls', () => {
  it('비로그인이면 로그인 화면으로 (돌아올 방 기억)', () => {
    auth.user = null
    const { result } = renderHook(() => usePerformControls('r1', true, false))
    act(() => result.current.handleBecomePerformer())
    expect(navigate).toHaveBeenCalledWith('/auth', { state: { from: '/rooms/r1' } })
    expect(result.current.showInstrumentSelect).toBe(false)
  })

  it('웹(연주 불가)이면 악기 선택을 열지 않는다', () => {
    room.canPerform = false
    const { result } = renderHook(() => usePerformControls('r1', true, false))
    act(() => result.current.handleBecomePerformer())
    expect(result.current.showInstrumentSelect).toBe(false)
  })

  it('자유참여 방: 악기 선택 → 엔진 시작', async () => {
    const { result } = renderHook(() => usePerformControls('r1', true, false))
    act(() => result.current.handleBecomePerformer())
    expect(result.current.showInstrumentSelect).toBe(true)

    await act(() => result.current.handleSelectInstrument('guitar'))
    expect(result.current.showInstrumentSelect).toBe(false)
    expect(room.startPerforming).toHaveBeenCalledWith('guitar')
    expect(room.requestPerform).not.toHaveBeenCalled()
    expect(result.current.performBusy).toBe(false)
  })

  it('승인 필요 방(방장 아님): 방장에게 요청만 보낸다', async () => {
    const { result } = renderHook(() => usePerformControls('r1', false, false))
    await act(() => result.current.handleSelectInstrument('bass'))
    expect(room.requestPerform).toHaveBeenCalledWith('bass')
    expect(room.startPerforming).not.toHaveBeenCalled()
  })

  it('승인 필요 방이라도 방장은 바로 시작', async () => {
    const { result } = renderHook(() => usePerformControls('r1', false, true))
    await act(() => result.current.handleSelectInstrument('drums'))
    expect(room.startPerforming).toHaveBeenCalledWith('drums')
  })

  it('승인 후 시작은 요청했던 악기로', async () => {
    room.myRequestInstrument = 'keyboard'
    const { result } = renderHook(() => usePerformControls('r1', false, false))
    await act(() => result.current.handleStartAfterApproval())
    expect(room.startPerforming).toHaveBeenCalledWith('keyboard')
  })

  it('엔진 시작 중에는 다시 시작하지 않는다', async () => {
    let finish!: () => void
    room.startPerforming.mockImplementationOnce(() => new Promise<boolean>(r => { finish = () => r(true) }))
    const { result } = renderHook(() => usePerformControls('r1', true, false))
    let first!: Promise<void>
    act(() => { first = result.current.handleSelectInstrument('guitar') })
    expect(result.current.performBusy).toBe(true)
    await act(() => result.current.handleSelectInstrument('bass'))
    expect(room.startPerforming).toHaveBeenCalledTimes(1)
    await act(async () => { finish(); await first })
    expect(result.current.performBusy).toBe(false)
  })

  it('관람자로 전환: 엔진 정지 + 대기 중 요청 취소', () => {
    room.myRequestStatus = 'pending'
    const { result } = renderHook(() => usePerformControls('r1', false, false))
    act(() => result.current.handleBecomeViewer())
    expect(room.stopPerforming).toHaveBeenCalled()
    expect(room.cancelRequest).toHaveBeenCalled()
  })
})

describe('useRejectPicker', () => {
  it('거절 사유를 고르고 확정하면 초기화된다', () => {
    const reject = vi.fn()
    const { result } = renderHook(() => useRejectPicker(reject))
    act(() => result.current.startReject('p1'))
    act(() => result.current.setRejectReason('자리 없음'))
    expect(result.current.rejectingId).toBe('p1')
    act(() => result.current.confirmReject('p1', result.current.rejectReason))
    expect(reject).toHaveBeenCalledWith('p1', '자리 없음')
    expect(result.current).toMatchObject({ rejectingId: null, rejectReason: '' })
  })

  it('취소하면 요청을 보내지 않는다', () => {
    const reject = vi.fn()
    const { result } = renderHook(() => useRejectPicker(reject))
    act(() => result.current.startReject('p1'))
    act(() => result.current.cancelReject())
    expect(result.current.rejectingId).toBeNull()
    expect(reject).not.toHaveBeenCalled()
  })
})

describe('useBubblePositions', () => {
  it('요청이 있으면 연주자 행 오른쪽에 말풍선 좌표를 계산하고, 해제 시 리스너를 뗀다', () => {
    const el = document.createElement('div')
    el.getBoundingClientRect = () => ({ top: 10, height: 20, right: 100 } as DOMRect)
    const remove = vi.spyOn(window, 'removeEventListener')
    const { result, rerender, unmount } = renderHook(({ pending }) => useBubblePositions(pending), {
      initialProps: { pending: [] as { oderId: string }[] },
    })
    result.current.performerRefs.current.p1 = el
    expect(result.current.bubblePositions).toEqual({})

    rerender({ pending: [{ oderId: 'p1' }, { oderId: 'missing' }] })
    expect(result.current.bubblePositions).toEqual({ p1: { top: 20, left: 108 } })

    unmount()
    expect(remove).toHaveBeenCalledWith('resize', expect.any(Function))
  })
})
