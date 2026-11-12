/**
 * 연결 수명 특성 테스트 — docs/server-architecture.md 3절 S0 "지금 테스트가 없는 흐름".
 * 하트비트 타임아웃, 대기 메시지 상한(MAX_PENDING_MESSAGES=100), IP당 연결 수가 닫힐 때 줄어드는지.
 * (토큰 만료 4001·재인증 승격은 ws-identity.test.ts)
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import { WebSocket, type WebSocketServer } from 'ws'
import { initializeWebSocket, type WsDeps, type WsOptions } from '../src/services/wsService'
import { connect, testVerifier, waitFor, type TestClient } from './helpers'
import { PROBE_ROOM, settle } from './wsSettle'

const servers: { server: Server; wss: WebSocketServer }[] = []
const sockets: WebSocket[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}, deps: Partial<WsDeps> = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits: { joinBurst: 1000, joinPerSec: 1000, maxAnonymousPerIpPerRoom: 100, ...limits },
    deps: {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async (roomId) => (roomId === PROBE_ROOM ? null : { hostId: 'host', freeJoin: true }),
      updateParticipants: async () => {},
      ...deps,
    },
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

async function client(url: string) {
  const c = await connect(url)
  openClients.push(c)
  return c
}

/** 연결 시도: welcome을 받으면 그 소켓, 서버가 먼저 닫으면 닫힘 코드 */
function tryConnect(url: string, options: { autoPong?: boolean } = {}): Promise<{ ws: WebSocket; welcome: boolean; code: number | null }> {
  return new Promise(resolve => {
    const ws = new WebSocket(url, options)
    sockets.push(ws)
    ws.on('message', (d) => {
      if (JSON.parse(d.toString()).type === 'welcome') resolve({ ws, welcome: true, code: null })
    })
    ws.on('close', (code) => resolve({ ws, welcome: false, code }))
    ws.on('error', () => {})
  })
}

const closed = (ws: WebSocket) => new Promise<{ code: number; reason: string }>(r => {
  if (ws.readyState === ws.CLOSED) return r({ code: -1, reason: 'already closed' })
  ws.on('close', (code, reason) => r({ code, reason: reason.toString() }))
})

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const ws of sockets) ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('heartbeat', async () => {
  // 한 주기 안에 pong이 와야 한다. 같은 프로세스라 주기를 너무 짧게 잡으면 이벤트 루프 지연으로 정상 연결도 끊길 수 있다
  const url = await startWs({ heartbeatMs: 300 })

  it('terminates a connection that does not answer pings, keeps one that does', async () => {
    const alive = await client(url)
    alive.send({ type: 'auth', accessToken: 'tok:alive' })
    await settle(alive)

    const silent = await tryConnect(url, { autoPong: false })
    assert.ok(silent.welcome)
    const res = await Promise.race([closed(silent.ws), new Promise<null>(r => setTimeout(() => r(null), 5000))])
    assert.ok(res, 'silent connection was not terminated')
    assert.equal(res.code, 1006, 'terminate() — no close frame')

    // 응답하는 연결은 그 사이 여러 주기를 지나도 열려 있다
    await settle(alive)
    assert.equal(alive.closedCode, null)
  })

  it('a heartbeat-terminated member leaves the room (anonymous, not suspended)', async () => {
    const watcher = await client(url)
    watcher.send({ type: 'auth', accessToken: 'tok:watcher' })
    watcher.send({ type: 'join', roomId: 'hb' })
    await settle(watcher)

    const silent = await tryConnect(url, { autoPong: false })
    silent.ws.send(JSON.stringify({ type: 'auth' }))
    silent.ws.send(JSON.stringify({ type: 'join', roomId: 'hb' }))
    assert.ok(await watcher.waitFor(m => m.type === 'participant-joined'))
    const silentId = watcher.inbox.find(m => m.type === 'participant-joined').participant.oderId
    assert.ok(await watcher.waitFor(m => m.type === 'participant-left' && m.oderId === silentId, 5000))
  })
})

describe('pending message cap (100 per connection)', async () => {
  const gates = new Map<string, () => void>()
  const blockedVerify = async (token: string) => {
    if (token.startsWith('tok:slow')) await new Promise<void>(r => gates.set(token, r))
    return testVerifier(token)
  }
  // 대기 상한만 보도록 속도 제한은 넉넉히
  const url = await startWs({ msgBurst: 10_000, msgPerSec: 10_000 }, { verifyToken: blockedVerify })
  after(() => { for (const release of gates.values()) release() })

  it('keeps a connection with up to 100 queued messages', async () => {
    const c = await client(url)
    c.send({ type: 'auth', accessToken: 'tok:slow-a' }) // 처리 중(verifyToken 대기) = 대기 1
    for (let i = 0; i < 99; i++) c.send({ type: 'noop' }) // 대기 100
    assert.ok(await waitFor(() => gates.has('tok:slow-a')))
    // 99개가 풀어 주기 전에 모두 도착했는지는 볼 수단이 없다(늦게 오면 대기 수가 100 미만). 상한 자체는 다음 테스트가 본다
    gates.get('tok:slow-a')!()
    await settle(c)
    assert.equal(c.closedCode, null)
    assert.deepEqual(c.inbox.filter(m => m.type !== 'welcome'), [{ type: 'auth-result', authenticated: true, userId: 'slow-a' }])
  })

  it('closes with 1008 when the 101st message arrives while 100 are pending', async () => {
    const c = await client(url)
    const done = closed(c.ws)
    c.send({ type: 'auth', accessToken: 'tok:slow-b' })
    for (let i = 0; i < 100; i++) c.send({ type: 'noop' })
    const res = await done
    assert.equal(res.code, 1008)
    assert.equal(res.reason, 'Too many pending messages')
  })
})

describe('per-IP connection count is released on close', async () => {
  const url = await startWs({ maxConnectionsPerIp: 2 })

  it('a closed or terminated connection frees its slot; rejected attempts do not consume one', async () => {
    const a = await client(url)
    const b = await client(url)
    for (const c of [a, b]) {
      c.send({ type: 'auth' })
      c.send({ type: 'join', roomId: 'ip' })
      await settle(c)
    }

    // 상한: 세 번째는 1008. 거부된 시도는 자리를 쓰지도 돌려주지도 않는다
    for (let i = 0; i < 2; i++) {
      const r = await tryConnect(url)
      assert.equal(r.welcome, false)
      assert.equal(r.code, 1008)
    }

    // 정상 종료: 서버의 close 처리(카운트 감소 → 퇴장 알림)가 끝난 것은 b가 participant-left를 받은 것으로 안다
    a.close()
    assert.ok(await b.waitFor(m => m.type === 'participant-left' && m.oderId === a.id))
    const d = await tryConnect(url)
    assert.ok(d.welcome, 'slot freed by close')
    assert.equal((await tryConnect(url)).code, 1008, 'only one slot was freed')

    // 비정상 종료(terminate)도 자리를 돌려준다
    d.ws.send(JSON.stringify({ type: 'auth' }))
    d.ws.send(JSON.stringify({ type: 'join', roomId: 'ip' }))
    assert.ok(await b.waitFor(m => m.type === 'participant-joined'))
    const dId = b.inbox.find(m => m.type === 'participant-joined').participant.oderId
    d.ws.terminate()
    assert.ok(await b.waitFor(m => m.type === 'participant-left' && m.oderId === dId))
    const e = await tryConnect(url)
    assert.ok(e.welcome, 'slot freed by terminate')
    assert.equal((await tryConnect(url)).code, 1008)
  })
})
