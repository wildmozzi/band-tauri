// @vitest-environment jsdom
// P1-15b 재연결 UX 통합: 실제 RoomProvider·useSignaling·RoomDetail·FloatingMiniPlayer에 가짜 WebSocket·가짜 타이머.
// 방 안에서 끊기면 화면은 그대로 + 상단 띠, 복구되면 띠가 사라진다. 포기하면 띠의 "다시 입장"이 새로고침 없이 같은 방에 입장한다.
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useNavigate, type NavigateFunction } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PerformSession } from '../../native/performSession'
import { FakeWebSocket, type Sent } from '../../shared/test/fakeWebSocket'
import { createTestQueryClient } from '../../shared/test/queryWrapper'
import type { DetailRoom } from './roomDetailModel'

const h = vi.hoisted(() => {
  const state = {
    phase: 'idle', instrument: null, sessionId: null, error: null, stats: null, inputMuted: false, relayError: null, metronome: null,
  }
  const session = {
    getState: () => state,
    subscribe: () => () => {},
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
  const room = {
    id: 'r1', title: '재연결 테스트 방', description: null, host_id: 'h1', max_participants: 8, current_participants: 1,
    status: 'open', genre: '록', tags: [], free_join: true, instrument_slots: [{ instrument: 'vocal', count: 1 }],
    is_private: false, has_password: false,
  }
  return { session, room }
})

vi.mock('../auth', async (importOriginal) => ({
  ...(await importOriginal<object>()),
  useAuth: () => ({ user: null, loading: false }),
}))
vi.mock('../settings', async (importOriginal) => {
  const settings = { setEngineInUse: () => {}, issues: [], stopTest: async () => {}, config: {} }
  return { ...(await importOriginal<object>()), useAudioSettings: () => settings }
})
vi.mock('../../native/updater', () => ({ appUpdater: { setInRoom: () => {} } }))
vi.mock('../../native/useNativeSession', () => ({
  useNativeSession: () => ({ session: h.session as unknown as PerformSession }),
}))
vi.mock('../../native/mixBridge', () => ({
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
vi.mock('../../shared/lib/supabaseClient', () => ({
  supabase: {
    auth: {
      getSession: async () => ({ data: { session: null } }),
      onAuthStateChange: () => ({ data: { subscription: { unsubscribe: () => {} } } }),
    },
  },
  isSupabaseConfigured: () => true,
}))
// 방 상세(서버 상태)는 고정값 — 이 테스트는 실시간 연결만 본다
vi.mock('./hooks/useRoomDetailData', () => ({
  useRoomDetailData: () => ({ room: h.room as DetailRoom, loading: false, hostNickname: '방장' }),
}))

import { RoomDetail } from './RoomDetail'
import { FloatingMiniPlayer } from './components/FloatingMiniPlayer'
import { RoomProvider } from './state/RoomProvider'
import { DUPLICATE_CONNECTION_CLOSE_REASON } from './state/reconnectPolicy'

const RECONNECTING = '서버 연결이 끊겨 다시 연결하는 중…'
const GAVE_UP = '서버 연결이 끊겼습니다'
const REPLACED = '다른 창이나 기기에서 이 합주실에 들어와 여기 연결은 종료됐습니다'

const last = () => FakeWebSocket.last()
const advance = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms))
const joinsSince = (n: number): Sent[] => FakeWebSocket.allSent().slice(n).filter((m) => m.type === 'join')
const title = () => screen.queryByRole('heading', { level: 1, name: h.room.title })

/** 서버가 연결을 받아 인증까지 끝내고, 앱이 보낸 join에 room-state로 답한다 */
async function accept(ws: FakeWebSocket, clientId: string) {
  await act(async () => {
    ws.open()
    ws.recv({ type: 'welcome', clientId })
  })
  await advance(0) // getAccessToken → auth
  await act(async () => ws.recv({ type: 'auth-result', authenticated: false }))
  await act(async () => ws.recv({ type: 'pong' }))
  await advance(0)
  if (ws.sent.some((m) => m.type === 'join')) {
    await act(async () => ws.recv({ type: 'room-state', peerIds: [clientId], participants: [], isHost: false }))
  }
}

/** 새 연결이 생길 때까지 진행 */
async function nextSocket() {
  const n = FakeWebSocket.instances.length
  while (FakeWebSocket.instances.length === n) await advance(100)
  return last()
}

/** 새로 만들어지는 연결을 계속 실패시키며 포기할 때까지 진행 */
async function failUntilGaveUp() {
  for (let t = 0; t < 40_000 && !screen.queryByText(GAVE_UP); t += 100) {
    const n = FakeWebSocket.instances.length
    await advance(100)
    if (FakeWebSocket.instances.length > n) act(() => last().fail())
  }
  expect(screen.getByText(GAVE_UP)).toBeTruthy()
}

let navigate: NavigateFunction
function NavGrab() {
  navigate = useNavigate()
  return null
}

beforeEach(async () => {
  vi.useFakeTimers()
  FakeWebSocket.reset()
  vi.stubGlobal('WebSocket', FakeWebSocket)
  vi.spyOn(console, 'log').mockImplementation(() => {})
  vi.spyOn(console, 'warn').mockImplementation(() => {})
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <MemoryRouter initialEntries={['/rooms/r1']}>
        <RoomProvider>
          <NavGrab />
          <Routes>
            <Route path="/rooms/:roomId" element={<RoomDetail />} />
            <Route path="/elsewhere" element={<div>다른 화면</div>} />
          </Routes>
          <FloatingMiniPlayer />
        </RoomProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  )
  // 첫 연결 → 자동 입장(useRoomPresence) → 입장 완료
  expect(screen.getByText('서버에 연결 중...')).toBeTruthy()
  await accept(last(), 'c1')
  expect(joinsSince(0)).toHaveLength(1)
  expect(title()).toBeTruthy()
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  vi.useRealTimers()
})

describe('재연결 UX (P1-15b)', () => {
  it('평상시에는 띠가 없다', () => {
    expect(screen.queryByRole('status')).toBeNull()
    expect(screen.queryByText(RECONNECTING, { exact: false })).toBeNull()
  })

  it('방 안에서 끊기면 합주실 화면은 그대로 + 띠, 재연결되면 띠가 사라진다 (재입장 join 1회)', async () => {
    const sentBefore = FakeWebSocket.allSent().length
    act(() => last().fail())
    expect(screen.getByText(RECONNECTING, { exact: false })).toBeTruthy()
    expect(title()).toBeTruthy()
    expect(document.querySelector('.chat-panel')).not.toBeNull()
    expect(screen.queryByText('서버에 연결 중...')).toBeNull()
    expect(screen.queryByText('시그널링 서버 연결에 실패했습니다.')).toBeNull()
    // 웹 관람(엔진 없음)은 오디오가 계속된다고 하지 않는다
    expect(screen.queryByText('오디오는 계속됩니다', { exact: false })).toBeNull()

    // 재연결 시도 중(connecting)에도 화면 유지
    const ws = await nextSocket()
    expect(screen.getByText(RECONNECTING, { exact: false })).toBeTruthy()
    expect(title()).toBeTruthy()

    await accept(ws, 'c2')
    expect(screen.queryByText(RECONNECTING, { exact: false })).toBeNull()
    expect(title()).toBeTruthy()
    const joins = joinsSince(sentBefore)
    expect(joins).toHaveLength(1)
    expect(joins[0]).toMatchObject({ roomId: 'r1', isRejoin: true })
  })

  it('포기하면 띠가 "서버 연결이 끊겼습니다" + 다시 입장 → 새로고침 없이 같은 방에 입장(join 1회) → 띠 사라짐', async () => {
    act(() => last().fail())
    await failUntilGaveUp()
    expect(title()).toBeTruthy()
    expect(screen.getByRole('alert').textContent).toContain(GAVE_UP)
    const sentBefore = FakeWebSocket.allSent().length
    const n = FakeWebSocket.instances.length

    fireEvent.click(screen.getByRole('button', { name: '다시 입장' }))
    expect(FakeWebSocket.instances.length).toBe(n + 1)
    expect(screen.getByText(RECONNECTING, { exact: false })).toBeTruthy()

    await accept(last(), 'c9')
    const joins = joinsSince(sentBefore)
    expect(joins).toHaveLength(1)
    expect(joins[0]).toMatchObject({ roomId: 'r1' })
    expect(joins[0].isRejoin).toBeUndefined()
    expect(screen.queryByText(GAVE_UP)).toBeNull()
    expect(screen.queryByText(RECONNECTING, { exact: false })).toBeNull()
    expect(title()).toBeTruthy()
  })

  it('미니 플레이어에도 재연결 중·연결 끊김을 작게 표시하고, 누르면 합주실로 돌아가 띠가 보인다', async () => {
    act(() => navigate('/elsewhere'))
    const mini = () => document.querySelector('.floating-mini-player') as HTMLElement | null
    expect(mini()).not.toBeNull()
    expect(document.querySelector('.mini-player-signal')).toBeNull()

    act(() => last().fail())
    expect(mini()!.textContent).toContain('재연결 중')
    await failUntilGaveUpMini()
    expect(mini()!.textContent).toContain('연결 끊김')

    // 툴팁은 누르면 일어나는 일(합주실로 돌아가기)을 말한다. 다시 입장은 띠에서 한다
    expect(document.querySelector('.mini-player-signal')!.getAttribute('title')).toContain('눌러서 합주실로 돌아가기')
    expect(document.querySelector('.mini-player-signal')!.getAttribute('title')).not.toContain('다시 입장')

    fireEvent.click(mini()!)
    expect(title()).toBeTruthy()
    expect(screen.getByText(GAVE_UP)).toBeTruthy()
    expect(screen.getByRole('button', { name: '다시 입장' })).toBeTruthy()
  })

  it('다른 창·기기에 밀려나면 원인을 알리고, "여기서 다시 입장"은 확인 한 번 뒤에만 입장한다 (리뷰 M-1)', async () => {
    act(() => last().serverClose(1000, DUPLICATE_CONNECTION_CLOSE_REASON))
    expect(screen.getByRole('alert').textContent).toContain(REPLACED)
    expect(screen.queryByText(GAVE_UP)).toBeNull()
    expect(title()).toBeTruthy()
    const n = FakeWebSocket.instances.length
    await advance(10_000)
    expect(FakeWebSocket.instances.length).toBe(n) // 자동 재연결 없음 (상대를 밀어내지 않는다)

    // 첫 클릭은 확인만 묻는다 — 취소하면 원래 안내로
    fireEvent.click(screen.getByRole('button', { name: '여기서 다시 입장' }))
    expect(FakeWebSocket.instances.length).toBe(n)
    expect(screen.getByText('다른 창이나 기기의 연결은 종료됩니다', { exact: false })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '취소' }))
    expect(screen.getByText(REPLACED)).toBeTruthy()
    expect(FakeWebSocket.instances.length).toBe(n)

    // 확인하면 새 연결 → 일반 입장 join 1회
    const sentBefore = FakeWebSocket.allSent().length
    fireEvent.click(screen.getByRole('button', { name: '여기서 다시 입장' }))
    fireEvent.click(screen.getByRole('button', { name: '확인' }))
    expect(FakeWebSocket.instances.length).toBe(n + 1)
    await accept(last(), 'c5')
    const joins = joinsSince(sentBefore)
    expect(joins).toHaveLength(1)
    expect(joins[0].isRejoin).toBeUndefined()
    expect(screen.queryByText(REPLACED)).toBeNull()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('미니 플레이어에 밀려남을 표시한다', async () => {
    act(() => navigate('/elsewhere'))
    act(() => last().serverClose(1000, DUPLICATE_CONNECTION_CLOSE_REASON))
    const signal = document.querySelector('.mini-player-signal.replaced')!
    expect(signal.textContent).toContain('다른 곳에서 입장')
    expect(signal.getAttribute('title')).toContain('눌러서 합주실로 돌아가기')
  })

  it('재연결 중 미니 플레이어에서 방을 나가고 다른 방에 들어가면 새로고침 없이 입장한다 (리뷰 M-2)', async () => {
    act(() => navigate('/elsewhere'))
    act(() => last().fail())
    expect(document.querySelector('.mini-player-signal.reconnecting')).not.toBeNull()
    const n = FakeWebSocket.instances.length
    fireEvent.click(screen.getByTitle('합주실 나가기'))
    expect(document.querySelector('.floating-mini-player')).toBeNull()
    expect(FakeWebSocket.instances.length).toBe(n + 1) // 백오프를 기다리지 않고 바로 다시 연다

    const sentBefore = FakeWebSocket.allSent().length
    act(() => navigate('/rooms/r2'))
    await accept(last(), 'c7')
    const joins = joinsSince(sentBefore)
    expect(joins).toHaveLength(1)
    expect(joins[0]).toMatchObject({ roomId: 'r2' })
    expect(screen.queryByText('시그널링 서버에 연결 중입니다', { exact: false })).toBeNull()
    expect(title()).toBeTruthy()
    expect(screen.queryByRole('status')).toBeNull()
  })
})

/** 미니 플레이어 모드(합주실 화면 없음)에서 포기할 때까지 */
async function failUntilGaveUpMini() {
  for (let t = 0; t < 40_000 && !document.querySelector('.mini-player-signal.gave-up'); t += 100) {
    const n = FakeWebSocket.instances.length
    await advance(100)
    if (FakeWebSocket.instances.length > n) act(() => last().fail())
  }
  expect(document.querySelector('.mini-player-signal.gave-up')).not.toBeNull()
}
