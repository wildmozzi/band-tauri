/**
 * P2-18c: 익명 관람자 세션 재개.
 * 익명 auth-result에 1회용 resumeToken을 싣고, 끊긴 익명 연결을 짧게 유예한다. 새 연결이 resumeClientId·resumeToken·resumeRoomId를
 * 보내고 셋이 맞을 때만 같은 clientId로 붙인다(participant-left/joined 없음 → 방송 연주자는 재offer하지 않는다).
 * 틀린 토큰·쓴 토큰·다른 방·만료·JWT가 있는 연결은 재개하지 않는다. 익명 유예 수 상한(전체·IP별)을 넘으면 바로 퇴장한다.
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { authJoin, connect, sleep, testVerifier, type TestClient } from './helpers'

const ROOMS: Record<string, RoomMeta> = {
  free: { hostId: 'host', freeJoin: true },
  other: { hostId: 'host', freeJoin: true },
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits: { heartbeatMs: 60_000, ...limits },
    deps: {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async (roomId) => ROOMS[roomId] ?? null,
      updateParticipants: async () => {},
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

/** 받은 마지막 익명 재개 토큰 */
function lastToken(c: TestClient): string {
  const t = [...c.inbox].reverse().find(m => m.type === 'auth-result' && typeof m.resumeToken === 'string')?.resumeToken
  assert.ok(t, 'anonymous auth-result carries a resumeToken')
  return t
}

/** 익명 관람자로 입장하고 재개 토큰을 돌려준다 */
async function anonJoin(url: string, roomId = 'free') {
  const c = await client(url)
  await authJoin(c, null, roomId)
  return { c, token: lastToken(c) }
}

/** 새 연결에서 익명 재개 요청 */
async function resumeAnon(url: string, req: { resumeClientId: string; resumeToken: string; resumeRoomId: string; accessToken?: string }) {
  const c = await client(url)
  c.send({ type: 'auth', ...req })
  await c.waitFor(m => m.type === 'auth-result')
  return c
}

const leftOrJoined = (id: string) => (m: any) =>
  (m.type === 'participant-left' && m.oderId === id) || (m.type === 'participant-joined' && m.participant?.oderId === id)

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('P2-18c: anonymous viewer resume', async () => {
  // 테스트 연결은 모두 127.0.0.1: 방·IP별 익명 상한과 IP별 익명 유예 상한을 넉넉히
  const url = await startWs({ anonResumeGraceMs: 1000, maxAnonymousPerRoom: 50, maxAnonymousPerIpPerRoom: 50, maxSuspendedAnonymousPerIp: 50 })

  it('resumes with the same clientId, rotates the token, no left/joined to peers, stays a viewer', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:anon-peer1', 'free')
    const { c: anon, token } = await anonJoin(url)
    assert.ok(/^[A-Za-z0-9_-]{43}$/.test(token), '256-bit base64url token')
    const oldId = anon.id
    const seenBefore = peer.inbox.filter(leftOrJoined(oldId)).length
    anon.ws.terminate()
    await sleep(100)

    const a2 = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(a2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    const ar = a2.inbox.find(m => m.type === 'auth-result')
    assert.equal(ar.authenticated, false)
    assert.equal(ar.resumed, true)
    assert.equal(ar.userId, null)
    assert.ok(typeof ar.resumeToken === 'string' && ar.resumeToken !== token, 'token rotated')
    assert.ok(await a2.waitFor(m => m.type === 'room-state'))
    await sleep(200)
    assert.equal(peer.inbox.filter(leftOrJoined(oldId)).length, seenBefore, 'peers never saw the anonymous viewer leave or rejoin')

    // 재개해도 익명 관람자: 연주 불가
    a2.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await a2.waitFor(m => m.type === 'error'))
    await sleep(100)
    assert.ok(!peer.has(m => m.type === 'performer-updated' && m.oderId === oldId))
  })

  it('rejects a wrong token without burning the real one', async () => {
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    anon.ws.terminate()
    await sleep(50)

    const bad = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token.slice(0, -2) + 'AA', resumeRoomId: 'free' })
    assert.ok(!bad.has(m => m.resumed === true))
    assert.ok(bad.has(m => m.type === 'auth-result' && m.authenticated === false))

    const good = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(good.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
  })

  it('a used token cannot be replayed (rotation)', async () => {
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    anon.ws.terminate()
    await sleep(50)
    const a2 = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(a2.has(m => m.resumed === true))
    const next = lastToken(a2)
    a2.ws.terminate()
    await sleep(50)

    const replay = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(!replay.has(m => m.resumed === true))
    const a3 = await resumeAnon(url, { resumeClientId: oldId, resumeToken: next, resumeRoomId: 'free' })
    assert.ok(a3.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
  })

  it('does not resume into a different room', async () => {
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    anon.ws.terminate()
    await sleep(50)
    const other = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'other' })
    assert.ok(!other.has(m => m.resumed === true))
  })

  it('a connection with a valid JWT does not take the anonymous path', async () => {
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    anon.ws.terminate()
    await sleep(50)
    const logged = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free', accessToken: 'tok:anon-mallory' })
    assert.ok(!logged.has(m => m.resumed === true))
    assert.ok(logged.has(m => m.type === 'auth-result' && m.userId === 'anon-mallory'))
  })

  it('takes over a half-open anonymous connection (old socket closed, same clientId)', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:anon-peer2', 'free')
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    const seenBefore = peer.inbox.filter(leftOrJoined(oldId)).length
    const a2 = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(a2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    assert.ok(await (async () => { for (let i = 0; i < 100 && anon.closedCode === null; i++) await sleep(10); return anon.closedCode !== null })())
    await sleep(200)
    assert.equal(peer.inbox.filter(leftOrJoined(oldId)).length, seenBefore)
  })
})

describe('P2-18c: anonymous grace expiry', async () => {
  const url = await startWs({ anonResumeGraceMs: 150 })

  it('leaves after the grace period and the token no longer resumes', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:anon-peer3', 'free')
    const { c: anon, token } = await anonJoin(url)
    const oldId = anon.id
    anon.ws.terminate()
    await sleep(50)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId), 'suspended, not left yet')
    assert.ok(await peer.waitFor(m => m.type === 'participant-left' && m.oderId === oldId, 1000))
    const late = await resumeAnon(url, { resumeClientId: oldId, resumeToken: token, resumeRoomId: 'free' })
    assert.ok(!late.has(m => m.resumed === true))
  })
})

describe('P2-18c: anonymous suspend limits', async () => {
  const url = await startWs({ anonResumeGraceMs: 5000, maxSuspendedAnonymousPerIp: 1 })

  it('over the per-IP anonymous suspend limit, the viewer leaves at once', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:anon-peer4', 'free')
    const { c: a } = await anonJoin(url)
    const { c: b } = await anonJoin(url)
    a.ws.terminate()
    await sleep(100)
    b.ws.terminate()
    assert.ok(await peer.waitFor(m => m.type === 'participant-left' && m.oderId === b.id, 500), 'second anonymous suspend refused')
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === a.id), 'first is still suspended')
  })
})
