/**
 * WS 회귀 테스트: 방 정원 서버 강제 (P3-12b)
 * - rooms.max_participants(관람 포함) 초과 입장은 error 'room-full'
 * - 방장은 항상 입장, 세션 재개·같은 사용자 재연결은 기존 자리 유지
 * - 동시 입장 경합에서도 정원을 넘지 않는다
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { connect, sleep, testVerifier, type TestClient } from './helpers'

const ROOMS: Record<string, RoomMeta> = {
  small: { hostId: 'host', freeJoin: true, maxParticipants: 3 },
  race: { hostId: 'host', freeJoin: true, maxParticipants: 2 },
  resume: { hostId: 'host', freeJoin: true, maxParticipants: 2 },
  dup: { hostId: 'host', freeJoin: true, maxParticipants: 2 },
  other: { hostId: 'host', freeJoin: true, maxParticipants: 5 },
  // max_participants가 없으면 서버 상한(maxRoomParticipants)
  nolimit: { hostId: 'host', freeJoin: true },
  // DB 값이 서버 상한보다 크면 서버 상한
  huge: { hostId: 'host', freeJoin: true, maxParticipants: 1000 },
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits: { joinBurst: 100, joinPerSec: 100, maxConnectionsPerIp: 200, ...limits },
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

const joined = (m: any) => m.type === 'room-state'
const full = (m: any) => m.type === 'error' && m.code === 'room-full'

/** auth 후 join, room-state 또는 room-full 중 먼저 온 것 */
async function tryJoin(c: TestClient, token: string | null, roomId: string, extra: Record<string, unknown> = {}): Promise<'joined' | 'full'> {
  c.send(token ? { type: 'auth', accessToken: token, ...extra } : { type: 'auth' })
  c.send({ type: 'join', roomId })
  const ok = await c.waitFor(m => joined(m) || full(m))
  if (!ok) throw new Error('join timed out')
  return c.has(joined) ? 'joined' : 'full'
}

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('room capacity (max_participants, viewers included)', async () => {
  const url = await startWs({ resumeGraceMs: 400, maxRoomParticipants: 6 })

  it('rejects joins over max_participants with room-full, host always gets in, leave frees a seat', async () => {
    const a = await client(url)
    const b = await client(url)
    const anon = await client(url)
    assert.equal(await tryJoin(a, 'tok:a', 'small'), 'joined')
    assert.equal(await tryJoin(b, 'tok:b', 'small'), 'joined')
    assert.equal(await tryJoin(anon, null, 'small'), 'joined') // 익명 관람자도 정원에 포함

    const late = await client(url)
    assert.equal(await tryJoin(late, 'tok:late', 'small'), 'full')
    const err = late.inbox.find(full)
    assert.equal(err.roomId, 'small')
    assert.equal(err.maxParticipants, 3)
    await sleep(50)
    assert.ok(!a.has(m => m.type === 'participant-joined' && m.participant.oderId === late.id), 'rejected join is not broadcast')

    // 방장은 정원이 차도 입장
    const host = await client(url)
    assert.equal(await tryJoin(host, 'tok:host', 'small'), 'joined')

    // 자리가 비면 들어온다 (방장도 자리를 차지하므로 a·anon이 나가면 b·host 2명)
    a.send({ type: 'leave' })
    anon.send({ type: 'leave' })
    await b.waitFor(m => m.type === 'participant-left' && m.oderId === anon.id)
    late.send({ type: 'join', roomId: 'small' })
    assert.ok(await late.waitFor(joined))
  })

  it('a rejected join keeps the client in its current room', async () => {
    const fillers: TestClient[] = []
    for (let i = 0; i < 2; i++) {
      const c = await client(url)
      assert.equal(await tryJoin(c, `tok:keep${i}`, 'race'), 'joined')
      fillers.push(c)
    }
    const mover = await client(url)
    const peer = await client(url)
    assert.equal(await tryJoin(mover, 'tok:mover', 'other'), 'joined')
    assert.equal(await tryJoin(peer, 'tok:peer', 'other'), 'joined')
    mover.send({ type: 'join', roomId: 'race' })
    assert.ok(await mover.waitFor(full))
    await sleep(50)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === mover.id))
    mover.send({ type: 'chat', message: 'still here' })
    assert.ok(await peer.waitFor(m => m.type === 'chat' && m.message === 'still here'))
    for (const c of fillers) c.send({ type: 'leave' })
  })

  it('concurrent joins never exceed the capacity', async () => {
    const roomId = 'race'
    await sleep(50) // 이전 테스트의 leave 반영
    const cs: TestClient[] = []
    for (let i = 0; i < 8; i++) {
      const c = await client(url)
      c.send({ type: 'auth', accessToken: `tok:racer${i}` })
      cs.push(c)
    }
    await Promise.all(cs.map(c => c.waitFor(m => m.type === 'auth-result')))
    for (const c of cs) c.send({ type: 'join', roomId })
    await Promise.all(cs.map(c => c.waitFor(m => joined(m) || full(m))))
    const ok = cs.filter(c => c.has(joined))
    const rejected = cs.filter(c => c.has(full))
    assert.equal(ok.length, 2)
    assert.equal(rejected.length, 6)
    // 방장이 들어와서 보면 방에는 정원(2)만큼만 있다 (participants는 자기 자신 제외)
    const host = await client(url)
    assert.equal(await tryJoin(host, 'tok:host', roomId), 'joined')
    const seen = host.inbox.filter(joined).at(-1).participants.map((p: any) => p.oderId).sort()
    assert.deepEqual(seen, ok.map(c => c.id).sort())
    for (const c of [...ok, host]) c.send({ type: 'leave' })
  })

  it('a suspended session keeps its seat and can resume while the room is full', async () => {
    const a = await client(url)
    const b = await client(url)
    assert.equal(await tryJoin(a, 'tok:ra', 'resume'), 'joined')
    assert.equal(await tryJoin(b, 'tok:rb', 'resume'), 'joined')
    const oldId = a.id
    a.ws.terminate()
    await sleep(50)

    // 유예 중: 자리는 그대로라 다른 사람은 못 들어온다
    const intruder = await client(url)
    assert.equal(await tryJoin(intruder, 'tok:intruder', 'resume'), 'full')

    // 같은 사용자는 resumeClientId로 재개 + rejoin
    const a2 = await client(url)
    a2.send({ type: 'auth', accessToken: 'tok:ra', resumeClientId: oldId })
    assert.ok(await a2.waitFor(m => m.type === 'welcome' && m.resumed === true && m.clientId === oldId))
    a2.send({ type: 'join', roomId: 'resume', isRejoin: true })
    assert.ok(await a2.waitFor(joined))
    assert.ok(!a2.has(full))

    // 유예가 끝나 자리가 비면 다른 사람이 들어온다
    b.ws.terminate()
    await sleep(600)
    intruder.send({ type: 'join', roomId: 'resume' })
    assert.ok(await intruder.waitFor(joined))
  })

  it('the same user reconnecting with a new connection replaces the old seat instead of being rejected', async () => {
    const a = await client(url)
    const b = await client(url)
    assert.equal(await tryJoin(a, 'tok:da', 'dup'), 'joined')
    assert.equal(await tryJoin(b, 'tok:db', 'dup'), 'joined')
    const a2 = await client(url)
    assert.equal(await tryJoin(a2, 'tok:da', 'dup'), 'joined')
    assert.ok(await b.waitFor(m => m.type === 'participant-left' && m.oderId === a.id))
    // room-state.participants는 자기 자신을 뺀 목록: b만 보인다
    const state = a2.inbox.filter(joined).at(-1)
    assert.deepEqual(state.participants.map((p: any) => p.oderId), [b.id])
  })

  it('without max_participants (or above it) the server cap applies', async () => {
    for (const roomId of ['nolimit', 'huge']) {
      const results: string[] = []
      for (let i = 0; i < 7; i++) {
        const c = await client(url)
        results.push(await tryJoin(c, `tok:${roomId}${i}`, roomId))
      }
      assert.deepEqual(results, ['joined', 'joined', 'joined', 'joined', 'joined', 'joined', 'full'])
    }
  })
})
