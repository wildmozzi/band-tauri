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
  // 익명 우선순위 (PR #23 H1·M2)
  flood: { hostId: 'host', freeJoin: true, maxParticipants: 4 },
  perip: { hostId: 'host', freeJoin: true, maxParticipants: 10 },
  peroom: { hostId: 'host', freeJoin: true, maxParticipants: 10 },
  anonrace: { hostId: 'host', freeJoin: true, maxParticipants: 3 },
  memberrace: { hostId: 'host', freeJoin: true, maxParticipants: 3 },
  hold: { hostId: 'host', freeJoin: true, maxParticipants: 2 },
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}, env: Record<string, string> = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test', ...env },
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
    assert.equal(await tryJoin(anon, null, 'small'), 'joined') // 익명 관람자: 빈자리가 있으면 입장
    assert.equal(await tryJoin(b, 'tok:b', 'small'), 'joined')
    // 정원(3)이 찼다: 새 익명은 거부
    const anon2 = await client(url)
    assert.equal(await tryJoin(anon2, null, 'small'), 'full')
    assert.equal(anon2.inbox.find(full).reason, 'room-full')
    // 로그인 사용자는 익명을 빼고 센다(2 < 3): 입장, 이미 있는 익명은 밀려나지 않는다 (H1)
    const member = await client(url)
    assert.equal(await tryJoin(member, 'tok:member', 'small'), 'joined')
    await sleep(50)
    assert.ok(!a.has(m => m.type === 'participant-left' && m.oderId === anon.id), 'existing anonymous viewer is not evicted')
    member.send({ type: 'leave' })
    await a.waitFor(m => m.type === 'participant-left' && m.oderId === member.id)
    anon.send({ type: 'leave' })
    await a.waitFor(m => m.type === 'participant-left' && m.oderId === anon.id)
    const c = await client(url)
    assert.equal(await tryJoin(c, 'tok:c', 'small'), 'joined')

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

    // 자리가 비면 들어온다 (방장도 자리를 차지하므로 a·c가 나가면 b·host 2명)
    a.send({ type: 'leave' })
    c.send({ type: 'leave' })
    await b.waitFor(m => m.type === 'participant-left' && m.oderId === c.id)
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

describe('anonymous viewers never lock out logged-in members (PR #23 H1, M2)', async () => {
  // fly 프록시 헤더(Fly-Client-IP)로 연결마다 다른 IP를 흉내 낸다
  const url = await startWs(
    { resumeGraceMs: 200, maxRoomParticipants: 20, maxAnonymousPerRoom: 4, maxAnonymousPerIpPerRoom: 2 },
    { FLY_APP_NAME: 'test' },
  )
  let ipSeq = 0
  const fromIp = (ip = `10.0.${Math.floor(++ipSeq / 250)}.${ipSeq % 250 + 1}`) => client2(url, ip)
  async function client2(u: string, ip: string) {
    const c = await connect(u, { 'fly-client-ip': ip })
    openClients.push(c)
    return c
  }

  it('anonymous connections filling the room do not block members or the host; new anonymous joins are rejected', async () => {
    const anons: TestClient[] = []
    for (let i = 0; i < 4; i++) {
      const c = await fromIp()
      assert.equal(await tryJoin(c, null, 'flood'), 'joined')
      anons.push(c)
    }
    const anon5 = await fromIp()
    assert.equal(await tryJoin(anon5, null, 'flood'), 'full')
    // 로그인 사용자는 정원(4)까지 로그인 사용자끼리만 센다
    const members: TestClient[] = []
    for (let i = 0; i < 4; i++) {
      const c = await fromIp()
      assert.equal(await tryJoin(c, `tok:flood${i}`, 'flood'), 'joined')
      members.push(c)
    }
    const member5 = await fromIp()
    assert.equal(await tryJoin(member5, 'tok:flood5', 'flood'), 'full')
    const host = await fromIp()
    assert.equal(await tryJoin(host, 'tok:host', 'flood'), 'joined')
    await sleep(50)
    for (const a of anons) {
      assert.ok(!members[0].has(m => m.type === 'participant-left' && m.oderId === a.id), 'existing anonymous viewers stay')
      assert.equal(a.closedCode, null)
    }
    for (const c of [...anons, ...members, host]) c.send({ type: 'leave' })
  })

  it('limits anonymous connections per IP and per room', async () => {
    const sameIp = '192.0.2.7'
    const res: string[] = []
    for (let i = 0; i < 3; i++) res.push(await tryJoin(await client2(url, sameIp), null, 'perip'))
    assert.deepEqual(res, ['joined', 'joined', 'full'])
    const err = openClients.at(-1)!.inbox.find(full)
    assert.equal(err.reason, 'anonymous-limit')
    // 같은 IP라도 로그인하면 들어온다
    assert.equal(await tryJoin(await client2(url, sameIp), 'tok:perip-member', 'perip'), 'joined')
    // 다른 IP의 익명은 방별 익명 상한(4)까지
    const more: string[] = []
    for (let i = 0; i < 3; i++) more.push(await tryJoin(await fromIp(), null, 'perip'))
    assert.deepEqual(more, ['joined', 'joined', 'full'])
  })

  it('concurrent anonymous joins never exceed the capacity or the per-IP limit', async () => {
    const distinct: TestClient[] = []
    for (let i = 0; i < 8; i++) distinct.push(await fromIp())
    const same: TestClient[] = []
    for (let i = 0; i < 6; i++) same.push(await client2(url, '198.51.100.9'))
    const all = [...distinct, ...same]
    for (const c of all) c.send({ type: 'auth' })
    await Promise.all(all.map(c => c.waitFor(m => m.type === 'auth-result')))
    for (const c of all) c.send({ type: 'join', roomId: 'anonrace' })
    await Promise.all(all.map(c => c.waitFor(m => joined(m) || full(m))))
    const ok = all.filter(c => c.has(joined))
    assert.equal(ok.length, 3, 'capacity 3')
    assert.ok(same.filter(c => c.has(joined)).length <= 2, 'per-IP anonymous limit')
    // 그 상태에서 로그인 사용자 6명이 동시에 들어와도 정원(3)만큼은 들어온다
    const members: TestClient[] = []
    for (let i = 0; i < 6; i++) members.push(await fromIp())
    for (const [i, c] of members.entries()) c.send({ type: 'auth', accessToken: `tok:mrace${i}` })
    await Promise.all(members.map(c => c.waitFor(m => m.type === 'auth-result')))
    for (const c of members) c.send({ type: 'join', roomId: 'anonrace' })
    await Promise.all(members.map(c => c.waitFor(m => joined(m) || full(m))))
    assert.equal(members.filter(c => c.has(joined)).length, 3)
    for (const c of [...ok, ...members]) c.send({ type: 'leave' })
  })

  it('a performer dropped past the resume grace gets priority to rejoin a full room; a plain viewer does not', async () => {
    const p = await fromIp()
    const v = await fromIp()
    assert.equal(await tryJoin(p, 'tok:hold-p', 'hold'), 'joined')
    assert.equal(await tryJoin(v, 'tok:hold-v', 'hold'), 'joined')
    p.send({ type: 'start-performing', instrument: 'guitar' })
    assert.ok(await v.waitFor(m => m.type === 'performer-updated' && m.oderId === p.id && m.isPerforming === true))
    p.ws.terminate()
    assert.ok(await v.waitFor(m => m.type === 'participant-left' && m.oderId === p.id), 'grace (200ms) expired')
    // 그사이 다른 로그인 사용자가 빈자리를 채운다
    const x = await fromIp()
    assert.equal(await tryJoin(x, 'tok:hold-x', 'hold'), 'joined')
    // 연주자는 새 연결(세션 재개 없이)로도 다시 들어온다
    const p2 = await fromIp()
    assert.equal(await tryJoin(p2, 'tok:hold-p', 'hold'), 'joined')
    // 연주하지 않던 관람자는 예약이 없다: v가 유예 밖으로 끊기면 p2·x(2명)로 정원이 차 있어 못 들어온다
    v.ws.terminate()
    assert.ok(await x.waitFor(m => m.type === 'participant-left' && m.oderId === v.id))
    const v2 = await fromIp()
    assert.equal(await tryJoin(v2, 'tok:hold-v', 'hold'), 'full')
  })
})
