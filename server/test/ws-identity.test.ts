/**
 * 신원 불신 특성 테스트 — docs/server-architecture.md 2.5절 "빈 곳"(S0).
 * 클라이언트가 거짓 신원·권한을 실어 보냈을 때 서버가 무시하는지 본다. 부정 확인("오지 않았다")은 sleep 대신
 * 표식 메시지(wsSettle.ts)로 앞선 처리가 끝났음을 확인한 뒤 inbox를 본다.
 *
 * - auth: 다른 사용자 토큰으로 재인증해도 userId 불변, 방 안 익명은 승격되지 않음(방 밖에서는 승격), 만료 후 4001
 * - capacity: isHost:true를 보낸 비방장은 꽉 찬 방에 못 들어감
 * - requests: 비방장의 approve/reject-perform, approve/reject-instrument-change 무시, 악기 변경 요청 흐름 6종
 * - viewerRelay: 클라이언트가 보낸 from은 덮어씀, 다른 방·방 밖 대상(to)은 차단
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { connect, testVerifier, waitFor, type TestClient } from './helpers'
import { PROBE_ROOM, settle, settleAll } from './wsSettle'

/** 방장은 모두 'host'. 테스트마다 다른 방 이름을 써서 서로 섞이지 않게 한다(같은 사용자의 중복 연결 정리 등) */
function roomMeta(roomId: string): RoomMeta | null {
  if (roomId.startsWith('locked')) return { hostId: 'host', freeJoin: false }
  if (roomId.startsWith('free')) return { hostId: 'host', freeJoin: true }
  if (roomId.startsWith('full')) return { hostId: 'host', freeJoin: true, maxParticipants: 2 }
  return null
}
let roomSeq = 0
const room = (kind: 'locked' | 'free' | 'full') => `${kind}-${++roomSeq}`

/** JWT 모양 토큰(서명 없음): 서버는 verifyToken 통과 뒤 payload의 exp만 읽는다(jwtExpiryMs) */
const jwt = (sub: string, expSec: number) =>
  `h.${Buffer.from(JSON.stringify({ sub, exp: expSec })).toString('base64url')}.s`

/** 'tok:<userId>'(helpers) 또는 jwt()의 sub */
async function verifyToken(token: string): Promise<string | null> {
  const plain = await testVerifier(token)
  if (plain) return plain
  const parts = token.split('.')
  if (parts.length !== 3) return null
  try {
    const sub = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8')).sub
    return typeof sub === 'string' ? sub : null
  } catch {
    return null
  }
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    // 표식 메시지가 일반·join 버킷을 쓴다. 한 서버에 연결이 많아 IP 상한도 넉넉히
    limits: { msgBurst: 1000, msgPerSec: 1000, joinBurst: 1000, joinPerSec: 1000, maxConnectionsPerIp: 1000, maxAnonymousPerIpPerRoom: 100, ...limits },
    deps: {
      hasBackend: true,
      verifyToken,
      fetchRoomMeta: async (roomId) => (roomId === PROBE_ROOM ? null : roomMeta(roomId)),
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

/** auth → (join) 후 결과가 도착할 때까지(표식) */
async function enter(url: string, token: string | null, roomId?: string, extra: Record<string, unknown> = {}) {
  const c = await client(url)
  c.send(token ? { type: 'auth', accessToken: token } : { type: 'auth' })
  if (roomId) c.send({ type: 'join', roomId, ...extra })
  await settle(c)
  return c
}

const types = (c: TestClient) => c.inbox.map(m => m.type)
const clear = (...cs: TestClient[]) => { for (const c of cs) c.inbox.length = 0 }
const last = (c: TestClient, type: string) => [...c.inbox].reverse().find(m => m.type === type)

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('auth: identity is fixed by the first verified token', async () => {
  const url = await startWs()

  it('a re-auth with another user\'s token does not change userId (in and out of a room)', async () => {
    const alice = await enter(url, 'tok:alice')
    assert.deepEqual(last(alice, 'auth-result'), { type: 'auth-result', authenticated: true, userId: 'alice' })

    // 방 밖: 방장 토큰으로 재인증해도 alice 그대로
    clear(alice)
    alice.send({ type: 'auth', accessToken: 'tok:host' })
    await settle(alice)
    assert.deepEqual(alice.inbox, [{ type: 'auth-result', authenticated: true, userId: 'alice' }])

    // 그 뒤 입장하면 방장으로 판정되지 않는다
    alice.send({ type: 'join', roomId: room('locked') })
    await settle(alice)
    assert.equal(last(alice, 'room-state').isHost, false)

    // 방 안: 다시 방장 토큰 → userId 불변, 방장 권한 없음
    clear(alice)
    alice.send({ type: 'auth', accessToken: 'tok:host' })
    alice.send({ type: 'start-performing', instrument: 'drums' })
    await settle(alice)
    // 위 deepEqual이 alice.inbox 타입을 좁혀 두었으므로 any로 다시 읽는다
    const [authResult, refusal]: any[] = alice.inbox
    assert.deepEqual(authResult, { type: 'auth-result', authenticated: true, userId: 'alice' })
    assert.equal(refusal.code, 'perform-not-approved')
  })

  it('an anonymous viewer inside a room is not upgraded by re-auth', async () => {
    const r = room('free')
    const watcher = await enter(url, 'tok:watcher', r)
    const anon = await enter(url, null, r)
    assert.deepEqual(last(anon, 'auth-result'), { type: 'auth-result', authenticated: false, userId: null })

    clear(anon, watcher)
    anon.send({ type: 'auth', accessToken: 'tok:carol' })
    anon.send({ type: 'start-performing', instrument: 'bass' })
    anon.send({ type: 'request-perform', instrument: 'bass' })
    await settleAll(anon, [watcher])
    assert.deepEqual(anon.inbox[0], { type: 'auth-result', authenticated: false, userId: null })
    assert.deepEqual(anon.inbox.slice(1).map(m => m.code), ['login-required', 'login-required', 'login-required'])
    assert.deepEqual(watcher.inbox, [], 'no performer-updated / request for the host')
  })

  it('an anonymous connection outside a room is upgraded by re-auth and can then perform', async () => {
    const r = room('free')
    const watcher = await enter(url, 'tok:watcher2', r)
    const anon = await enter(url, null, r)
    anon.send({ type: 'leave' })
    clear(anon)
    anon.send({ type: 'auth', accessToken: 'tok:dave' })
    await settle(anon)
    assert.deepEqual(anon.inbox, [{ type: 'auth-result', authenticated: true, userId: 'dave' }])

    // 승격된 뒤에는 다른 사용자의 토큰으로 바뀌지 않는다
    clear(anon)
    anon.send({ type: 'auth', accessToken: 'tok:erin' })
    await settle(anon)
    assert.deepEqual(anon.inbox, [{ type: 'auth-result', authenticated: true, userId: 'dave' }])

    anon.send({ type: 'join', roomId: r })
    anon.send({ type: 'start-performing', instrument: 'bass' })
    await settleAll(anon, [watcher])
    assert.ok(watcher.has(m => m.type === 'performer-updated' && m.oderId === anon.id && m.isPerforming === true))
  })

  it('closes with 4001 once the token has expired (exp + grace in the past)', async () => {
    const now = Math.floor(Date.now() / 1000)
    const expired = await client(url)
    expired.send({ type: 'auth', accessToken: jwt('frank', now - 120) })
    assert.ok(await waitFor(() => expired.closedCode !== null, 3000))
    assert.equal(expired.closedCode, 4001)

    // 유효한 토큰은 열려 있다
    const fresh = await enter(url, jwt('gina', now + 3600))
    assert.deepEqual(last(fresh, 'auth-result'), { type: 'auth-result', authenticated: true, userId: 'gina' })
    assert.equal(fresh.closedCode, null)
  })

  it('a same-user re-auth replaces the expiry; another user\'s expired token does not', async () => {
    const now = Math.floor(Date.now() / 1000)
    const c = await enter(url, jwt('hana', now + 3600), room('free'))

    // 다른 사용자의 만료 토큰: 무시(만료 시각도 바뀌지 않음)
    c.send({ type: 'auth', accessToken: jwt('ivan', now - 120) })
    await settle(c)
    assert.equal(c.closedCode, null)
    assert.equal(last(c, 'auth-result').userId, 'hana')

    // 같은 사용자의 (이미 만료된) 토큰: 만료 시각을 그 토큰으로 바꾼다 → 4001
    c.send({ type: 'auth', accessToken: jwt('hana', now - 120) })
    assert.ok(await waitFor(() => c.closedCode !== null, 3000))
    assert.equal(c.closedCode, 4001)
  })
})

describe('capacity: client-sent isHost does not bypass a full room', async () => {
  const url = await startWs()

  it('rejects a non-host that claims isHost/userId of the host when the room is full', async () => {
    const r = room('full')
    await enter(url, 'tok:host', r)
    await enter(url, 'tok:m1', r)

    const liar = await enter(url, 'tok:m2', r, { isHost: true, userId: 'host' })
    const err = last(liar, 'error')
    assert.equal(err.code, 'room-full')
    assert.equal(err.reason, 'room-full')
    assert.ok(!liar.has(m => m.type === 'room-state'))

    const anonLiar = await enter(url, null, r, { isHost: true, userId: 'host' })
    assert.equal(last(anonLiar, 'error').code, 'room-full')
    assert.ok(!anonLiar.has(m => m.type === 'room-state'))
  })
})

describe('requests: only the DB host approves or rejects', async () => {
  const url = await startWs()

  it('ignores approve-perform / reject-perform from a non-host member', async () => {
    const r = room('locked')
    const host = await enter(url, 'tok:host', r)
    const req = await enter(url, 'tok:req', r)
    const by = await enter(url, 'tok:bystander', r)
    const all = [host, req, by]

    req.send({ type: 'request-perform', instrument: 'guitar' })
    await settleAll(req, all)
    assert.ok(host.has(m => m.type === 'perform-request-received' && m.request.oderId === req.id))

    clear(...all)
    by.send({ type: 'approve-perform', targetId: req.id, isHost: true })
    by.send({ type: 'reject-perform', targetId: req.id, reason: 'nope' })
    await settleAll(by, all)
    assert.deepEqual(all.map(types), [[], [], []])

    req.send({ type: 'start-performing', instrument: 'guitar' })
    await settleAll(req, all)
    assert.equal(last(req, 'error').code, 'perform-not-approved')

    // 요청은 그대로 남아 방장이 승인할 수 있다
    clear(...all)
    host.send({ type: 'approve-perform', targetId: req.id })
    await settleAll(host, all)
    assert.deepEqual(req.inbox, [{ type: 'perform-request-approved', instrument: 'guitar' }])
  })

  it('ignores approve-instrument-change / reject-instrument-change from a non-host member', async () => {
    const r = room('free')
    const host = await enter(url, 'tok:host', r)
    const perf = await enter(url, 'tok:perf', r)
    const by = await enter(url, 'tok:bystander2', r)
    const all = [host, perf, by]
    perf.send({ type: 'start-performing', instrument: 'guitar' })
    by.send({ type: 'start-performing', instrument: 'keys' }) // 연주자여도 방장이 아니면 권한 없음
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    await settleAll(by, all)
    await settleAll(perf, all)
    assert.ok(host.has(m => m.type === 'instrument-change-request-received' && m.request.oderId === perf.id))

    clear(...all)
    by.send({ type: 'approve-instrument-change', targetId: perf.id })
    by.send({ type: 'reject-instrument-change', targetId: perf.id, reason: 'nope' })
    await settleAll(by, all)
    assert.deepEqual(all.map(types), [[], [], []])

    host.send({ type: 'approve-instrument-change', targetId: perf.id })
    await settleAll(host, all)
    assert.deepEqual(perf.inbox, [{ type: 'instrument-change-approved', newInstrument: 'bass' }])
    assert.deepEqual(by.inbox, [{ type: 'participant-instrument-changed', oderId: perf.id, instrument: 'bass' }])
  })
})

describe('instrument change request flows', async () => {
  const url = await startWs()

  /** 방장 + 연주자 + 관람자, 연주자는 free 방이라 바로 연주 */
  async function band(roomId = room('free')) {
    const host = await enter(url, 'tok:host', roomId)
    const perf = await enter(url, `tok:perf-${Math.random().toString(36).slice(2, 8)}`, roomId)
    const viewer = await enter(url, null, roomId)
    perf.send({ type: 'start-performing', instrument: 'guitar' })
    await settleAll(perf, [host, viewer])
    clear(host, perf, viewer)
    return { roomId, host, perf, viewer, all: [host, perf, viewer] }
  }

  it('1. request: host receives it with the current instrument, the requester gets a receipt, nobody else', async () => {
    const { host, perf, viewer, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass', currentInstrument: 'spoof', oderId: 'spoof' })
    await settleAll(perf, all)
    assert.equal(host.inbox.length, 1)
    const { request } = host.inbox[0]
    assert.equal(host.inbox[0].type, 'instrument-change-request-received')
    assert.equal(request.oderId, perf.id)
    assert.equal(request.currentInstrument, 'guitar')
    assert.equal(request.newInstrument, 'bass')
    assert.equal(typeof request.timestamp, 'number')
    assert.deepEqual(perf.inbox, [{ type: 'instrument-change-request-sent', newInstrument: 'bass' }])
    assert.deepEqual(viewer.inbox, [])
  })

  it('2. a duplicate pending request and a non-performer request are ignored', async () => {
    const { host, perf, viewer, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    perf.send({ type: 'request-instrument-change', newInstrument: 'keys' })
    viewer.send({ type: 'request-instrument-change', newInstrument: 'tuba' })
    await settleAll(viewer, all)
    await settleAll(perf, all)
    assert.deepEqual(types(host), ['instrument-change-request-received'])
    assert.deepEqual(types(perf), ['instrument-change-request-sent'])
    assert.deepEqual(viewer.inbox, [])
  })

  it('3. approve: requester gets approved, everyone else gets participant-instrument-changed, room-state shows it', async () => {
    const { roomId, host, perf, viewer, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    await settleAll(perf, all)
    clear(...all)
    host.send({ type: 'approve-instrument-change', targetId: perf.id })
    await settleAll(host, all)
    assert.deepEqual(perf.inbox, [{ type: 'instrument-change-approved', newInstrument: 'bass' }])
    const changed = { type: 'participant-instrument-changed', oderId: perf.id, instrument: 'bass' }
    assert.deepEqual(host.inbox, [changed])
    assert.deepEqual(viewer.inbox, [changed])

    const late = await enter(url, null, roomId)
    assert.equal(last(late, 'room-state').participants.find((p: any) => p.oderId === perf.id).instrument, 'bass')

    // 처리된 요청을 다시 승인해도 아무 일 없음
    clear(...all)
    host.send({ type: 'approve-instrument-change', targetId: perf.id })
    await settleAll(host, all)
    assert.deepEqual(all.map(types), [[], [], []])
  })

  it('4. reject: requester gets the reason (or the default), and may request again', async () => {
    const { host, perf, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    await settleAll(perf, all)
    host.send({ type: 'reject-instrument-change', targetId: perf.id, reason: 'not now' })
    await settleAll(host, all)
    assert.deepEqual(last(perf, 'instrument-change-rejected'), { type: 'instrument-change-rejected', reason: 'not now' })

    clear(...all)
    perf.send({ type: 'request-instrument-change', newInstrument: 'keys' })
    await settleAll(perf, all)
    assert.deepEqual(types(host), ['instrument-change-request-received'])
    host.send({ type: 'reject-instrument-change', targetId: perf.id })
    await settleAll(host, all)
    assert.deepEqual(last(perf, 'instrument-change-rejected'), { type: 'instrument-change-rejected', reason: '방장이 악기 변경을 거절했습니다.' })
  })

  it('5. cancel: host gets instrument-change-request-cancelled, a later approve does nothing', async () => {
    const { host, perf, viewer, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    await settleAll(perf, all)
    clear(...all)
    perf.send({ type: 'cancel-instrument-change-request' })
    await settleAll(perf, all)
    assert.deepEqual(host.inbox, [{ type: 'instrument-change-request-cancelled', oderId: perf.id }])
    assert.deepEqual(viewer.inbox, [])

    clear(...all)
    host.send({ type: 'approve-instrument-change', targetId: perf.id })
    perf.send({ type: 'cancel-instrument-change-request' }) // 대기 요청이 없으면 알림도 없음
    await settleAll(host, all)
    await settleAll(perf, all)
    assert.deepEqual(all.map(types), [[], [], []])
  })

  it('6. leave with a pending request cancels it; a host who joins later receives queued requests', async () => {
    const { host, perf, viewer, all } = await band()
    perf.send({ type: 'request-instrument-change', newInstrument: 'bass' })
    await settleAll(perf, all)
    clear(...all)
    perf.send({ type: 'leave' })
    await settleAll(perf, all)
    assert.deepEqual(host.inbox, [
      { type: 'instrument-change-request-cancelled', oderId: perf.id },
      { type: 'participant-left', oderId: perf.id },
    ])
    assert.deepEqual(viewer.inbox, [{ type: 'participant-left', oderId: perf.id }])

    // 방장이 없을 때 온 요청은 방장이 입장하면 room-state 뒤에 받는다
    const r2 = room('free')
    const p2 = await enter(url, 'tok:solo', r2)
    p2.send({ type: 'start-performing', instrument: 'guitar' })
    p2.send({ type: 'request-instrument-change', newInstrument: 'drums' })
    await settle(p2)
    assert.deepEqual(last(p2, 'instrument-change-request-sent'), { type: 'instrument-change-request-sent', newInstrument: 'drums' })
    const host2 = await enter(url, 'tok:host', r2)
    const idx = types(host2).indexOf('room-state')
    assert.ok(idx >= 0)
    assert.equal(host2.inbox[idx + 1].type, 'instrument-change-request-received')
    assert.equal(host2.inbox[idx + 1].request.oderId, p2.id)
    assert.equal(host2.inbox[idx + 1].request.newInstrument, 'drums')
  })
})

describe('viewerRelay: from is set by the server, targets outside the room are unreachable', async () => {
  const url = await startWs()

  it('overwrites a client-sent from on offer/answer/ice-candidate', async () => {
    const r = room('free')
    const host = await enter(url, 'tok:host', r)
    const viewer = await enter(url, null, r)
    host.send({ type: 'start-performing', instrument: 'drums' })
    await settleAll(host, [viewer])
    clear(host, viewer)

    host.send({ type: 'offer', to: viewer.id, from: 'spoofed-id', offer: { sdp: 'o' } })
    host.send({ type: 'ice-candidate', to: viewer.id, from: viewer.id, candidate: { c: 1 } })
    viewer.send({ type: 'answer', to: host.id, from: host.id, answer: { sdp: 'a' } })
    await settleAll(host, [viewer])
    await settleAll(viewer, [host])
    assert.deepEqual(viewer.inbox, [
      { type: 'offer', to: viewer.id, from: host.id, offer: { sdp: 'o' } },
      { type: 'ice-candidate', to: viewer.id, from: host.id, candidate: { c: 1 } },
    ])
    assert.deepEqual(host.inbox, [{ type: 'answer', to: host.id, from: viewer.id, answer: { sdp: 'a' } }])
  })

  it('does not relay to a client in another room or to/from a client outside any room', async () => {
    const hostA = await enter(url, 'tok:host', room('free'))
    const viewerB = await enter(url, null, room('free'))
    const lobby = await enter(url, 'tok:lobby') // 방 밖
    hostA.send({ type: 'start-performing', instrument: 'drums' })
    await settle(hostA)
    clear(hostA, viewerB, lobby)
    const all = [hostA, viewerB, lobby]

    hostA.send({ type: 'offer', to: viewerB.id, offer: { sdp: 'o' } })
    hostA.send({ type: 'ice-candidate', to: lobby.id, candidate: { c: 1 } })
    viewerB.send({ type: 'answer', to: hostA.id, answer: { sdp: 'a' } })
    lobby.send({ type: 'offer', to: hostA.id, offer: { sdp: 'x' } })
    await settleAll(hostA, all)
    await settleAll(viewerB, all)
    await settleAll(lobby, all)
    assert.deepEqual(all.map(types), [[], [], []])
  })
})
