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
import { MAX_BUFFERED_BYTES, MAX_PENDING_RELAY_MESSAGES, MAX_RELAY_MESSAGE_BYTES } from '../src/signaling/protocol'
import { createCapacity } from '../src/signaling/capacity'
import { createPerformers } from '../src/signaling/performers'
import { createRegistry } from '../src/signaling/registry'
import { createRoomMeta } from '../src/signaling/roomMeta'
import { DEFAULT_LIMITS } from '../src/signaling/types'
import { ipBucket } from '../src/lib/roomPassword'
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
  // 익명 관람자가 끊기면 곧바로 participant-left가 되게(익명 재개 유예 P2-18c는 끔) — 끊김 판정을 퇴장으로 본다
  const wss = initializeWebSocket(server, null, { env: { NODE_ENV: 'test', ...env }, limits: { anonResumeGraceMs: 0, ...limits }, deps })
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

/** 방장(연주자) 1명 + 익명 관람자 n명이 같은 방에 들어온 상태 */
async function hostWithViewers(url: string, n: number, roomId = 'r'): Promise<{ host: TestClient; viewers: TestClient[] }> {
  const host = await client(url)
  host.send({ type: 'auth', accessToken: 'tok:host' })
  host.send({ type: 'join', roomId })
  assert.ok(await host.waitFor(m => m.type === 'room-state'))
  const viewers: TestClient[] = []
  for (let i = 0; i < n; i++) {
    const v = await client(url) // 익명 관람자: 끊기면 유예 없이 곧바로 participant-left (startWs가 익명 재개 유예를 끈다)
    v.send({ type: 'auth' })
    v.send({ type: 'join', roomId })
    assert.ok(await host.waitFor(m => m.type === 'participant-joined' && m.participant.oderId === v.id))
    viewers.push(v)
  }
  return { host, viewers }
}

const pause = (c: TestClient) => (c.ws as unknown as { _socket: { pause(): void } })._socket.pause()
const resume = (c: TestClient) => (c.ws as unknown as { _socket: { resume(): void } })._socket.resume()
const offerOf = (to: string, sdpBytes: number) => ({ type: 'offer', to, offer: { type: 'offer', sdp: 'a'.repeat(sdpBytes) } })
const RELAXED_RELAY = { relayMsgBurst: 1e6, relayMsgPerSec: 1e6, relayPerTargetBurst: 1e6, relayPerTargetPerSec: 1e6, relayInBytesBurst: 1e9, relayInBytesPerSec: 1e9, relayPairBytesBurst: 1e9, relayPairBytesPerSec: 1e9 }

describe('slow reader', () => {
  it('terminates a connection that stops reading once its send buffer passes the cap', async () => {
    const url = await startWs({ msgBurst: 10_000, msgPerSec: 10_000, ...RELAXED_RELAY })
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    pause(viewer)

    // 방장 → 관람자 15KB offer. 커널 버퍼가 차면 서버 대기열이 쌓이고, 상한을 넘으면 서버가 끊는다
    const offer = offerOf(viewer.id, 15 * 1024)
    const total = Math.ceil((MAX_BUFFERED_BYTES * 16) / (15 * 1024))
    let left = false
    for (let i = 0; i < total && !left; i++) {
      host.send(offer)
      if (i % 50 === 49) left = await waitFor(() => host.has(m => m.type === 'participant-left' && m.oderId === viewer.id), 50)
    }
    assert.ok(left || await host.waitFor(m => m.type === 'participant-left' && m.oderId === viewer.id, 5000),
      'server dropped the non-reading viewer')
    assert.equal(host.closedCode, null, 'the sender stays connected')
  })

  it('terminates the heaviest connections when the total send buffer passes the server cap', async () => {
    // 연결당 상한(1MB)보다 작은 전체 상한: 1초 감시가 대기열이 가장 큰 연결을 끊는다
    const url = await startWs({ msgBurst: 10_000, msgPerSec: 10_000, maxTotalBufferedBytes: 256 * 1024, ...RELAXED_RELAY })
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    pause(viewer)
    // 서버 쪽 대기열이 전체 상한(256KB)은 넘고 연결당 상한(1MB)에는 못 미치는 동안만 보낸다 → 전체 상한 감시만 끊을 수 있다
    const wss = servers[servers.length - 1].wss
    const maxBuffered = () => Math.max(0, ...[...wss.clients].map(ws => ws.bufferedAmount))
    for (let i = 0; i < 2000 && maxBuffered() < 300 * 1024; i++) {
      host.send(offerOf(viewer.id, 15 * 1024))
      if (i % 5 === 4) await new Promise(r => setTimeout(r, 5))
    }
    assert.ok(maxBuffered() < MAX_BUFFERED_BYTES, `stopped below the per-connection cap (${maxBuffered()})`)
    assert.ok(await host.waitFor(m => m.type === 'participant-left' && m.oderId === viewer.id, 3000))
    assert.equal(host.closedCode, null)
  })
})

describe('viewer relay abuse (#184 review M: targeted kick of a performer)', () => {
  it('anonymous viewers flooding offers cannot fill a performer send buffer', async () => {
    // 기본 속도 제한 그대로. 연주자가 잠시 읽지 못하는(느린 회선) 동안 관람자 둘이 큰 offer를 최대 속도로 보낸다
    const url = await startWs()
    const { host, viewers } = await hostWithViewers(url, 2)
    host.send({ type: 'start-performing', instrument: 'drums' })
    await new Promise(r => setTimeout(r, 100))
    pause(host)
    const end = Date.now() + 3000
    while (Date.now() < end) {
      for (const v of viewers) {
        v.send(offerOf(host.id, 60 * 1024)) // 크기 상한(16KB) 초과 — 중계 안 됨
        v.send(offerOf(host.id, 16 * 1024 - 16)) // 상한 안 — 대상 바이트 버킷이 막는다
      }
      await new Promise(r => setTimeout(r, 50))
    }
    resume(host)
    host.send({ type: 'ping' })
    assert.ok(await host.waitFor(m => m.type === 'pong', 5000), 'performer connection survived')
    assert.equal(host.closedCode, null)
    const relayedBytes = host.inbox.filter(m => m.type === 'offer').reduce((a, m) => a + m.offer.sdp.length, 0)
    assert.ok(relayedBytes <= DEFAULT_LIMITS.relayInBytesBurst + DEFAULT_LIMITS.relayInBytesPerSec * 4,
      `relayed ${relayedBytes} bytes`)
    assert.ok(host.inbox.every(m => m.type !== 'offer' || m.offer.sdp.length < 16 * 1024), 'oversized offers are dropped')
  })

  it('limits messages per sender → target but lets a normal ICE trickle through', async () => {
    const url = await startWs({ msgBurst: 10_000, msgPerSec: 10_000, relayMsgBurst: 10_000, relayMsgPerSec: 10_000 })
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    for (let i = 0; i < 60; i++) host.send({ type: 'ice-candidate', to: viewer.id, candidate: { candidate: `candidate:${i}`, sdpMid: '0' } })
    host.send({ type: 'ping' })
    assert.ok(await host.waitFor(m => m.type === 'pong'))
    await waitFor(() => viewer.inbox.filter(m => m.type === 'ice-candidate').length >= DEFAULT_LIMITS.relayPerTargetBurst)
    await new Promise(r => setTimeout(r, 100))
    const got = viewer.inbox.filter(m => m.type === 'ice-candidate').length
    assert.ok(got >= 30, `a normal trickle (30) passes: got ${got}`)
    assert.ok(got <= DEFAULT_LIMITS.relayPerTargetBurst + 2, `burst is capped: got ${got}`)
  })
})

describe('viewer relay burst (server load discovery: many viewers join at once)', () => {
  /** 방장 1명 + 로그인 관람자 n명(익명 상한과 무관하게) */
  async function hostWithLoggedInViewers(url: string, n: number, roomId: string) {
    const host = await client(url)
    host.send({ type: 'auth', accessToken: 'tok:host' })
    host.send({ type: 'join', roomId })
    assert.ok(await host.waitFor(m => m.type === 'room-state'))
    const viewers: TestClient[] = []
    for (let i = 0; i < n; i++) {
      const v = await client(url)
      v.send({ type: 'auth', accessToken: `tok:burst-viewer-${i}` })
      v.send({ type: 'join', roomId })
      assert.ok(await host.waitFor(m => m.type === 'participant-joined' && m.participant.oderId === v.id))
      viewers.push(v)
    }
    return { host, viewers }
  }

  it('lets the broadcaster send an offer and 12 candidates to 14 viewers at once with the default limits', async () => {
    // 일반 버킷(40)이었을 때는 14 × 13 = 182개 중 약 68%가 버려졌다(docs/overhaul/server-load-2026-11-18.md)
    const url = await startWs()
    const { host, viewers } = await hostWithLoggedInViewers(url, 14, 'burst')
    for (const v of viewers) {
      host.send(offerOf(v.id, 2400))
      for (let i = 0; i < 12; i++) host.send({ type: 'ice-candidate', to: v.id, candidate: { candidate: `candidate:${i}`, sdpMid: '0' } })
    }
    host.send({ type: 'ping' })
    assert.ok(await host.waitFor(m => m.type === 'pong'))
    for (const v of viewers) {
      assert.ok(await waitFor(() => v.inbox.filter(m => m.type === 'ice-candidate').length === 12), `viewer got all 12 candidates (${v.inbox.filter(m => m.type === 'ice-candidate').length})`)
      assert.ok(v.has(m => m.type === 'offer'))
    }
    assert.ok(!host.has(m => m.type === 'error' && m.code === 'rate-limited'), 'no rate-limited notice for the broadcaster')
  })

  it('a viewer keeps the base relay budget (same as the general bucket) and is told when it overflows', async () => {
    const url = await startWs()
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1, 'base')
    for (let i = 0; i < DEFAULT_LIMITS.relayMsgBurst + 20; i++) viewer.send({ type: 'ice-candidate', to: host.id, candidate: { candidate: `candidate:${i}`, sdpMid: '0' } })
    assert.ok(await viewer.waitFor(m => m.type === 'error' && m.code === 'rate-limited' && m.rejectedType === 'ice-candidate'))
    await new Promise(r => setTimeout(r, 100))
    assert.ok(host.inbox.filter(m => m.type === 'ice-candidate').length <= DEFAULT_LIMITS.relayMsgBurst + 2)
  })
})


describe('relay messages queued behind a stalled chain (#215 review H1)', () => {
  /** 'slow' 방의 방 정보 조회가 release()까지 멈춘다 — 그동안 이 연결의 체인(순차 처리)이 막힌다 */
  async function startStalling(limits: WsOptions['limits']) {
    let release!: () => void
    const stalled = new Promise<void>(r => { release = r })
    const server = createServer()
    const deps: Partial<WsDeps> = {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async (roomId) => { if (roomId === 'slow') await stalled; return { hostId: 'host', freeJoin: true } },
      updateParticipants: async () => {},
    }
    const wss = initializeWebSocket(server, null, { env: { NODE_ENV: 'test' }, limits, deps })
    await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
    servers.push({ server, wss })
    return { url: `ws://127.0.0.1:${(server.address() as { port: number }).port}`, release }
  }
  const relaxed = { relayMsgBurst: 1e6, relayMsgPerSec: 1e6, maxDropped: 1e6 }
  const ice = (to: string, pad = 0) => ({ type: 'ice-candidate', to, candidate: { candidate: 'candidate:1', sdpMid: '0' }, pad: 'x'.repeat(pad) })

  it('drops relay messages over the raw size cap instead of queueing them', async () => {
    const { url, release } = await startStalling(relaxed)
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    host.send(ice(viewer.id, MAX_RELAY_MESSAGE_BYTES)) // 원문이 상한을 넘는다(본문은 정상)
    assert.ok(await host.waitFor(m => m.type === 'error' && m.code === 'rate-limited' && m.rejectedType === 'ice-candidate'))
    host.send(ice(viewer.id))
    assert.ok(await viewer.waitFor(m => m.type === 'ice-candidate'))
    assert.equal(viewer.inbox.filter(m => m.type === 'ice-candidate').length, 1, 'only the small one was relayed')
    release()
  })

  it('caps the relay messages and bytes a connection can park while its chain is stalled (reviewer probe)', async () => {
    const { url, release } = await startStalling(relaxed)
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    host.send({ type: 'join', roomId: 'slow' }) // 체인이 여기서 멈춘다
    // 버킷은 사실상 무제한 — 대기 상한만이 막을 수 있다. 개수 상한 넘게, 바이트 상한 넘게 보낸다
    for (let i = 0; i < MAX_PENDING_RELAY_MESSAGES + 50; i++) host.send(ice(viewer.id))
    assert.ok(await host.waitFor(m => m.type === 'error' && m.code === 'rate-limited' && m.rejectedType === 'ice-candidate'), 'count cap while stalled')
    release()
    // 바이트 상한: 15KB × 100 = 1.5MB > 1MB
    const s2 = await startStalling(relaxed)
    const p2 = await hostWithViewers(s2.url, 1)
    p2.host.send({ type: 'join', roomId: 'slow' })
    for (let i = 0; i < 100; i++) p2.host.send(ice(p2.viewers[0].id, 15 * 1024))
    assert.ok(await p2.host.waitFor(m => m.type === 'error' && m.code === 'rate-limited'), 'byte cap while stalled')
    assert.equal(p2.host.closedCode, null, 'dropping does not close the connection by itself')
    s2.release()
  })
})

describe('viewer relay fairness (#187 review M: targeted broadcast denial)', () => {
  const answerOf = (to: string, sdpBytes: number) => ({ type: 'answer', to, answer: { type: 'answer', sdp: 'a'.repeat(sdpBytes) } })

  it('does not relay an offer from a non-performer to a performer, but relays answers and candidates', async () => {
    const url = await startWs()
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    viewer.send(offerOf(host.id, 1000))
    viewer.send(answerOf(host.id, 1000))
    viewer.send({ type: 'ice-candidate', to: host.id, candidate: { candidate: 'candidate:1', sdpMid: '0' } })
    assert.ok(await host.waitFor(m => m.type === 'ice-candidate'))
    assert.deepEqual(host.inbox.filter(m => ['offer', 'answer', 'ice-candidate'].includes(m.type)).map(m => m.type), ['answer', 'ice-candidate'])
  })

  it('one viewer flooding answers cannot use up the budget other viewers need to reach the performer', async () => {
    const url = await startWs({ msgBurst: 1000, msgPerSec: 1000, relayMsgBurst: 1000, relayMsgPerSec: 1000 })
    const { host, viewers: [attacker, legit] } = await hostWithViewers(url, 2)
    for (let i = 0; i < 40; i++) attacker.send(answerOf(host.id, 15 * 1024))
    attacker.send({ type: 'ping' })
    assert.ok(await attacker.waitFor(m => m.type === 'pong'))
    legit.send(answerOf(host.id, 3000))
    for (let i = 0; i < 5; i++) legit.send({ type: 'ice-candidate', to: host.id, candidate: { candidate: `candidate:${i}`, sdpMid: '0' } })
    assert.ok(await host.waitFor(m => m.type === 'ice-candidate' && m.from === legit.id && m.candidate.candidate === 'candidate:4'))
    assert.ok(host.has(m => m.type === 'answer' && m.from === legit.id), 'legit answer arrived')
    const fromAttacker = host.inbox.filter(m => m.type === 'answer' && m.from === attacker.id).length
    assert.ok(fromAttacker <= Math.ceil(DEFAULT_LIMITS.relayPairBytesBurst / (15 * 1024)) + 1, `attacker relayed ${fromAttacker} answers`)
  })

  it('measures relay size in UTF-8 bytes', async () => {
    const url = await startWs()
    const { host, viewers: [viewer] } = await hostWithViewers(url, 1)
    // 6000자 × 3바이트 = 18KB > 16KB (UTF-16 길이로는 6000이라 전에는 통과)
    host.send({ type: 'offer', to: viewer.id, offer: { type: 'offer', sdp: '가'.repeat(6000) } })
    host.send({ type: 'offer', to: viewer.id, offer: { type: 'offer', sdp: '가'.repeat(5000) } })
    assert.ok(await viewer.waitFor(m => m.type === 'offer'))
    await new Promise(r => setTimeout(r, 100))
    assert.deepEqual(viewer.inbox.filter(m => m.type === 'offer').map(m => m.offer.sdp.length), [5000])
  })
})

describe('server-wide connection cap', () => {
  it('rejects connections over maxConnections with 1013', async () => {
    const url = await startWs({ maxConnections: 2 })
    assert.equal(await tryConnect(url, {}), 'open')
    assert.equal(await tryConnect(url, {}), 'open')
    assert.equal(await tryConnect(url, {}), 1013)
  })

  it('terminates a rejected connection even if the peer ignores the close frame', async () => {
    const url = await startWs({ maxConnections: 1 })
    assert.equal(await tryConnect(url, {}), 'open')
    // 응답하지 않는 상대: 원시 TCP로 업그레이드만 하고 아무것도 읽지 않는다 → 서버가 1초 뒤 소켓을 끊어야 한다
    const net = await import('node:net')
    const { port } = new URL(url)
    const sock = net.connect(Number(port), '127.0.0.1')
    let ended = false
    sock.on('close', () => { ended = true })
    sock.on('error', () => {})
    sock.resume() // 읽기는 하되 close 프레임에 답하지 않는다
    sock.write(['GET / HTTP/1.1', 'Host: x', 'Upgrade: websocket', 'Connection: Upgrade', 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==', 'Sec-WebSocket-Version: 13', '', ''].join(String.fromCharCode(13, 10)))
    assert.ok(await waitFor(() => ended, 3000), 'server terminated the rejected socket')
    sock.destroy()
  })
})

describe('ipBucket', () => {
  it('maps hex IPv4-mapped IPv6 to the IPv4 address instead of a shared /64', () => {
    assert.equal(ipBucket('::ffff:7f00:1'), '127.0.0.1')
    assert.equal(ipBucket('0:0:0:0:0:ffff:c000:201'), '192.0.2.1')
    assert.equal(ipBucket('::FFFF:C000:0201'), '192.0.2.1')
    assert.equal(ipBucket('::ffff:192.0.2.1'), '192.0.2.1')
    assert.equal(ipBucket('2001:db8:1:2::5'), '2001:db8:1:2::/64')
    assert.equal(ipBucket('::1'), '0:0:0:0::/64')
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
