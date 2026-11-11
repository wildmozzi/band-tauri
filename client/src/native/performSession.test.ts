import { describe, expect, it, vi } from 'vitest'
import type { EngineConfig, EngineStats, LocalDescriptor, RelayConfig, RelayStats } from './engineApi'
import { DEFAULT_ENGINE_CONFIG } from './engineConfig'
import { PerformSession, performErrorFromCode, type EngineBackend, type SignalOut } from './performSession'

const desc = (ssrc: number): LocalDescriptor => ({ ssrc, key: `k${ssrc}`, candidates: [{ kind: 'lan', addr: '10.0.0.1:5000' }] })
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const last = <T,>(a: T[]): T => a[a.length - 1]
const flush = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve()
}

const stats = (running: boolean): EngineStats => ({
  running, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 10, peers: [],
})

class FakeEngine implements EngineBackend {
  calls: string[] = []
  active = 0
  maxActive = 0
  /** 다음 start를 수동으로 끝내고 싶으면 true */
  holdStart = false
  private release: (() => void) | null = null
  startFail: string | null = null
  addPeerFailures = 0
  running = false
  ssrc = 1
  statsRunning = true
  relayConfigs: RelayConfig[] = []
  relayFail: string | null = null
  relayStats: RelayStats | null = null

  private async track<T>(name: string, fn: () => Promise<T>): Promise<T> {
    this.calls.push(name)
    this.active++
    this.maxActive = Math.max(this.maxActive, this.active)
    try {
      return await fn()
    } finally {
      this.active--
    }
  }
  finishStart() {
    this.release?.()
  }
  start(_config: EngineConfig, peerId: string) {
    return this.track(`start:${peerId}`, async () => {
      if (this.holdStart) await new Promise<void>((r) => { this.release = r })
      if (this.startFail) throw this.startFail
      if (this.running) throw 'already running'
      this.running = true
      return desc(this.ssrc++)
    })
  }
  stop() {
    return this.track('stop', async () => {
      this.running = false
      return null
    })
  }
  addPeer(peerId: string) {
    return this.track(`add:${peerId}`, async () => {
      if (this.addPeerFailures > 0) {
        this.addPeerFailures--
        throw 'bind race'
      }
      return null
    })
  }
  removePeer(peerId: string) {
    return this.track(`remove:${peerId}`, async () => null)
  }
  setRelay(relay: RelayConfig | null) {
    return this.track(`relay:${relay ? `${relay.addr}:${relay.token.slice(0, 4)}` : 'null'}`, async () => {
      if (relay) this.relayConfigs.push(relay)
      if (this.relayFail) throw this.relayFail
      return null
    })
  }
  setPeerGain(peerId: string, gain: number, muted: boolean, pan?: number) {
    return this.track(`gain:${peerId}:${gain}:${muted}:${pan}`, async () => null)
  }
  setInputMuted(muted: boolean) {
    return this.track(`mute:${muted}`, async () => null)
  }
  stats() {
    return this.track('stats', async () => ({ ...stats(this.statsRunning), relay: this.relayStats }))
  }
}

function setup(opts: { others?: string[] } = {}) {
  const engine = new FakeEngine()
  const sent: SignalOut[] = []
  const env = { selfId: 'me' as string | null, roomId: 'room' as string | null, others: opts.others ?? [] }
  const session = new PerformSession(engine, {
    send: (p) => sent.push(p),
    getSelfId: () => env.selfId,
    getRoomId: () => env.roomId,
    getOtherPerformers: () => env.others,
  }, { retryDelaysMs: [5, 5, 5], reconnectTimeoutMs: 30, deadStatsThreshold: 2, relayRetryMs: 20 })
  const start = () => session.start({ instrument: 'guitar', config: DEFAULT_ENGINE_CONFIG, nickname: 'n', isHost: false })
  const types = () => sent.map((m) => m.type + ('to' in m ? `>${m.to}` : ''))
  return { engine, sent, env, session, start, types }
}

describe('PerformSession 시작/정지', () => {
  it('정상 시작: 엔진 → start-performing → descriptor 브로드캐스트', async () => {
    const { session, start, types, engine } = setup({ others: ['B'] })
    expect(await start()).toBe(true)
    expect(session.getState()).toMatchObject({ phase: 'running', instrument: 'guitar', sessionId: 'me' })
    expect(types()).toEqual(['start-performing', 'native-descriptor'])
    expect(engine.calls).toEqual(['start:me'])
  })

  it('엔진 시작 실패: idle + 에러, 아무것도 보내지 않음', async () => {
    const { session, start, sent, engine } = setup()
    engine.startFail = 'ASIO 장치 없음'
    expect(await start()).toBe(false)
    expect(session.getState().phase).toBe('idle')
    expect(session.getState().error).toContain('ASIO 장치 없음')
    expect(sent).toEqual([])
  })

  it('H2: 시작 중 stop → 시작이 끝나면 엔진을 닫고 연주 상태가 되지 않는다', async () => {
    const { session, start, sent, engine } = setup()
    engine.holdStart = true
    const p = start()
    await flush()
    expect(session.getState().phase).toBe('starting')
    await session.stop({ notify: true })
    expect(session.getState().phase).toBe('idle')
    engine.finishStart()
    expect(await p).toBe(false)
    expect(engine.calls).toEqual(['start:me', 'stop'])
    expect(engine.running).toBe(false)
    expect(sent).toEqual([])
    expect(session.getState().phase).toBe('idle')
  })

  it('H2: 시작 중 퇴장(roomId 없음) → 엔진을 닫는다', async () => {
    const { session, start, sent, engine, env } = setup()
    engine.holdStart = true
    const p = start()
    await flush()
    env.roomId = null
    engine.finishStart()
    expect(await p).toBe(false)
    expect(engine.running).toBe(false)
    expect(sent).toEqual([])
    expect(session.getState().phase).toBe('idle')
  })

  it('H2: 시작 중 clientId가 바뀌면(재연결) 엔진을 닫는다', async () => {
    const { start, engine, env, sent } = setup()
    engine.holdStart = true
    const p = start()
    await flush()
    env.selfId = 'new-id'
    engine.finishStart()
    expect(await p).toBe(false)
    expect(engine.running).toBe(false)
    expect(sent).toEqual([])
  })

  it('정지: native-leave + stop-performing 후 engine_stop (notify=false면 전송 없음)', async () => {
    const a = setup()
    await a.start()
    await a.session.stop({ notify: true })
    expect(a.types()).toEqual(['start-performing', 'native-descriptor', 'native-leave', 'stop-performing'])
    expect(last(a.engine.calls)).toBe('stop')
    const b = setup()
    await b.start()
    await b.session.stop({ notify: false })
    expect(b.types()).toEqual(['start-performing', 'native-descriptor'])
    expect(b.engine.running).toBe(false)
  })

  it('L6: 엔진 커맨드는 한 번에 하나씩 (stop 직후 start도 겹치지 않음)', async () => {
    const { session, start, engine } = setup()
    await start()
    const stopP = session.stop({ notify: false })
    const startP = start()
    await Promise.all([stopP, startP])
    expect(engine.maxActive).toBe(1)
    expect(engine.calls).toEqual(['start:me', 'stop', 'start:me'])
    expect(session.getState().phase).toBe('running')
  })
})

describe('PerformSession 서버 거부 (H3)', () => {
  for (const code of ['perform-not-approved', 'room-full-performers', 'login-required']) {
    it(`${code} → 연주 상태 롤백 + 메시지, stop-performing은 보내지 않음`, async () => {
      const { session, start, types, engine } = setup()
      await start()
      expect(session.handleSignal({ type: 'error', code })).toBe(true)
      await flush()
      const st = session.getState()
      expect(st.phase).toBe('idle')
      expect(st.instrument).toBeNull()
      expect(st.error).toBe(performErrorFromCode(code)!.message)
      expect(engine.running).toBe(false)
      expect(types()).toEqual(['start-performing', 'native-descriptor'])
    })
  }

  it('서버 메시지가 있으면 그 문구를 쓴다', () => {
    expect(performErrorFromCode('room-full-performers', '연주자는 최대 6명입니다.')!.message).toBe('연주자는 최대 6명입니다.')
    expect(performErrorFromCode('rate-limited')).toBeNull()
  })

  it('연주 중이 아닐 때의 거부는 메시지만 표시', async () => {
    const { session, engine } = setup()
    expect(session.handleSignal({ type: 'error', code: 'login-required' })).toBe(true)
    expect(session.getState().error).toBe('연주하려면 로그인이 필요합니다.')
    expect(engine.calls).toEqual([])
  })

  it('다른 에러 코드는 처리하지 않는다', () => {
    const { session } = setup()
    expect(session.handleSignal({ type: 'error', code: 'rate-limited' })).toBe(false)
  })
})

describe('PerformSession WS 재연결 (M6 + 세션 재개)', () => {
  it('resumed && 같은 id → 엔진 유지, 같은 descriptor 재브로드캐스트', async () => {
    const { session, start, sent, engine } = setup({ others: ['B'] })
    await start()
    const first = sent.find((m) => m.type === 'native-descriptor')!.descriptor
    session.connectionLost()
    await session.welcome('me', true)
    expect(engine.calls).toEqual(['start:me'])
    const lastMsg = last(sent)
    expect(lastMsg.type).toBe('native-descriptor')
    expect(lastMsg.to).toBeUndefined()
    expect(lastMsg.descriptor).toEqual(first)
    expect(session.getState().phase).toBe('running')
    await sleep(50) // 재연결 타이머가 취소됐는지
    expect(session.getState().phase).toBe('running')
  })

  it('재개 안 됨(새 id) → 이전 엔진 정지 후 새 id로 재시작, 같은 악기로 start-performing', async () => {
    const { session, start, sent, engine, env } = setup()
    await start()
    session.updateInstrument('drums')
    session.connectionLost()
    env.selfId = 'new'
    await session.welcome('new', false)
    expect(engine.calls).toEqual(['start:me', 'stop', 'start:new'])
    expect(session.getState()).toMatchObject({ phase: 'running', sessionId: 'new', instrument: 'drums' })
    const starts = sent.filter((m) => m.type === 'start-performing')
    expect(last(starts).instrument).toBe('drums')
    // 이전 연결로 native-leave/stop-performing을 보내지 않음
    expect(sent.some((m) => m.type === 'stop-performing')).toBe(false)
  })

  it('resumed라도 id가 다르면 재시작', async () => {
    const { session, start, engine, env } = setup()
    await start()
    env.selfId = 'other'
    await session.welcome('other', true)
    expect(engine.calls).toEqual(['start:me', 'stop', 'start:other'])
  })

  it('재연결이 시간 안에 안 되면 연주를 정지하고 안내', async () => {
    const { session, start, engine } = setup()
    await start()
    session.connectionLost()
    await sleep(60)
    expect(session.getState().phase).toBe('idle')
    expect(session.getState().error).toContain('연결이 끊겨')
    expect(engine.running).toBe(false)
  })

  it('연주 중이 아니면 welcome은 아무것도 하지 않는다', async () => {
    const { session, engine } = setup()
    await session.welcome('me', false)
    expect(engine.calls).toEqual([])
  })
})

describe('PerformSession 피어/엔진 상태', () => {
  it('M1: add_peer 실패를 백오프로 재시도해 결국 등록', async () => {
    const { session, start, engine } = setup()
    await start()
    engine.addPeerFailures = 2
    session.handleSignal({ type: 'native-descriptor', from: 'B', descriptor: desc(50) })
    // 백오프 타이머는 실시간이라 CI 부하에서 60ms 고정 대기는 간헐 실패했다 — 조건이 맞을 때까지 기다린다.
    await vi.waitFor(() => expect(engine.calls.filter((c) => c === 'add:B')).toHaveLength(3), { timeout: 3000, interval: 10 })
    expect(session.peers).toEqual(['B'])
  })

  it('M1: 재시도 중 피어가 나가면 재시도 중단', async () => {
    const { session, start, engine } = setup()
    await start()
    engine.addPeerFailures = 5
    session.handleSignal({ type: 'native-descriptor', from: 'B', descriptor: desc(50) })
    await flush()
    session.peerGone('B')
    await sleep(40)
    expect(engine.calls.filter((c) => c === 'add:B')).toHaveLength(1)
  })

  it('L7: engine_stats.running=false가 연속되면 정지 + 다른 사람에게 알림', async () => {
    const { session, start, engine, types } = setup()
    await start()
    engine.statsRunning = false
    await session.pollStats()
    expect(session.getState().phase).toBe('running')
    await session.pollStats()
    expect(session.getState().phase).toBe('idle')
    expect(session.getState().error).toContain('엔진이 멈췄')
    expect(types().slice(-2)).toEqual(['native-leave', 'stop-performing'])
  })

  it('stats가 정상이면 상태에 반영', async () => {
    const { session, start } = setup()
    await start()
    await session.pollStats()
    expect(session.getState().stats?.running).toBe(true)
  })

  it('L2: 피어 pan과 입력 뮤트를 엔진에 전달, 재시작 시 뮤트 유지', async () => {
    const { session, start, engine } = setup()
    session.setInputMuted(true)
    expect(engine.calls).toEqual([]) // 연주 전에는 상태만
    await start()
    await flush()
    session.setPeerGain('B', 0.5, false, -0.3)
    await flush()
    expect(engine.calls).toEqual(['start:me', 'mute:true', 'gain:B:0.5:false:-0.3'])
  })

  it('연주 중이 아닐 때 받은 descriptor는 무시', () => {
    const { session, engine } = setup()
    session.handleSignal({ type: 'native-descriptor', from: 'B', descriptor: desc(5) })
    expect(engine.calls).toEqual([])
  })
})

describe('PerformSession 릴레이 토큰 (P1-4b)', () => {
  const token = 'A'.repeat(70)
  const relayMsg = (ssrc: number, expiresAt: number, t = token) => ({ type: 'relay-token', token: t, addr: 'relay.example.com:3478', ssrc, expiresAt })

  it('내 ssrc의 relay-token → engine_set_relay(addr, token)', async () => {
    const { session, start, engine } = setup()
    await start() // ssrc 1
    expect(session.handleSignal(relayMsg(1, Math.floor(Date.now() / 1000) + 120))).toBe(true)
    await flush()
    expect(engine.calls).toEqual(['start:me', 'relay:relay.example.com:3478:AAAA'])
  })

  it('다른 ssrc(재시작 전 엔진)·잘못된 형식·연주 전 토큰은 무시', async () => {
    const { session, start, engine } = setup()
    const exp = Math.floor(Date.now() / 1000) + 120
    expect(session.handleSignal(relayMsg(1, exp))).toBe(true) // 시작 전
    await start()
    session.handleSignal(relayMsg(99, exp))
    session.handleSignal(relayMsg(1, exp, 'short'))
    session.handleSignal({ type: 'relay-token', token, addr: 'no-port', ssrc: 1, expiresAt: exp })
    await flush()
    expect(engine.calls).toEqual(['start:me'])
  })

  it('PC 시계가 서버보다 5분 느려도 받은 시각 + expiresIn 기준으로 갱신을 예약하고, 엔진에 남은 수명을 넘긴다(M1)', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(1_700_000_000_000))
    try {
      const { session, start, types, engine } = setup()
      await start()
      const serverNow = Math.floor(Date.now() / 1000) + 300 // 서버 시계가 5분 앞섬
      session.handleSignal({ ...relayMsg(1, serverNow + 120), expiresIn: 120, serverNow })
      await flush()
      expect(engine.relayConfigs[0].expiresInSec).toBe(120)
      // 만료 60초 전 = 받은 뒤 60초 (내 시계로 계산했다면 360초 뒤 → 이미 만료)
      await vi.advanceTimersByTimeAsync(59_999)
      expect(types()).not.toContain('relay-token-request')
      await vi.advanceTimersByTimeAsync(1)
      expect(types().filter((t) => t === 'relay-token-request')).toHaveLength(1)
      await session.stop()
    } finally {
      vi.useRealTimers()
    }
  })

  it('엔진 릴레이가 expired/unreachable이면 바로 새 토큰을 요청(간격 제한)', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(1_700_000_000_000))
    try {
      const { session, start, types, engine } = setup()
      await start()
      session.handleSignal({ ...relayMsg(1, Math.floor(Date.now() / 1000) + 120), expiresIn: 120 })
      const requests = () => types().filter((t) => t === 'relay-token-request').length
      engine.relayStats = { state: 'bound', addr: '1.2.3.4:3478', rttMs: 20, txKbps: 100, overBudget: false }
      await session.pollStats()
      expect(requests()).toBe(0)
      engine.relayStats = { ...engine.relayStats, state: 'unreachable' }
      await session.pollStats()
      expect(requests()).toBe(1)
      await session.pollStats()
      expect(requests()).toBe(1) // relayRetryMs(20ms) 안에는 한 번만
      await vi.advanceTimersByTimeAsync(20)
      expect(requests()).toBe(2) // 응답 없음 → 재시도 타이머
      engine.relayStats = { ...engine.relayStats, state: 'rejected' }
      await vi.advanceTimersByTimeAsync(5)
      const before = requests()
      await session.pollStats()
      expect(requests()).toBe(before) // 거절(정원 등)은 새 토큰으로 풀리지 않는다
      await session.stop()
    } finally {
      vi.useRealTimers()
    }
  })

  it('engine_set_relay 실패 사유를 상태로 보관하고, 성공하면 지운다(L5)', async () => {
    const { session, start, engine } = setup()
    await start()
    engine.relayFail = '허용되지 않는 릴레이 주소'
    session.handleSignal({ ...relayMsg(1, Math.floor(Date.now() / 1000) + 120), expiresIn: 120 })
    for (let i = 0; i < 3; i++) await flush()
    expect(session.getState().relayError).toBe('허용되지 않는 릴레이 주소')
    engine.relayFail = null
    session.handleSignal({ ...relayMsg(1, Math.floor(Date.now() / 1000) + 120), expiresIn: 120 })
    for (let i = 0; i < 3; i++) await flush()
    expect(session.getState().relayError).toBeNull()
    await session.stop()
  })

  it('만료 60초 전(짧은 수명이면 절반)에 relay-token-request, 응답이 없으면 다시 요청, 새 토큰이 오면 다시 예약', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(1_700_000_000_000))
    try {
      const { session, start, types } = setup()
      await start()
      const now = Math.floor(Date.now() / 1000)
      session.handleSignal(relayMsg(1, now + 60)) // 옛 서버(expiresIn 없음): 내 시계로 60초 → 절반(30초) 지점
      await vi.advanceTimersByTimeAsync(29_999)
      expect(types()).not.toContain('relay-token-request')
      await vi.advanceTimersByTimeAsync(1)
      expect(types().filter((t) => t === 'relay-token-request')).toHaveLength(1)
      await vi.advanceTimersByTimeAsync(20) // relayRetryMs
      expect(types().filter((t) => t === 'relay-token-request')).toHaveLength(2)
      // 새 토큰 → 재시도 중단, 새 만료 기준으로 예약
      session.handleSignal(relayMsg(1, Math.floor(Date.now() / 1000) + 120))
      await vi.advanceTimersByTimeAsync(59_000) // 새 토큰 120초 → 60초 뒤 갱신
      expect(types().filter((t) => t === 'relay-token-request')).toHaveLength(2)
      // 정지하면 타이머도 멈춘다
      await session.stop()
      await vi.advanceTimersByTimeAsync(120_000)
      expect(types().filter((t) => t === 'relay-token-request')).toHaveLength(2)
    } finally {
      vi.useRealTimers()
    }
  })

  it('ssrc-conflict → 엔진을 새 ssrc로 재시작(native-leave 없이), 연주 상태 유지', async () => {
    const { session, start, engine, types } = setup()
    await start()
    expect(session.handleSignal({ type: 'error', code: 'ssrc-conflict', message: 'x' })).toBe(true)
    for (let i = 0; i < 5; i++) await flush()
    expect(engine.calls).toEqual(['start:me', 'stop', 'start:me'])
    expect(session.getState().phase).toBe('running')
    expect(types()).not.toContain('native-leave')
    // 새 엔진(ssrc 2)의 토큰만 받는다
    session.handleSignal(relayMsg(1, Math.floor(Date.now() / 1000) + 120))
    session.handleSignal(relayMsg(2, Math.floor(Date.now() / 1000) + 120))
    await flush()
    expect(engine.calls.filter((c) => c.startsWith('relay:'))).toHaveLength(1)
  })
})
