// @vitest-environment jsdom
// P1-15 재연결 통합: 실제 useSignaling + RoomProvider에 가짜 WebSocket·가짜 타이머.
// 서버 세션 유예(30초) 안의 단절은 세션 재개로 이어 붙이고(엔진 유지), 예산(35초)을 넘기면 포기 안내 + 엔진 정지.
import { act, cleanup, render } from '@testing-library/react'
import { useContext } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PerformSession } from '../../../native/performSession'

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
  /** 설정하면 getSession이 이 Promise가 풀릴 때까지 기다린다 (토큰 대기 중 닫힘 재현) */
  const auth = { tokenGate: null as Promise<void> | null }
  return { session, auth }
})

vi.mock('../../auth', () => ({ useAuth: () => ({ user: null }) }))
vi.mock('../../settings', () => {
  const settings = { setEngineInUse: () => {}, issues: [], stopTest: async () => {}, config: {} }
  return { useAudioSettings: () => settings }
})
vi.mock('../../../native/updater', () => ({ appUpdater: { setInRoom: () => {} } }))
vi.mock('../../../native/useNativeSession', () => ({
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
// 로그인 상태 (토큰이 있어야 resumeClientId를 보낸다)
vi.mock('../../../shared/lib/supabaseClient', () => ({
  supabase: {
    auth: {
      getSession: async () => {
        if (h.auth.tokenGate) await h.auth.tokenGate
        return { data: { session: { access_token: 'tok' } } }
      },
      onAuthStateChange: () => ({ data: { subscription: { unsubscribe: () => {} } } }),
    },
  },
}))

import { RoomProvider } from './RoomProvider'
import { RoomRuntimeContext, useRoomConnection, type RoomActions } from './roomHooks'
import type { RoomSessionStore } from './roomSessionStore'
import { IDLE_PING_MS, PONG_TIMEOUT_MS, STALE_CLOSE_CODE } from './reconnectPolicy'

const AUTH_FALLBACK_MS = 2000 // useSignaling과 같은 값

type Sent = Record<string, unknown> & { type: string }

class FakeWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3
  static instances: FakeWebSocket[] = []
  /** true면 반쯤 열린 연결처럼 close()가 닫기 핸드셰이크만 시작하고 onclose를 부르지 않는다 */
  static halfOpen = false
  readyState = FakeWebSocket.CONNECTING
  sent: Sent[] = []
  closedWith: number | null = null
  onopen: (() => void) | null = null
  onclose: ((e: { code: number }) => void) | null = null
  onerror: (() => void) | null = null
  onmessage: ((e: { data: string }) => void) | null = null
  constructor(public url: string) {
    FakeWebSocket.instances.push(this)
  }
  send(data: string) {
    this.sent.push(JSON.parse(data))
  }
  /** 클라이언트가 닫음 */
  close(code?: number) {
    if (this.readyState === FakeWebSocket.CLOSED) return
    const wasConnecting = this.readyState === FakeWebSocket.CONNECTING
    this.closedWith = code ?? 1005
    if (FakeWebSocket.halfOpen && !wasConnecting) {
      this.readyState = FakeWebSocket.CLOSING
      return
    }
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.({ code: wasConnecting ? 1006 : (code ?? 1005) })
  }
  // --- 서버/네트워크 쪽 조작 ---
  open() {
    this.readyState = FakeWebSocket.OPEN
    this.onopen?.()
  }
  fail() {
    this.readyState = FakeWebSocket.CLOSED
    this.onerror?.()
    this.onclose?.({ code: 1006 })
  }
  recv(msg: Record<string, unknown>) {
    this.onmessage?.({ data: JSON.stringify(msg) })
  }
  recvRaw(data: string) {
    this.onmessage?.({ data })
  }
}

const last = () => FakeWebSocket.instances[FakeWebSocket.instances.length - 1]
const allSent = () => FakeWebSocket.instances.flatMap((w) => w.sent)
const flush = () => act(() => vi.advanceTimersByTimeAsync(0))
const advance = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms))

let store: RoomSessionStore
let actions: Pick<RoomActions, 'joinRoom'>
function Grab() {
  store = useContext(RoomRuntimeContext)!.store
  actions = useRoomConnection()
  return null
}

/** 서버가 연결을 받아들이고 인증까지 끝냄. resumed면 이전 세션을 이어 붙인 것처럼 응답 */
async function accept(ws: FakeWebSocket, opts: { resumeAs?: string; pong?: boolean } = {}) {
  await act(async () => {
    ws.open()
    ws.recv({ type: 'welcome', clientId: `tmp${FakeWebSocket.instances.length}` })
  })
  await flush() // getAccessToken → auth 전송
  const auth = ws.sent.find((m) => m.type === 'auth')
  expect(auth).toBeDefined()
  await act(async () => {
    if (opts.resumeAs) {
      ws.recv({ type: 'welcome', clientId: opts.resumeAs, resumed: true })
      ws.recv({ type: 'auth-result', authenticated: true, resumed: true })
    } else {
      ws.recv({ type: 'auth-result', authenticated: true })
    }
  })
  // ready 직후의 pong 지원 확인 ping
  if (opts.pong !== false && ws.sent.some((m) => m.type === 'ping')) await act(async () => ws.recv({ type: 'pong' }))
  return auth!
}

/** 방에 들어가 있는 상태(clientId c1)를 만든다 */
async function enterRoom() {
  const ws = last()
  await act(async () => {
    ws.open()
    ws.recv({ type: 'welcome', clientId: 'c1' })
  })
  await flush()
  await act(async () => ws.recv({ type: 'auth-result', authenticated: true }))
  await act(async () => ws.recv({ type: 'pong' }))
  act(() => actions.joinRoom('r1'))
  await act(async () => ws.recv({ type: 'room-state', peerIds: ['c1'], participants: [], isHost: false }))
  expect(ws.sent.filter((m) => m.type === 'join')).toHaveLength(1)
  return ws
}

/** 새로 만들어지는 연결을 계속 실패시키며 ms만큼 진행. 실패시킨 연결 수를 돌려준다 */
async function failFor(ms: number) {
  const until = Date.now() + ms
  let failed = 0
  while (Date.now() < until) {
    const n = FakeWebSocket.instances.length
    await advance(100)
    if (FakeWebSocket.instances.length > n) {
      act(() => last().fail())
      failed++
    }
  }
  return failed
}

let stopSpy: ReturnType<typeof vi.spyOn>
let welcomeSpy: ReturnType<typeof vi.spyOn>

beforeEach(() => {
  vi.useFakeTimers()
  FakeWebSocket.instances = []
  FakeWebSocket.halfOpen = false
  h.auth.tokenGate = null
  vi.stubGlobal('WebSocket', FakeWebSocket)
  stopSpy = vi.spyOn(h.session, 'stop')
  welcomeSpy = vi.spyOn(h.session, 'welcome')
  vi.spyOn(console, 'log').mockImplementation(() => {})
  vi.spyOn(console, 'warn').mockImplementation(() => {})
  render(
    <MemoryRouter>
      <RoomProvider>
        <Grab />
      </RoomProvider>
    </MemoryRouter>,
  )
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  vi.useRealTimers()
})

describe('WS 재연결 (시간 예산)', () => {
  it('25초 동안 연결이 실패하다 성공하면 세션을 재개한다 — resumeClientId, 엔진 정지 0회, 재입장 1회', async () => {
    const first = await enterRoom()
    const sentBefore = allSent().length
    act(() => first.fail())
    const failed = await failFor(25_000)
    expect(failed).toBeGreaterThanOrEqual(6) // 0.5, 1, 2, 4, 4, 4 … 초 백오프
    expect(store.getSnapshot().connection.signalStatus).not.toBe('gave-up')

    // 다음 시도는 성공 (서버 유예 30초 안)
    const n = FakeWebSocket.instances.length
    while (FakeWebSocket.instances.length === n) await advance(100)
    const auth = await accept(last(), { resumeAs: 'c1' })

    expect(auth).toMatchObject({ type: 'auth', accessToken: 'tok', resumeClientId: 'c1' })
    const joinsAfter = allSent().slice(sentBefore).filter((m) => m.type === 'join')
    expect(joinsAfter).toHaveLength(1)
    expect(joinsAfter[0]).toMatchObject({ roomId: 'r1', isRejoin: true })
    expect(stopSpy).not.toHaveBeenCalled()
    expect(welcomeSpy).toHaveBeenLastCalledWith('c1', true)
    expect(store.getSnapshot().connection).toMatchObject({ signalStatus: 'connected', clientId: 'c1', currentRoomId: 'r1' })
  })

  it('35초를 넘기면 포기: signalStatus=gave-up, 엔진 정지, 더 이상 연결하지 않는다', async () => {
    const first = await enterRoom()
    act(() => first.fail())
    await failFor(36_000)
    expect(store.getSnapshot().connection.signalStatus).toBe('gave-up')
    expect(stopSpy).toHaveBeenCalledTimes(1)
    expect(stopSpy).toHaveBeenCalledWith({ notify: false })
    const n = FakeWebSocket.instances.length
    await advance(60_000)
    expect(FakeWebSocket.instances.length).toBe(n)
  })

  it('열리기만 하고 ready가 안 되는 연결은 예산을 리셋하지 않는다', async () => {
    const first = await enterRoom()
    act(() => first.fail())
    const until = Date.now() + 36_000
    while (Date.now() < until) {
      const n = FakeWebSocket.instances.length
      await advance(100)
      if (FakeWebSocket.instances.length > n) {
        const ws = last()
        act(() => ws.open()) // 열리자마자
        act(() => ws.fail()) // 죽는 서버 (재시작 중)
      }
    }
    expect(store.getSnapshot().connection.signalStatus).toBe('gave-up')
  })

  it('15초 침묵 → ping, 5초 무응답 → close(4000) → 재연결', async () => {
    const ws = await enterRoom()
    const pingsBefore = ws.sent.filter((m) => m.type === 'ping').length
    await advance(IDLE_PING_MS - 100)
    expect(ws.sent.filter((m) => m.type === 'ping')).toHaveLength(pingsBefore)
    await advance(100)
    expect(ws.sent.filter((m) => m.type === 'ping')).toHaveLength(pingsBefore + 1)
    expect(ws.closedWith).toBeNull()
    await advance(PONG_TIMEOUT_MS)
    expect(ws.closedWith).toBe(STALE_CLOSE_CODE)

    // 비정상 닫힘이므로 재연결하고, 세션을 재개한다 (엔진 유지)
    const n = FakeWebSocket.instances.length
    await advance(1000)
    expect(FakeWebSocket.instances.length).toBe(n + 1)
    const auth = await accept(last(), { resumeAs: 'c1' })
    expect(auth.resumeClientId).toBe('c1')
    expect(stopSpy).not.toHaveBeenCalled()
  })

  it('ping에 pong이 오면 연결을 유지한다', async () => {
    const ws = await enterRoom()
    for (let i = 0; i < 3; i++) {
      await advance(IDLE_PING_MS)
      await act(async () => ws.recv({ type: 'pong' }))
    }
    await advance(PONG_TIMEOUT_MS)
    expect(ws.closedWith).toBeNull()
    expect(FakeWebSocket.instances).toHaveLength(1)
  })

  it('pong을 모르는 구 서버면 침묵만으로 닫지 않는다', async () => {
    const ws = last()
    await act(async () => {
      ws.open()
      ws.recv({ type: 'welcome', clientId: 'c1' })
    })
    await flush()
    await act(async () => ws.recv({ type: 'auth-result', authenticated: true })) // 이후 pong 없음
    act(() => actions.joinRoom('r1'))
    await advance((IDLE_PING_MS + PONG_TIMEOUT_MS) * 3)
    expect(ws.closedWith).toBeNull()
    expect(FakeWebSocket.instances).toHaveLength(1)
  })

  it('반쯤 열린 연결: close()가 onclose를 부르지 않아도 수신 없음 판정 즉시 재연결하고, 늦은 옛 onclose는 무시한다', async () => {
    FakeWebSocket.halfOpen = true
    const ws = await enterRoom()
    const lateOnclose = ws.onclose!
    await advance(IDLE_PING_MS + PONG_TIMEOUT_MS)
    expect(ws.closedWith).toBe(STALE_CLOSE_CODE)
    expect(ws.readyState).toBe(FakeWebSocket.CLOSING) // 브라우저는 아직 onclose를 보내지 않음
    expect(ws.onclose).toBeNull()
    expect(store.getSnapshot().connection.signalStatus).toBe('idle')

    // 첫 백오프(0.5초 ±20%) 안에 새 연결
    const n = FakeWebSocket.instances.length
    await advance(600)
    expect(FakeWebSocket.instances.length).toBe(n + 1)
    await accept(last(), { resumeAs: 'c1' })
    expect(store.getSnapshot().connection).toMatchObject({ signalStatus: 'connected', clientId: 'c1' })

    // 브라우저 닫기 핸드셰이크 타임아웃 뒤 늦게 도착한 옛 onclose
    act(() => lateOnclose({ code: 1006 }))
    expect(store.getSnapshot().connection).toMatchObject({ signalStatus: 'connected', clientId: 'c1', currentRoomId: 'r1' })
    await advance(5_000)
    expect(FakeWebSocket.instances.length).toBe(n + 1)
    expect(stopSpy).not.toHaveBeenCalled()
  })

  it('토큰을 기다리는 사이 닫힌 연결은 ready로 보지 않는다 — auth 미전송, 예산 리셋 없음 → 35초 뒤 포기', async () => {
    const first = await enterRoom()
    const readyCalls = welcomeSpy.mock.calls.length
    act(() => first.fail())
    const dropped: FakeWebSocket[] = []
    const until = Date.now() + 36_000
    while (Date.now() < until) {
      const n = FakeWebSocket.instances.length
      await advance(100)
      if (FakeWebSocket.instances.length > n) {
        const ws = last()
        let release!: () => void
        h.auth.tokenGate = new Promise<void>((r) => (release = r))
        act(() => ws.open()) // getSession 대기 중
        act(() => ws.fail()) // 그 사이 닫힘
        release()
        h.auth.tokenGate = null
        await flush()
        await advance(AUTH_FALLBACK_MS) // fallback이 걸렸다면 여기서 ready
        dropped.push(ws)
      }
    }
    expect(dropped.length).toBeGreaterThanOrEqual(3)
    for (const ws of dropped) expect(ws.sent.filter((m) => m.type === 'auth')).toHaveLength(0)
    expect(welcomeSpy.mock.calls.length).toBe(readyCalls) // onReady 없음
    expect(store.getSnapshot().connection.signalStatus).toBe('gave-up')
  })

  it('서버 유예가 지나 새 clientId를 받으면(resumed:false) 엔진을 새 id로 한 번 재시작하고 재입장한다', async () => {
    const first = await enterRoom()
    const sentBefore = allSent().length
    const welcomesBefore = welcomeSpy.mock.calls.length
    act(() => first.fail())
    const n = FakeWebSocket.instances.length
    while (FakeWebSocket.instances.length === n) await advance(100)
    const auth = await accept(last()) // 서버가 재개하지 않음 → 새 id
    expect(auth.resumeClientId).toBe('c1') // 재개는 요청했다
    const newId = `tmp${FakeWebSocket.instances.length}`
    const welcomesAfter = welcomeSpy.mock.calls.slice(welcomesBefore)
    expect(welcomesAfter).toEqual([[newId, false]])
    expect(stopSpy).not.toHaveBeenCalled()
    const joinsAfter = allSent().slice(sentBefore).filter((m) => m.type === 'join')
    expect(joinsAfter).toHaveLength(1)
    expect(joinsAfter[0]).toMatchObject({ roomId: 'r1', isRejoin: true })
    expect(store.getSnapshot().connection).toMatchObject({ signalStatus: 'connected', clientId: newId })
  })

  it('침묵 판정은 pong이 아닌 모든 수신(파싱 실패 포함)으로도 리셋된다', async () => {
    const ws = await enterRoom()
    const pings = () => ws.sent.filter((m) => m.type === 'ping').length
    const before = pings()
    await advance(IDLE_PING_MS - 1000)
    await act(async () => ws.recv({ type: 'peer-joined', peerId: 'x' }))
    await advance(IDLE_PING_MS - 1000)
    await act(async () => ws.recvRaw('not json'))
    await advance(IDLE_PING_MS - 1000)
    expect(pings()).toBe(before)
    await advance(1000)
    expect(pings()).toBe(before + 1)
  })

  it('언마운트하면 타이머가 남지 않는다 (연결 유지 중)', async () => {
    await enterRoom()
    await advance(IDLE_PING_MS) // idle → pong 대기 타이머까지 무장
    expect(vi.getTimerCount()).toBeGreaterThan(0)
    cleanup()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('언마운트하면 타이머가 남지 않는다 (재연결 대기 중)', async () => {
    const first = await enterRoom()
    act(() => first.fail()) // 재연결 타이머 대기
    expect(vi.getTimerCount()).toBeGreaterThan(0)
    cleanup()
    expect(vi.getTimerCount()).toBe(0)
  })
})
