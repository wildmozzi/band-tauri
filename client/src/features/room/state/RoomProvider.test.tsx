// @vitest-environment jsdom
// RoomProvider 조립 확인: 선택 구독(리렌더 범위), 디스패처 → 스토어, 합주실 입장 중 앱 업데이트 보류(appUpdater.setInRoom)
import { act, cleanup, render } from '@testing-library/react'
import { Profiler, useContext } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PerformSession } from '../../../native/performSession'
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
    /** RoomProvider 본문이 실행된 횟수 (useAudioSettings는 Provider만 부른다) */
    providerRenders: 0,
  }
})

vi.mock('../../auth', () => ({ useAuth: () => ({ user: null }) }))
vi.mock('../../settings', () => {
  const settings = { setEngineInUse: () => {}, issues: [], stopTest: async () => {}, config: {} }
  return {
    useAudioSettings: () => {
      h.providerRenders++
      return settings
    },
  }
})
vi.mock('../../../native/updater', () => ({ appUpdater: { setInRoom: h.setInRoom } }))
vi.mock('../../../native/useNativeSession', () => ({
  // 실제 훅처럼 세션 상태는 돌려주지 않는다(Provider는 스토어 미러에서 읽는다)
  useNativeSession: () => ({ session: h.session as unknown as PerformSession }),
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
      return { clientIdRef, send: h.signaling.send, isOpen: () => true, authenticated: null, restart: () => {} }
    },
  }
})

import { FloatingMiniPlayer } from '../components/FloatingMiniPlayer'
import { ReconnectBanner } from '../components/ReconnectBanner'
import { RoomProvider } from './RoomProvider'
import {
  RoomRuntimeContext,
  useRoomChat,
  useRoomConnection,
  useRoomMixer,
  useRoomPerform,
  useRoomRecordings,
  useRoomRequests,
  useRoomTelemetry,
  type RoomActions,
} from './roomHooks'
import type { RoomSessionStore } from './roomSessionStore'

const renders = { chat: 0, mini: 0, detail: 0, stats: 0, levels: 0, connection: 0, banner: 0 }
let joinRoom: RoomActions['joinRoom'] = () => {}
let leaveRoom: RoomActions['leaveRoom'] = () => {}
let peers: string[] = []
let store: RoomSessionStore

function ChatProbe() {
  renders.chat++
  useRoomChat()
  return null
}
/** RoomDetail(화면 본체)이 구독하는 훅 다섯 개와 같다 */
function DetailProbe() {
  renders.detail++
  useRoomConnection()
  useRoomPerform()
  useRoomMixer()
  useRoomRequests()
  useRoomRecordings()
  return null
}
function StatsProbe() {
  renders.stats++
  useRoomTelemetry('engineStats')
  return null
}
function LevelsProbe() {
  renders.levels++
  useRoomTelemetry('audioLevels')
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
function StoreGrab() {
  store = useContext(RoomRuntimeContext)!.store
  return null
}

const message = (p: SignalPayload) => act(() => h.signaling.handlers!.onMessage(p))

beforeEach(() => {
  Object.assign(renders, { chat: 0, mini: 0, detail: 0, stats: 0, levels: 0, connection: 0, banner: 0 })
  h.providerRenders = 0
  h.signaling.send.mockClear()
  h.setInRoom.mockClear()
  h.session.set({ phase: 'idle', instrument: null, stats: null })
  render(
    <MemoryRouter>
      <RoomProvider>
        <StoreGrab />
        <ChatProbe />
        {/* 실제 미니 플레이어(훅 네 개 구독)를 그대로 그려 렌더 횟수를 센다 */}
        <Profiler id="mini" onRender={() => renders.mini++}>
          <FloatingMiniPlayer />
        </Profiler>
        <DetailProbe />
        {/* 재연결 안내 띠 (P1-15b) */}
        <Profiler id="banner" onRender={() => renders.banner++}>
          <ReconnectBanner />
        </Profiler>
        <StatsProbe />
        <LevelsProbe />
        <ConnectionProbe />
      </RoomProvider>
    </MemoryRouter>,
  )
})
afterEach(() => cleanup())

const enginePeer = (level: number) => ({ peerId: 'a', level }) as never

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

  it('엔진 통계(500ms)가 바뀌면 통계를 구독한 컴포넌트만 다시 그린다 — 미니 플레이어·화면 본체·Provider는 0', () => {
    act(() => joinRoom('r1'))
    act(() => store.patch('miniPlayer', { active: true }))
    act(() => h.session.set({ phase: 'running', instrument: 'guitar', stats: { peers: [enginePeer(0)], inputLevel: 0, outputLevel: 0 } }))
    const before = { ...renders }
    const providerBefore = h.providerRenders
    for (let i = 1; i <= 5; i++) {
      act(() => h.session.set({ stats: { peers: [enginePeer(i / 10)], inputLevel: i / 10, outputLevel: 0 } }))
    }
    expect(renders.stats - before.stats).toBe(5)
    expect(renders.levels - before.levels).toBe(5) // 연주 중 레벨 미터는 엔진 피어 레벨
    expect(renders.mini).toBe(before.mini)
    expect(renders.detail).toBe(before.detail)
    expect(renders.chat).toBe(before.chat)
    expect(renders.connection).toBe(before.connection)
    expect(renders.banner).toBe(before.banner)
    expect(h.providerRenders).toBe(providerBefore)
  })

  it('웹 믹서 레벨(66ms)이 바뀌면 레벨을 구독한 컴포넌트만 다시 그린다 — 미니 플레이어·화면 본체·Provider는 0', () => {
    act(() => joinRoom('r1'))
    act(() => store.patch('miniPlayer', { active: true }))
    const before = { ...renders }
    const providerBefore = h.providerRenders
    for (let i = 1; i <= 5; i++) {
      act(() => store.patch('levels', { audioLevels: { a: i / 10 }, masterLevel: i / 10 }))
    }
    expect(renders.levels - before.levels).toBe(5)
    expect(renders.stats).toBe(before.stats)
    expect(renders.mini).toBe(before.mini)
    expect(renders.detail).toBe(before.detail)
    expect(renders.chat).toBe(before.chat)
    expect(h.providerRenders).toBe(providerBefore)
  })

  it('녹음 시간(1s)이 바뀌어도 화면 본체(목록만 구독)는 다시 그리지 않는다', () => {
    act(() => store.patch('recording', { isRecording: true }))
    const before = { ...renders }
    for (let i = 1; i <= 3; i++) {
      act(() => store.patch('recording', { recordingDuration: i }))
    }
    expect(renders.detail).toBe(before.detail)
    expect(renders.mini).toBe(before.mini)
  })

  it('대기 요청이 없던 참가자가 나가도 요청 구독자는 그대로, 연결 구독자만 다시 그린다', () => {
    act(() => joinRoom('r1'))
    message({ type: 'room-state', peerIds: ['me', 'a'], participants: [] })
    const requestsBefore = store.getSnapshot().requests
    const before = { ...renders }
    message({ type: 'participant-left', oderId: 'a' })
    expect(peers).toEqual([])
    expect(store.getSnapshot().requests).toBe(requestsBefore)
    expect(renders.connection - before.connection).toBe(1)
  })

  it('채팅이 오면 채팅을 구독한 컴포넌트만 다시 그린다', () => {
    const before = { ...renders }
    message({ type: 'chat', oderId: 'a', message: 'hi', nickname: 'A', timestamp: 1 })
    expect(renders.chat - before.chat).toBe(1)
    expect(renders.stats).toBe(before.stats)
    expect(renders.mini).toBe(before.mini)
    expect(renders.detail).toBe(before.detail)
  })
})
