// @vitest-environment jsdom
// RoomProvider 조립 확인: 선택 구독(리렌더 범위), 디스패처 → 스토어, 합주실 입장 중 앱 업데이트 보류(appUpdater.setInRoom)
import { act, cleanup, render } from '@testing-library/react'
import { useSyncExternalStore } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PerformState } from '../../../native/performSession'
import type { SignalPayload } from './useSignaling'

const h = vi.hoisted(() => {
  const listeners = new Set<(s: unknown) => void>()
  let state = {
    phase: 'idle', instrument: null, sessionId: null, error: null, stats: null, inputMuted: false, relayError: null, metronome: null,
  } as Record<string, unknown>
  const session = {
    getState: () => state,
    subscribe: (l: (s: unknown) => void) => {
      listeners.add(l)
      return () => listeners.delete(l)
    },
    set(patch: Record<string, unknown>) {
      state = { ...state, ...patch }
      listeners.forEach((l) => l(state))
    },
    handleSignal: () => false,
    welcome: async () => {},
    connectionLost: () => {},
    stop: async () => {},
    performerAppeared: () => {},
    peerGone: () => {},
    clearError: () => {},
    setPeerGain: () => {},
    setInputMuted: () => {},
    setHostMetronome: async () => true,
    updateInstrument: () => {},
    dispose: () => {},
  }
  return {
    session,
    signaling: { handlers: null as null | { onMessage: (p: SignalPayload) => void }, send: vi.fn() },
    setInRoom: vi.fn(),
  }
})

vi.mock('../../auth', () => ({ useAuth: () => ({ user: null }) }))
vi.mock('../../settings', () => {
  const settings = { setEngineInUse: () => {}, issues: [], stopTest: async () => {}, config: {} }
  return { useAudioSettings: () => settings }
})
vi.mock('../../../native/updater', () => ({ appUpdater: { setInRoom: h.setInRoom } }))
vi.mock('../../../native/useNativeSession', () => ({
  useNativeSession: () => {
    const state = useSyncExternalStore(h.session.subscribe, h.session.getState) as unknown as PerformState
    return { session: h.session, state }
  },
}))
vi.mock('../../../native/mixBridge', () => ({
  MixSource: class {
    invalidate() {}
    resume() {}
  },
  MixBroadcaster: class {
    stop() {}
    start = async () => {}
    syncViewers() {}
    hasPeer() {
      return false
    }
  },
}))
vi.mock('./useSignaling', async (importOriginal) => {
  const { useRef } = await import('react')
  return {
    ...(await importOriginal<object>()),
    useSignaling: (handlers: { onMessage: (p: SignalPayload) => void }) => {
      h.signaling.handlers = handlers
      const clientIdRef = useRef<string | null>('me')
      return { clientIdRef, send: h.signaling.send, isOpen: () => true, authenticated: null }
    },
  }
})

import { RoomProvider } from './RoomProvider'
import { useMiniPlayer, useRoomChat, useRoomConnection, useRoomTelemetry, type RoomActions } from './roomHooks'

const renders = { chat: 0, mini: 0, stats: 0, connection: 0 }
let joinRoom: RoomActions['joinRoom'] = () => {}
let leaveRoom: RoomActions['leaveRoom'] = () => {}
let peers: string[] = []

function ChatProbe() {
  renders.chat++
  useRoomChat()
  return null
}
function MiniProbe() {
  renders.mini++
  useMiniPlayer()
  return null
}
function StatsProbe() {
  renders.stats++
  useRoomTelemetry('engineStats')
  return null
}
function ConnectionProbe() {
  renders.connection++
  const c = useRoomConnection()
  joinRoom = c.joinRoom
  leaveRoom = c.leaveRoom
  peers = c.peers
  return null
}

const message = (p: SignalPayload) => act(() => h.signaling.handlers!.onMessage(p))

beforeEach(() => {
  Object.assign(renders, { chat: 0, mini: 0, stats: 0, connection: 0 })
  h.signaling.send.mockClear()
  h.setInRoom.mockClear()
  render(
    <RoomProvider>
      <ChatProbe />
      <MiniProbe />
      <StatsProbe />
      <ConnectionProbe />
    </RoomProvider>,
  )
})
afterEach(() => cleanup())

describe('RoomProvider', () => {
  it('입장하면 join을 보내고 앱 업데이트 설치를 보류, 퇴장하면 해제한다', () => {
    expect(h.setInRoom).toHaveBeenLastCalledWith(false)
    act(() => joinRoom('r1'))
    expect(h.signaling.send).toHaveBeenCalledWith(expect.objectContaining({ type: 'join', roomId: 'r1' }))
    expect(h.setInRoom).toHaveBeenLastCalledWith(true)
    act(() => leaveRoom())
    expect(h.signaling.send).toHaveBeenLastCalledWith({ type: 'leave' })
    expect(h.setInRoom).toHaveBeenLastCalledWith(false)
  })

  it('room-state 메시지가 스토어를 거쳐 화면에 반영된다', () => {
    act(() => joinRoom('r1'))
    message({ type: 'room-state', peerIds: ['me', 'a'], participants: [] })
    expect(peers).toEqual(['a'])
  })

  it('엔진 통계가 바뀌면 통계를 구독한 컴포넌트만 다시 그린다', () => {
    act(() => h.session.set({ phase: 'running', instrument: 'guitar', stats: { peers: [], inputLevel: 0, outputLevel: 0 } }))
    const before = { ...renders }
    for (let i = 1; i <= 5; i++) {
      act(() => h.session.set({ stats: { peers: [], inputLevel: i / 10, outputLevel: 0 } }))
    }
    expect(renders.stats - before.stats).toBe(5)
    expect(renders.chat).toBe(before.chat)
    expect(renders.mini).toBe(before.mini)
    expect(renders.connection).toBe(before.connection)
  })

  it('채팅이 오면 채팅을 구독한 컴포넌트만 다시 그린다', () => {
    const before = { ...renders }
    message({ type: 'chat', oderId: 'a', message: 'hi', nickname: 'A', timestamp: 1 })
    expect(renders.chat - before.chat).toBe(1)
    expect(renders.stats).toBe(before.stats)
    expect(renders.mini).toBe(before.mini)
  })
})
