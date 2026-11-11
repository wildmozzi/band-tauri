/**
 * WS 회귀 테스트: H1(연주 시작 권한), M2(insecureDevMode), M1(rejoin/시그널링 역할), M3(rate limit), M4(chat)
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { authJoin, connect, sleep, testVerifier, type TestClient } from './helpers'

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
    member.send({ type: 'native-descriptor', descriptor: { ssrc: 1 } })
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
  const url = await startWs(backend({ chatBurst: 3, chatPerSec: 0.01 }))

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
  const desc = (ssrc: number) => ({ ssrc, key: 'AAAA', candidates: [] })
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
