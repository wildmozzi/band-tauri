/**
 * 남용·자원 회귀 테스트 (서버 보안·부하 점검 fix/d-server-audit).
 * - 읽지 않는 수신자: 보내기 대기열이 MAX_BUFFERED_BYTES를 넘으면 끊는다(전에는 5쌍으로 40초에 241MB가 쌓였다)
 * - 처리하지 않는 프레임(깨진 JSON·바이너리)도 속도 제한에 센다(전에는 무제한, 이벤트 루프 수백 ms 지연)
 * - IP당 연결 수는 IPv6 /64 단위로 센다(전에는 주소마다 따로 세어 같은 /64에서 상한을 넘었다)
 * - 하트비트 정리: 재입장 기록·승인 유예·사람 없는 방 캐시
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import { WebSocket, type WebSocketServer } from 'ws'
import { initializeWebSocket, type WsDeps, type WsOptions } from '../src/services/wsService'
import { MAX_BUFFERED_BYTES } from '../src/signaling/protocol'
import { createCapacity } from '../src/signaling/capacity'
import { createPerformers } from '../src/signaling/performers'
import { createRegistry } from '../src/signaling/registry'
import { createRoomMeta } from '../src/signaling/roomMeta'
import { DEFAULT_LIMITS } from '../src/signaling/types'
import { connect, testVerifier, waitFor, type TestClient } from './helpers'

const servers: { server: Server; wss: WebSocketServer }[] = []
const opened: WebSocket[] = []

async function startWs(limits: WsOptions['limits'] = {}, env: Record<string, string> = {}): Promise<string> {
  const server = createServer()
  const deps: Partial<WsDeps> = {
    hasBackend: true,
    verifyToken: testVerifier,
    fetchRoomMeta: async () => ({ hostId: 'host', freeJoin: true }),
    updateParticipants: async () => {},
  }
  const wss = initializeWebSocket(server, null, { env: { NODE_ENV: 'test', ...env }, limits, deps })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

async function client(url: string, headers?: Record<string, string>): Promise<TestClient> {
  const c = await connect(url, headers)
  opened.push(c.ws)
  return c
}

/** 연결 시도: welcome이면 열림, 서버가 먼저 닫으면 닫힘 코드 */
function tryConnect(url: string, headers: Record<string, string>): Promise<number | 'open'> {
  return new Promise(resolve => {
    const ws = new WebSocket(url, { headers })
    opened.push(ws)
    ws.on('message', () => resolve('open'))
    ws.on('close', (code) => resolve(code))
    ws.on('error', () => {})
  })
}

after(() => {
  for (const ws of opened) ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('slow reader', () => {
  it('terminates a connection that stops reading once its send buffer passes the cap', async () => {
    const url = await startWs({ msgBurst: 10_000, msgPerSec: 10_000 })
    const host = await client(url)
    host.send({ type: 'auth', accessToken: 'tok:host' })
    host.send({ type: 'join', roomId: 'r' })
    assert.ok(await host.waitFor(m => m.type === 'room-state'))
    const viewer = await client(url) // 익명 관람자: 끊기면 유예 없이 곧바로 participant-left
    viewer.send({ type: 'auth' })
    viewer.send({ type: 'join', roomId: 'r' })
    assert.ok(await host.waitFor(m => m.type === 'participant-joined'))
    ;(viewer.ws as unknown as { _socket: { pause(): void } })._socket.pause()

    // 방장(연주자) → 관람자 60KB offer. 커널 버퍼가 차면 서버 대기열이 쌓이고, 상한을 넘으면 서버가 끊는다
    const offer = { type: 'offer', to: viewer.id, sdp: 'a'.repeat(60 * 1024) }
    const total = Math.ceil((MAX_BUFFERED_BYTES * 16) / (60 * 1024))
    let left = false
    for (let i = 0; i < total && !left; i++) {
      host.send(offer)
      if (i % 20 === 19) left = await waitFor(() => host.has(m => m.type === 'participant-left' && m.oderId === viewer.id), 50)
    }
    assert.ok(left || await host.waitFor(m => m.type === 'participant-left' && m.oderId === viewer.id, 5000),
      'server dropped the non-reading viewer')
    assert.equal(host.closedCode, null, 'the sender stays connected')
  })
})

describe('invalid frames', () => {
  it('counts unparsable and binary frames against the rate limit and closes a flood', async () => {
    const url = await startWs({ msgBurst: 5, msgPerSec: 1, maxDropped: 10 })
    const c = await client(url)
    for (let i = 0; i < 40; i++) {
      if (i % 3 === 0) c.ws.send(Buffer.from([1, 2, 3]))
      else if (i % 3 === 1) c.send('{not json')
      else c.send(JSON.stringify([1, 2]))
    }
    assert.ok(await waitFor(() => c.closedCode !== null, 3000), 'closed')
    assert.equal(c.closedCode, 1008)
  })

  it('a few stray frames do not close the connection', async () => {
    const url = await startWs({ msgBurst: 5, msgPerSec: 1, maxDropped: 10 })
    const c = await client(url)
    c.send('{not json')
    c.send({ type: 'ping' })
    assert.ok(await c.waitFor(m => m.type === 'pong'))
    assert.equal(c.closedCode, null)
  })
})

describe('connections per IP', () => {
  it('counts IPv6 addresses of the same /64 together', async () => {
    const url = await startWs({ maxConnectionsPerIp: 3 }, { FLY_APP_NAME: 'test' })
    const results: (number | 'open')[] = []
    for (let i = 1; i <= 4; i++) results.push(await tryConnect(url, { 'fly-client-ip': `2001:db8:1:2::${i}` }))
    assert.deepEqual(results, ['open', 'open', 'open', 1008])
    assert.equal(await tryConnect(url, { 'fly-client-ip': '2001:db8:1:3::1' }), 'open', 'another /64 is separate')
    assert.equal(await tryConnect(url, { 'fly-client-ip': '203.0.113.7' }), 'open')
  })
})

describe('heartbeat sweeps', () => {
  it('capacity sweep drops expired rejoin records, not live ones', () => {
    const cap = createCapacity(createRegistry(), DEFAULT_LIMITS)
    const now = Date.now()
    cap.noteLeft('r1', 'gone', now - 1)
    cap.noteLeft('r2', 'live', now + 60_000)
    cap.sweep(now)
    assert.deepEqual(cap.size(), { seatHolds: 0, recentlyLeft: 1 })
    assert.equal(cap.consumeRejoin('r2', 'live'), true)
  })

  it('performers sweep drops expired approval grace, keeps indefinite and live ones', () => {
    const p = createPerformers(createRoomMeta({ hasBackend: false, verifyToken: async () => null, fetchRoomMeta: async () => null, updateParticipants: async () => {} }, false), false)
    const now = Date.now()
    p.setApproval('r', 'expired', now - 1)
    p.setApproval('r', 'forever', null)
    p.setApproval('r', 'grace', now + 60_000)
    p.sweep(now)
    assert.equal(p.hasApproval('r', 'forever'), true)
    assert.equal(p.hasApproval('r', 'grace'), true)
    assert.equal(p.hasApproval('r', 'expired'), false)
  })

  it('room meta sweep forgets old entries of rooms nobody is in', async () => {
    const meta = createRoomMeta({ hasBackend: true, verifyToken: async () => null, fetchRoomMeta: async () => null, updateParticipants: async () => {} }, false)
    await meta.getRoomMeta('empty', 0)
    await meta.getRoomMeta('busy', 0)
    meta.sweep(Date.now(), () => true)
    assert.equal(meta.cacheSize(), 2, 'fresh entries stay')
    meta.sweep(Date.now() + 60_000, id => id === 'busy')
    assert.equal(meta.cacheSize(), 1, 'only the active room stays')
  })
})
