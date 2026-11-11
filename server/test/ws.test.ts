/**
 * WS 회귀 테스트: H1(연주 시작 권한), M2(insecureDevMode), M1(rejoin/시그널링 역할), M3(rate limit), M4(chat)
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, MAX_ENGINE_CANDIDATES, parseCandidates, parseEngineKey, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { authJoin, connect, engineKey, sleep, testVerifier, type TestClient } from './helpers'

const ROOMS: Record<string, RoomMeta> = {
  locked: { hostId: 'host', freeJoin: false },
  free: { hostId: 'host', freeJoin: true },
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(options: WsOptions): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, options)
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

const backend = (limits: WsOptions['limits'] = {}): WsOptions => ({
  env: { NODE_ENV: 'test' },
  limits,
  deps: {
    hasBackend: true,
    verifyToken: testVerifier,
    fetchRoomMeta: async (roomId) => ROOMS[roomId] ?? null,
    updateParticipants: async () => {},
  },
})

async function client(url: string) {
  const c = await connect(url)
  openClients.push(c)
  return c
}

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('H1: start-performing requires host / free_join / approval', async () => {
  const url = await startWs(backend())

  it('denies a logged-in member in a locked room without approval', async () => {
    const host = await client(url)
    const member = await client(url)
    await authJoin(host, 'tok:host', 'locked')
    await authJoin(member, 'tok:member', 'locked', { isHost: true, userId: 'host' })
    assert.ok(member.has(m => m.type === 'room-state' && m.isHost === false), 'client-sent isHost/userId must be ignored')

    member.send({ type: 'start-performing', instrument: 'guitar', isHost: true })
    assert.ok(await member.waitFor(m => m.type === 'error' && m.code === 'perform-not-approved'))
    await sleep(50)
    assert.ok(!host.has(m => m.type === 'performer-updated' && m.oderId === member.id))

    // 승인 전에는 네이티브 시그널링도 불가
    member.send({ type: 'native-descriptor', descriptor: { ssrc: 1, key: engineKey(1), candidates: [] } })
    await sleep(50)
    assert.ok(!host.has(m => m.type === 'native-descriptor'))

    // 방장 아닌 사람이 approve 시도 → 무시
    member.send({ type: 'request-perform', instrument: 'guitar' })
    assert.ok(await host.waitFor(m => m.type === 'perform-request-received'))
    member.send({ type: 'approve-perform', targetId: member.id })
    await sleep(50)
    assert.ok(!member.has(m => m.type === 'perform-request-approved'))
    member.send({ type: 'start-performing', instrument: 'guitar' })
    await sleep(50)
    assert.equal(member.inbox.filter(m => m.code === 'perform-not-approved').length, 2)

    // 방장 승인 후 허용
    host.send({ type: 'approve-perform', targetId: member.id })
    assert.ok(await member.waitFor(m => m.type === 'perform-request-approved'))
    member.send({ type: 'start-performing', instrument: 'guitar' })
    assert.ok(await host.waitFor(m => m.type === 'performer-updated' && m.oderId === member.id && m.isPerforming === true))

    // leave 후 재입장하면 승인은 사라진다
    member.send({ type: 'leave' })
    await sleep(50)
    member.inbox.length = 0
    member.send({ type: 'join', roomId: 'locked' })
    assert.ok(await member.waitFor(m => m.type === 'room-state'))
    member.send({ type: 'start-performing', instrument: 'guitar' })
    assert.ok(await member.waitFor(m => m.type === 'error' && m.code === 'perform-not-approved'))
  })

  it('allows the host in a locked room', async () => {
    const host = await client(url)
    const viewer = await client(url)
    await authJoin(host, 'tok:host', 'locked')
    await authJoin(viewer, 'tok:viewer2', 'locked')
    host.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await viewer.waitFor(m => m.type === 'performer-updated' && m.oderId === host.id && m.isHost === true))
  })

  it('allows any logged-in member in a free_join room', async () => {
    const a = await client(url)
    const b = await client(url)
    await authJoin(a, 'tok:freeA', 'free')
    await authJoin(b, 'tok:freeB', 'free')
    a.send({ type: 'start-performing', instrument: 'bass' })
    assert.ok(await b.waitFor(m => m.type === 'performer-updated' && m.oderId === a.id))
  })

  it('denies anonymous viewers even in a free_join room', async () => {
    const anon = await client(url)
    await authJoin(anon, null, 'free')
    anon.send({ type: 'start-performing', instrument: 'bass' })
    assert.ok(await anon.waitFor(m => m.type === 'error' && m.code === 'login-required'))
  })

  it('rejects joins to rooms that do not exist', async () => {
    const c = await client(url)
    await authJoin(c, null, 'no-such-room')
    assert.ok(c.has(m => m.type === 'error' && m.code === 'room-not-found'))
  })
})

describe('M2: insecureDevMode requires development + ALLOW_INSECURE_WS=1', async () => {
  const noBackend = (env: Record<string, string>): WsOptions => ({
    env,
    deps: { hasBackend: false, verifyToken: async () => null, fetchRoomMeta: async () => null, updateParticipants: async () => {} },
  })

  for (const env of [{}, { NODE_ENV: 'development' }, { NODE_ENV: 'production', ALLOW_INSECURE_WS: '1' }]) {
    it(`client-sent identity is NOT trusted with env=${JSON.stringify(env)}`, async () => {
      const url = await startWs(noBackend(env))
      const victim = await client(url)
      const attacker = await client(url)
      await authJoin(victim, 'anything', 'r1', { userId: 'victim', isHost: true })
      await authJoin(attacker, 'anything', 'r1', { userId: 'victim', isHost: true })
      assert.ok(attacker.has(m => m.type === 'auth-result' && m.authenticated === false))
      assert.ok(attacker.has(m => m.type === 'room-state' && m.isHost === false))
      attacker.send({ type: 'start-performing', instrument: 'x' })
      assert.ok(await attacker.waitFor(m => m.code === 'login-required'))
      await sleep(50)
      assert.equal(victim.closedCode, null, 'victim must not be kicked as a duplicate connection')
    })
  }

  it('trusts client identity only with explicit opt-in in development', async () => {
    const url = await startWs(noBackend({ NODE_ENV: 'development', ALLOW_INSECURE_WS: '1' }))
    const c = await client(url)
    await authJoin(c, 'anything', 'r1', { userId: 'dev', isHost: true })
    assert.ok(c.has(m => m.type === 'room-state' && m.isHost === true))
  })
})

describe('M1/M4/M3: rejoin, signaling roles, chat, rate limit', async () => {
  // 테스트 클라이언트는 모두 127.0.0.1: IP당 방별 익명 상한(PR #23 H1)은 room-capacity 테스트에서 따로 본다
  const url = await startWs(backend({ chatBurst: 3, chatPerSec: 0.01, maxAnonymousPerIpPerRoom: 100 }))

  it('a fake isRejoin still announces the participant', async () => {
    const watcher = await client(url)
    await authJoin(watcher, 'tok:w', 'free')
    const sneaky = await client(url)
    await authJoin(sneaky, null, 'free', { isRejoin: true })
    assert.ok(await watcher.waitFor(m => m.type === 'participant-joined' && m.participant.oderId === sneaky.id))
  })

  it('does not relay offers between two non-performers', async () => {
    const a = await client(url)
    const b = await client(url)
    await authJoin(a, null, 'free')
    await authJoin(b, null, 'free')
    a.send({ type: 'offer', to: b.id, offer: { sdp: 'x' } })
    await sleep(80)
    assert.ok(!b.has(m => m.type === 'offer'))
  })

  it('rebuilds chat messages and drops non-string payloads', async () => {
    const a = await client(url)
    const b = await client(url)
    await authJoin(a, 'tok:chatA', 'locked')
    await authJoin(b, 'tok:chatB', 'locked')
    a.send({ type: 'chat', message: { evil: true } })
    a.send({ type: 'chat', message: 'hi', timestamp: 1, isHost: true, from: 'spoof' })
    assert.ok(await b.waitFor(m => m.type === 'chat'))
    const chats = b.inbox.filter(m => m.type === 'chat')
    assert.equal(chats.length, 1)
    assert.equal(chats[0].message, 'hi')
    assert.equal(chats[0].from, a.id)
    assert.notEqual(chats[0].timestamp, 1)
    assert.equal(chats[0].isHost, undefined)
  })

  it('rate-limits chat floods', async () => {
    const a = await client(url)
    const b = await client(url)
    await authJoin(a, 'tok:floodA', 'free')
    await authJoin(b, 'tok:floodB', 'free')
    for (let i = 0; i < 20; i++) a.send({ type: 'chat', message: `m${i}` })
    assert.ok(await a.waitFor(m => m.type === 'error' && m.code === 'rate-limited'))
    await sleep(100)
    assert.ok(b.inbox.filter(m => m.type === 'chat').length <= 3)
  })
})

describe('M3: per-IP connection limit', async () => {
  const url = await startWs(backend({ maxConnectionsPerIp: 2 }))
  it('closes connections beyond the limit', async () => {
    await client(url)
    await client(url)
    const { WebSocket } = await import('ws')
    const ws = new WebSocket(url)
    const code = await new Promise<number>(r => ws.on('close', (c) => r(c)))
    assert.equal(code, 1008)
  })
})

describe('P2-3: shared metronome signaling (host verified by DB)', async () => {
  const url = await startWs(backend())
  const desc = (ssrc: number) => ({ ssrc, key: engineKey(ssrc), candidates: [] })
  const state = { running: true, bpm: 120, beatsPerBar: 4, startUs: 1_500_000 }

  it('relays only the host’s valid metronome to performers and replays it to late engines', async () => {
    const host = await client(url)
    const member = await client(url)
    const viewer = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(member, 'tok:metroMember', 'free', { isHost: true, userId: 'host' })
    await authJoin(viewer, 'tok:metroViewer', 'free')
    host.send({ type: 'start-performing', instrument: 'drums' })
    member.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await host.waitFor(m => m.type === 'performer-updated' && m.oderId === member.id))
    member.send({ type: 'native-descriptor', descriptor: desc(12) })
    host.send({ type: 'native-descriptor', descriptor: desc(11) })
    assert.ok(await member.waitFor(m => m.type === 'native-descriptor' && m.from === host.id))

    // 방장이 아니면(클라이언트가 isHost/userId를 보내도) 거부, 중계 없음
    member.send({ type: 'metronome', state })
    assert.ok(await member.waitFor(m => m.type === 'error' && m.code === 'metronome-host-only'))
    // 범위 밖 값은 거부
    host.send({ type: 'metronome', state: { ...state, bpm: 1000 } })
    assert.ok(await host.waitFor(m => m.type === 'error' && m.code === 'metronome-invalid'))
    await sleep(50)
    assert.ok(!host.has(m => m.type === 'metronome'))
    assert.ok(!member.has(m => m.type === 'metronome'))

    host.send({ type: 'metronome', state, from: 'spoofed' })
    const got = await member.waitFor(m => m.type === 'metronome')
    assert.ok(got)
    const msg = member.inbox.find(m => m.type === 'metronome')!
    assert.equal(msg.from, host.id)
    assert.deepEqual(msg.state, state)
    await sleep(50)
    assert.ok(!viewer.has(m => m.type === 'metronome'), '관람자에게는 보내지 않음')

    // 엔진을 다시 띄운(descriptor를 다시 보낸) 연주자에게 현재 상태를 다시 알려 준다
    member.inbox.length = 0
    member.send({ type: 'native-descriptor', descriptor: desc(12) })
    assert.ok(await member.waitFor(m => m.type === 'metronome' && m.from === host.id && (m.state as typeof state).startUs === state.startUs))

    // 방장 엔진이 내려가면 상태를 지운다
    host.send({ type: 'native-leave' })
    assert.ok(await member.waitFor(m => m.type === 'native-leave' && m.from === host.id))
    member.inbox.length = 0
    member.send({ type: 'native-descriptor', descriptor: desc(12) })
    await sleep(100)
    assert.ok(!member.has(m => m.type === 'metronome'))
  })

  it('clears the host metronome and relays a stop when the host engine restarts without native-leave (#36 M1)', async () => {
    const host = await client(url)
    const member = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(member, 'tok:metroMember3', 'free')
    host.send({ type: 'start-performing', instrument: 'drums' })
    member.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await host.waitFor(m => m.type === 'performer-updated' && m.oderId === member.id))
    member.send({ type: 'native-descriptor', descriptor: desc(22) })
    host.send({ type: 'native-descriptor', descriptor: desc(21) })
    assert.ok(await member.waitFor(m => m.type === 'native-descriptor' && m.from === host.id))
    host.send({ type: 'metronome', state })
    assert.ok(await member.waitFor(m => m.type === 'metronome' && (m.state as typeof state).running))

    // 같은 엔진의 재알림(같은 ssrc, WS 재개·회신)은 메트로놈을 건드리지 않는다
    member.inbox.length = 0
    host.send({ type: 'native-descriptor', descriptor: desc(21) })
    assert.ok(await member.waitFor(m => m.type === 'native-descriptor' && m.from === host.id))
    await sleep(50)
    assert.ok(!member.has(m => m.type === 'metronome'))

    // 새 ssrc = 엔진 재시작 → 정지 중계, 늦게 띄운 엔진에도 옛 상태를 다시 보내지 않는다
    host.send({ type: 'native-descriptor', descriptor: desc(23) })
    assert.ok(await member.waitFor(m => m.type === 'metronome' && m.from === host.id && (m.state as typeof state).running === false))
    member.inbox.length = 0
    member.send({ type: 'native-descriptor', descriptor: desc(22) })
    await sleep(100)
    assert.ok(!member.has(m => m.type === 'metronome'))

    // 방장이 새 엔진 기준 startUs로 다시 시작하면 그 값이 중계된다
    host.send({ type: 'metronome', state: { ...state, startUs: 42 } })
    assert.ok(await member.waitFor(m => m.type === 'metronome' && (m.state as typeof state).startUs === 42))
  })

  it('rejects metronome before the host engine is running', async () => {
    const host = await client(url)
    const member = await client(url)
    await authJoin(host, 'tok:host', 'locked')
    await authJoin(member, 'tok:metroMember2', 'locked')
    host.send({ type: 'start-performing', instrument: 'drums' })
    host.send({ type: 'metronome', state })
    await sleep(100)
    assert.ok(!member.has(m => m.type === 'metronome'))
  })
})

describe('P1-1c: engine public key is bound to the JWT user within a room', async () => {
  // 테스트마다 다른 방(연주자 상한 6에 걸리지 않게). 'key-*' 방은 모두 free_join
  const opts = backend({ resumeGraceMs: 300, maxConnectionsPerIp: 100 }) // 테스트가 많아 IP당 연결 상한을 넉넉히
  const url = await startWs({ ...opts, deps: { ...opts.deps, fetchRoomMeta: async (roomId) => roomId.startsWith('key-') ? { hostId: 'host', freeJoin: true } : null } })
  const desc = (ssrc: number, key = engineKey(ssrc)) => ({ ssrc, key, candidates: [{ kind: 'lan', addr: '10.0.0.1:5000' }] })
  const relayedFrom = (c: TestClient, from: string) => c.inbox.filter(m => m.type === 'native-descriptor' && m.from === from)

  async function performers(room: string, ...users: string[]) {
    const cs: TestClient[] = []
    for (const u of users) {
      const c = await client(url)
      await authJoin(c, `tok:${u}`, room)
      c.send({ type: 'start-performing', instrument: 'drums' })
      cs.push(c)
    }
    await sleep(50)
    return cs
  }

  it('parseEngineKey accepts only canonical standard base64 of 32 bytes', () => {
    const k = engineKey(1)
    assert.equal(parseEngineKey(k), k)
    assert.equal(parseEngineKey(undefined), null)
    assert.equal(parseEngineKey('AAAA'), null)
    assert.equal(parseEngineKey(Buffer.alloc(33).toString('base64')), null)
    assert.equal(parseEngineKey(Buffer.alloc(31).toString('base64')), null)
    assert.equal(parseEngineKey(Buffer.alloc(32, 0xfb).toString('base64url')), null, 'URL-safe 표기는 거부')
    assert.equal(parseEngineKey(k.slice(0, 43)), null, '패딩 없음 거부')
    // 남는 비트를 켠 비정규 표기(디코딩하면 같은 키)는 거부: 같은 키를 다른 문자열로 등록하지 못하게
    const zero = engineKey(0)
    const nonCanonical = zero.slice(0, 42) + 'B='
    assert.ok(Buffer.from(nonCanonical, 'base64').equals(Buffer.from(zero, 'base64')))
    assert.equal(parseEngineKey(nonCanonical), null)
  })

  it('rejects malformed keys with descriptor-invalid and does not relay', async () => {
    const [host, member] = await performers('key-1', 'host', 'kvMember')
    for (const key of [undefined, 'AAAA', Buffer.alloc(33, 1).toString('base64'), 'not base64 at all!!']) {
      member.send({ type: 'native-descriptor', descriptor: { ssrc: 5, key, candidates: [] } })
    }
    assert.ok(await member.waitFor(() => member.inbox.filter(x => x.type === 'error' && x.code === 'descriptor-invalid').length === 4))
    await sleep(50)
    assert.equal(relayedFrom(host, member.id).length, 0)
  })

  it('rejects another user\'s descriptor with an already-bound key (key-conflict) and relays server-built fields only', async () => {
    const [host, victim, thief] = await performers('key-2', 'host', 'kcVictim', 'kcThief')
    const k = engineKey(101)
    victim.send({ type: 'native-descriptor', descriptor: { ...desc(101, k), from: 'spoofed', userId: 'host' }, from: 'spoofed' })
    assert.ok(await host.waitFor(() => relayedFrom(host, victim.id).length === 1))
    assert.deepEqual(relayedFrom(host, victim.id)[0].descriptor, desc(101, k), 'descriptor 안의 from/userId는 버린다')

    // 다른 사용자가 같은 키(다른 ssrc)로 위장 → 거부, 중계 없음
    thief.send({ type: 'native-descriptor', descriptor: desc(102, k) })
    assert.ok(await thief.waitFor(m => m.type === 'error' && m.code === 'key-conflict'))
    await sleep(50)
    assert.equal(relayedFrom(host, thief.id).length, 0)
    assert.equal(relayedFrom(victim, thief.id).length, 0)

    // 자기 키면 정상
    thief.send({ type: 'native-descriptor', descriptor: desc(102) })
    assert.ok(await host.waitFor(() => relayedFrom(host, thief.id).length === 1))

    // 같은 사용자의 같은 키 재알림(WS 재개·회신)은 허용
    victim.send({ type: 'native-descriptor', descriptor: desc(101, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, victim.id).length === 2))
  })

  it('binds keys per room: the same key in another room is not a conflict', async () => {
    const k = engineKey(111)
    const [a] = await performers('key-3', 'roomA')
    a.send({ type: 'native-descriptor', descriptor: desc(111, k) })
    await sleep(50)
    const [b] = await performers('key-4', 'roomB')
    b.send({ type: 'native-descriptor', descriptor: desc(112, k) })
    await sleep(100)
    assert.ok(!b.has(m => m.type === 'error' && m.code === 'key-conflict'))
  })

  it('releases the binding on engine restart (new key), native-leave and leave', async () => {
    const [host, owner, other] = await performers('key-5', 'host', 'relOwner', 'relOther')
    const k1 = engineKey(121)
    const k2 = engineKey(122)
    owner.send({ type: 'native-descriptor', descriptor: desc(121, k1) })
    assert.ok(await host.waitFor(() => relayedFrom(host, owner.id).length === 1))

    // 엔진 재시작(새 키·새 ssrc): 옛 키는 풀린다
    owner.send({ type: 'native-descriptor', descriptor: desc(123, k2) })
    assert.ok(await host.waitFor(() => relayedFrom(host, owner.id).length === 2))
    other.send({ type: 'native-descriptor', descriptor: desc(124, k1) })
    assert.ok(await host.waitFor(() => relayedFrom(host, other.id).length === 1))

    // native-leave: 키가 풀린다
    other.send({ type: 'native-leave' })
    owner.send({ type: 'native-leave' })
    assert.ok(await host.waitFor(m => m.type === 'native-leave' && m.from === owner.id))
    other.send({ type: 'native-descriptor', descriptor: desc(125, k2) })
    assert.ok(await host.waitFor(() => relayedFrom(host, other.id).length === 2))

    // 퇴장: 키가 풀린다
    other.send({ type: 'leave' })
    assert.ok(await host.waitFor(m => m.type === 'participant-left' && m.oderId === other.id))
    owner.send({ type: 'native-descriptor', descriptor: desc(126, k2) })
    assert.ok(await host.waitFor(() => relayedFrom(host, owner.id).length === 3))
    assert.ok(!owner.has(m => m.type === 'error' && m.code === 'key-conflict'))
  })

  it('keeps the binding during the resume grace period and releases it when the connection is finally closed', async () => {
    const [host, owner, thief] = await performers('key-6', 'host', 'graceOwner', 'graceThief')
    const k = engineKey(131)
    owner.send({ type: 'native-descriptor', descriptor: desc(131, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, owner.id).length === 1))
    owner.ws.terminate()
    await sleep(50)

    // 유예 중: 끊긴 연주자의 키를 가로챌 수 없다
    thief.send({ type: 'native-descriptor', descriptor: desc(132, k) })
    assert.ok(await thief.waitFor(m => m.type === 'error' && m.code === 'key-conflict'))
    assert.ok(!host.has(m => m.type === 'participant-left' && m.oderId === owner.id), '아직 유예 중')

    // 유예가 끝나 최종 퇴장하면 풀린다
    assert.ok(await host.waitFor(m => m.type === 'participant-left' && m.oderId === owner.id))
    thief.send({ type: 'native-descriptor', descriptor: desc(133, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, thief.id).length === 1))
  })

  it('allows the same user to reuse the key from a new connection (reconnect without resume)', async () => {
    const [host, first] = await performers('key-7', 'host', 'reconnUser')
    const k = engineKey(141)
    first.send({ type: 'native-descriptor', descriptor: desc(141, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, first.id).length === 1))
    const [second] = await performers('key-7', 'reconnUser') // 같은 userId 새 연결: 기존 연결은 중복 정리
    second.send({ type: 'native-descriptor', descriptor: desc(141, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, second.id).length === 1))
    assert.ok(!second.has(m => m.type === 'error' && m.code === 'key-conflict'))
  })

  it('releases the binding when the owner moves to another room (join move → clearNative)', async () => {
    const [host, mover, other] = await performers('key-8', 'host', 'moveOwner', 'moveOther')
    const k = engineKey(151)
    mover.send({ type: 'native-descriptor', descriptor: desc(151, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, mover.id).length === 1))
    // 이동 전: 같은 방의 다른 사용자는 쓸 수 없다
    other.send({ type: 'native-descriptor', descriptor: desc(152, k) })
    assert.ok(await other.waitFor(m => m.type === 'error' && m.code === 'key-conflict'))

    const roomStates = () => mover.inbox.filter(m => m.type === 'room-state').length
    const before = roomStates()
    mover.send({ type: 'join', roomId: 'key-9' })
    assert.ok(await mover.waitFor(() => roomStates() > before))
    assert.ok(await host.waitFor(m => m.type === 'participant-left' && m.oderId === mover.id))

    // 이동 후: 옛 방에서 그 키가 풀렸다
    other.send({ type: 'native-descriptor', descriptor: desc(153, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, other.id).length === 1))
  })

  it('releases the old connection\'s binding when a duplicate connection of the same user replaces it', async () => {
    const [host, first, thief] = await performers('key-10', 'host', 'dupUser', 'dupThief')
    const k = engineKey(161)
    first.send({ type: 'native-descriptor', descriptor: desc(161, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, first.id).length === 1))
    thief.send({ type: 'native-descriptor', descriptor: desc(162, k) })
    assert.ok(await thief.waitFor(m => m.type === 'error' && m.code === 'key-conflict'))

    // 같은 userId의 새 연결이 기존 연결을 정리한다(새 연결은 아직 엔진 descriptor를 보내지 않음) → 옛 바인딩이 풀린다
    await performers('key-10', 'dupUser')
    assert.ok(await host.waitFor(m => m.type === 'participant-left' && m.oderId === first.id))
    thief.send({ type: 'native-descriptor', descriptor: desc(163, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, thief.id).length === 1))
  })

  it('a rejected key-conflict does not overwrite the victim\'s binding: the victim\'s re-announce is still relayed', async () => {
    const [host, victim, thief] = await performers('key-11', 'host', 'ovVictim', 'ovThief')
    const k = engineKey(171)
    victim.send({ type: 'native-descriptor', descriptor: desc(171, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, victim.id).length === 1))
    thief.send({ type: 'native-descriptor', descriptor: desc(172, k) })
    assert.ok(await thief.waitFor(m => m.type === 'error' && m.code === 'key-conflict'))
    // 재알림(같은 키·같은 ssrc)은 여전히 피해자 것으로 중계되고, 공격자는 여전히 거부된다
    victim.send({ type: 'native-descriptor', descriptor: desc(171, k) })
    assert.ok(await host.waitFor(() => relayedFrom(host, victim.id).length === 2))
    assert.ok(!victim.has(m => m.type === 'error'))
    thief.send({ type: 'native-descriptor', descriptor: desc(173, k) })
    assert.ok(await thief.waitFor(() => thief.inbox.filter(m => m.type === 'error' && m.code === 'key-conflict').length === 2))
    await sleep(50)
    assert.equal(relayedFrom(host, thief.id).length, 0)
  })
})

describe('PR #48 M1: descriptor ssrc/candidates are validated and rebuilt by the server', async () => {
  const opts = backend()
  const url = await startWs({ ...opts, deps: { ...opts.deps, fetchRoomMeta: async (roomId) => roomId.startsWith('cand-') ? { hostId: 'host', freeJoin: true } : null } })
  const relayedFrom = (c: TestClient, from: string) => c.inbox.filter(m => m.type === 'native-descriptor' && m.from === from)
  const invalids = (c: TestClient) => c.inbox.filter(m => m.type === 'error' && m.code === 'descriptor-invalid').length

  async function pair(room: string, a: string, b: string) {
    const host = await client(url)
    const member = await client(url)
    await authJoin(host, `tok:${a}`, room)
    await authJoin(member, `tok:${b}`, room)
    host.send({ type: 'start-performing', instrument: 'drums' })
    member.send({ type: 'start-performing', instrument: 'bass' })
    await sleep(50)
    return [host, member]
  }

  it('parseCandidates accepts only {kind, addr:IPv4:port} items, up to 8, and drops extra fields', () => {
    assert.deepEqual(parseCandidates([]), [])
    assert.deepEqual(
      parseCandidates([{ kind: 'lan', addr: '192.168.0.10:50000', from: 'x', userId: 'host', nested: { a: 1 } }, { kind: 'public', addr: '203.0.113.5:1' }]),
      [{ kind: 'lan', addr: '192.168.0.10:50000' }, { kind: 'public', addr: '203.0.113.5:1' }],
    )
    assert.deepEqual(parseCandidates([{ kind: 'lan', addr: '127.0.0.1:9' }]), [{ kind: 'lan', addr: '127.0.0.1:9' }], '루프백 판단은 엔진 몫')
    const eight = Array.from({ length: MAX_ENGINE_CANDIDATES }, (_, i) => ({ kind: 'lan', addr: `10.0.0.${i + 1}:5000` }))
    assert.equal(parseCandidates(eight)?.length, 8)
    assert.equal(parseCandidates([...eight, { kind: 'lan', addr: '10.0.0.9:5000' }]), null, '8개 초과')
    for (const bad of [
      undefined, null, 'x', {}, [null], ['10.0.0.1:5000'], [[]],
      [{ kind: 'turn', addr: '10.0.0.1:5000' }],
      [{ addr: '10.0.0.1:5000' }],
      [{ kind: 'lan' }],
      [{ kind: 'lan', addr: 5000 }],
      [{ kind: 'lan', addr: 'evil.com:5000' }],
      [{ kind: 'lan', addr: '[::1]:5000' }],
      [{ kind: 'lan', addr: '10.0.0.1' }],
      [{ kind: 'lan', addr: '10.0.0.256:5000' }],
      [{ kind: 'lan', addr: '10.0.0.1:0' }],
      [{ kind: 'lan', addr: '10.0.0.1:65536' }],
      [{ kind: 'lan', addr: '10.0.0.01:5000' }],
      [{ kind: 'lan', addr: '10.0.0.1:05000' }],
      [{ kind: 'lan', addr: ' 10.0.0.1:5000' }],
      [{ kind: 'lan', addr: '10.0.0.1:5000\n' }],
      [{ kind: 'lan', addr: '1'.repeat(1000) }],
    ]) {
      assert.equal(parseCandidates(bad), null, JSON.stringify(bad))
    }
  })

  it('strips unknown fields inside candidates when relaying', async () => {
    const [host, member] = await pair('cand-1', 'host', 'candMember1')
    const key = engineKey(201)
    member.send({ type: 'native-descriptor', descriptor: { ssrc: 201, key, candidates: [{ kind: 'lan', addr: '10.0.0.1:5000', from: 'spoofed', userId: 'host', big: 'x'.repeat(1000) }], extra: 1 } })
    assert.ok(await host.waitFor(() => relayedFrom(host, member.id).length === 1))
    assert.deepEqual(relayedFrom(host, member.id)[0].descriptor, { ssrc: 201, key, candidates: [{ kind: 'lan', addr: '10.0.0.1:5000' }] })
  })

  it('rejects invalid ssrc or candidates with descriptor-invalid and does not relay', async () => {
    const [host, member] = await pair('cand-2', 'host', 'candMember2')
    const key = engineKey(211)
    const bad: unknown[] = [
      { ssrc: '211', key, candidates: [] },
      { ssrc: -1, key, candidates: [] },
      { ssrc: 1.5, key, candidates: [] },
      { ssrc: 0x1_0000_0000, key, candidates: [] },
      { key, candidates: [] },
      { ssrc: 211, key },
      { ssrc: 211, key, candidates: Array.from({ length: 9 }, (_, i) => ({ kind: 'lan', addr: `10.0.0.${i + 1}:5000` })) },
      { ssrc: 211, key, candidates: [{ kind: 'lan', addr: 'evil.com:5000' }] },
      { ssrc: 211, key, candidates: [{ kind: 'relay', addr: '10.0.0.1:5000' }] },
    ]
    for (const descriptor of bad) member.send({ type: 'native-descriptor', descriptor })
    assert.ok(await member.waitFor(() => invalids(member) === bad.length))
    await sleep(50)
    assert.equal(relayedFrom(host, member.id).length, 0)
    // 거부된 descriptor는 키를 묶지 않는다: 다른 사용자가 같은 키를 정상 등록할 수 있다
    host.send({ type: 'native-descriptor', descriptor: { ssrc: 212, key, candidates: [] } })
    assert.ok(await member.waitFor(() => relayedFrom(member, host.id).length === 1))
  })
})
