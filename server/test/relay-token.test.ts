/**
 * UDP 릴레이 토큰: 서명/검증, Rust 릴레이(relay/src/token.rs)와의 바이트 호환, WS 발급 권한
 */
import { describe, it, after } from 'node:test'
import { readFileSync } from 'fs'
import { join } from 'path'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta } from '../src/services/wsService'
import { relayConfigFromEnv, signRelayToken, verifyRelayToken } from '../src/services/relayToken'
import { authJoin, connect, engineKey, sleep, testVerifier, type TestClient } from './helpers'

const SECRET = 'relay-test-secret-0123456789abcdef!!'
const NOW = 1_800_000_000

describe('relay token format', () => {
  const claims = { roomId: 'room-1', memberId: 'client-a', ssrc: 0xdeadbeef, expiresAt: NOW + 600 }

  it('roundtrips', () => {
    const t = signRelayToken(claims, SECRET)
    assert.deepEqual(verifyRelayToken(t, SECRET, NOW), claims)
  })

  it('rejects expired, forged, and wrong-secret tokens', () => {
    const t = signRelayToken(claims, SECRET)
    assert.equal(verifyRelayToken(t, SECRET, NOW + 600), null, 'expired')
    assert.equal(verifyRelayToken(t, 'another-secret-another-secret-another', NOW), null, 'wrong secret')
    for (const idx of [5, 9, 15, t.length - 1]) {
      const f = Buffer.from(t)
      f[idx] ^= 1
      assert.equal(verifyRelayToken(f, SECRET, NOW), null, `byte ${idx}`)
    }
    assert.equal(verifyRelayToken(t.subarray(0, t.length - 1), SECRET, NOW), null, 'truncated')
  })

  it('validates claim ranges', () => {
    assert.throws(() => signRelayToken({ ...claims, roomId: '' }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, roomId: 'x'.repeat(129) }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, memberId: 'x'.repeat(65) }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, ssrc: 2 ** 32 }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, ssrc: 1.5 }, SECRET))
  })

  it('matches the shared vectors with the Rust relay byte-for-byte', () => {
    // relay/src/token.rs tests::cross_language_vectors 도 같은 파일을 읽는다
    const fixture = JSON.parse(readFileSync(join(__dirname, '../../docs/overhaul/relay-token-vectors.json'), 'utf8'))
    assert.ok(fixture.vectors.length >= 3)
    for (const v of fixture.vectors) {
      const claims = { roomId: v.roomId, memberId: v.memberId, ssrc: v.ssrc, expiresAt: v.expiresAt }
      const t = signRelayToken(claims, v.secret)
      assert.equal(t.toString('hex'), v.hex, v.name)
      assert.deepEqual(verifyRelayToken(t, v.secret, v.expiresAt - 1), claims, `${v.name}: 만료 1초 전 유효`)
      assert.equal(verifyRelayToken(t, v.secret, v.expiresAt), null, `${v.name}: 만료 시각에 무효`)
    }
  })

  it('reads config from env only when complete', () => {
    assert.equal(relayConfigFromEnv({}), null)
    assert.equal(relayConfigFromEnv({ RELAY_SECRET: 'short', RELAY_ADDR: 'relay:5000' }), null)
    assert.equal(relayConfigFromEnv({ RELAY_SECRET: SECRET }), null)
    assert.deepEqual(relayConfigFromEnv({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' }), {
      secret: SECRET,
      addr: 'relay.example:5000',
      ttlSec: 120,
    })
    assert.equal(relayConfigFromEnv({ RELAY_SECRET: SECRET, RELAY_ADDR: 'r:1', RELAY_TOKEN_TTL_SEC: '120' })?.ttlSec, 120)
  })
})

const ROOMS: Record<string, RoomMeta> = { free: { hostId: 'host', freeJoin: true } }
const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(env: Record<string, string>): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test', ...env },
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

const descriptor = (ssrc: number) => ({ ssrc, key: engineKey(ssrc), candidates: [] })

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('relay-token issuance over WS', async () => {
  const url = await startWs({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' })

  it('issues a token bound to room, verified userId, and descriptor ssrc to a performer', async () => {
    const host = await client(url)
    await authJoin(host, 'tok:host', 'free')
    host.send({ type: 'native-descriptor', descriptor: descriptor(0x1234abcd) })
    assert.ok(await host.waitFor(m => m.type === 'relay-token'))
    const msg = host.inbox.find(m => m.type === 'relay-token')
    assert.equal(msg.addr, 'relay.example:5000')
    assert.equal(msg.ssrc, 0x1234abcd)
    const claims = verifyRelayToken(Buffer.from(msg.token, 'base64url'), SECRET, Math.floor(Date.now() / 1000))
    assert.deepEqual(claims, { roomId: 'free', memberId: 'host', ssrc: 0x1234abcd, expiresAt: msg.expiresAt })
    const left = msg.expiresAt - Date.now() / 1000
    assert.ok(left > 100 && left <= 120, `짧은 TTL(120초): ${left}`)
    // 클라이언트 시계와 무관한 상대 만료(PR #10 M1)
    assert.equal(msg.expiresIn, 120)
    assert.equal(msg.expiresAt - msg.serverNow, 120)

    // 갱신 요청
    const before = host.inbox.filter(m => m.type === 'relay-token').length
    host.send({ type: 'relay-token-request' })
    assert.ok(await host.waitFor(() => host.inbox.filter(m => m.type === 'relay-token').length === before + 1))

    // native-leave 후에는 ssrc를 잊으므로 갱신 불가
    host.send({ type: 'native-leave' })
    await sleep(50)
    const afterLeave = host.inbox.filter(m => m.type === 'relay-token').length
    host.send({ type: 'relay-token-request' })
    await sleep(100)
    assert.equal(host.inbox.filter(m => m.type === 'relay-token').length, afterLeave)
  })

  it('does not issue to viewers or anonymous clients', async () => {
    const viewer = await client(url)
    await authJoin(viewer, 'tok:viewer', 'free')
    viewer.send({ type: 'native-descriptor', descriptor: descriptor(1) })
    viewer.send({ type: 'relay-token-request' })
    const anon = await client(url)
    await authJoin(anon, null, 'free')
    anon.send({ type: 'native-descriptor', descriptor: descriptor(2) })
    anon.send({ type: 'relay-token-request' })
    await sleep(150)
    assert.ok(!viewer.has(m => m.type === 'relay-token'))
    assert.ok(!anon.has(m => m.type === 'relay-token'))
  })

  it('rejects a descriptor with invalid ssrc (descriptor-invalid): no relay, no token (PR #48 M1)', async () => {
    const host = await client(url)
    const peer = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(peer, 'tok:p2', 'free')
    peer.send({ type: 'start-performing', instrument: 'bass' })
    await sleep(50)
    host.send({ type: 'native-descriptor', descriptor: { ssrc: -1, key: engineKey(200), candidates: [] } })
    assert.ok(await host.waitFor(m => m.type === 'error' && m.code === 'descriptor-invalid'))
    await sleep(50)
    assert.ok(!peer.has(m => m.type === 'native-descriptor' && m.from === host.id))
    assert.ok(!host.has(m => m.type === 'relay-token'))
  })
})

describe('relay review fixes', async () => {
  const url = await startWs({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' })
  const tokens = (c: TestClient) => c.inbox.filter(m => m.type === 'relay-token')

  it('M1: rejects a descriptor whose ssrc another user in the room already uses', async () => {
    const host = await client(url)
    const thief = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(thief, 'tok:thief', 'free')
    thief.send({ type: 'start-performing', instrument: 'bass' })
    await sleep(50)
    host.send({ type: 'native-descriptor', descriptor: descriptor(0xabc) })
    assert.ok(await host.waitFor(m => m.type === 'relay-token'))
    thief.send({ type: 'native-descriptor', descriptor: { ...descriptor(0xabc), key: engineKey(77) } })
    assert.ok(await thief.waitFor(m => m.type === 'error' && m.code === 'ssrc-conflict'))
    await sleep(50)
    assert.equal(tokens(thief).length, 0)
    assert.ok(!host.has(m => m.type === 'native-descriptor' && m.from === thief.id), '충돌 descriptor는 중계하지 않음')
    // 다른 ssrc면 정상
    thief.send({ type: 'native-descriptor', descriptor: descriptor(0xdef) })
    assert.ok(await thief.waitFor(m => m.type === 'relay-token' && m.ssrc === 0xdef))
  })

  it('M2: stop-performing forgets ssrc so the token cannot be renewed', async () => {
    const host = await client(url)
    const p = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(p, 'tok:p9', 'free')
    p.send({ type: 'start-performing', instrument: 'drums' })
    await sleep(50)
    p.send({ type: 'native-descriptor', descriptor: descriptor(0x999) })
    assert.ok(await p.waitFor(m => m.type === 'relay-token'))
    p.send({ type: 'stop-performing' })
    await sleep(50)
    p.send({ type: 'relay-token-request' })
    await sleep(100)
    assert.equal(tokens(p).length, 1)
  })

  it('M3: a reconnect without session resume gets a token with the same memberId', async () => {
    const first = await client(url)
    await authJoin(first, 'tok:host', 'free')
    first.send({ type: 'native-descriptor', descriptor: descriptor(0x111) })
    assert.ok(await first.waitFor(m => m.type === 'relay-token'))
    first.ws.terminate()
    await sleep(50)
    const second = await client(url)
    await authJoin(second, 'tok:host', 'free')
    second.send({ type: 'native-descriptor', descriptor: descriptor(0x222) })
    assert.ok(await second.waitFor(m => m.type === 'relay-token'))
    const now = Math.floor(Date.now() / 1000)
    const a = verifyRelayToken(Buffer.from(tokens(first)[0].token, 'base64url'), SECRET, now)
    const b = verifyRelayToken(Buffer.from(tokens(second)[0].token, 'base64url'), SECRET, now)
    assert.notEqual(first.id, second.id)
    assert.equal(a?.memberId, 'host')
    assert.equal(b?.memberId, 'host', '릴레이가 같은 멤버로 보고 이전 슬롯을 교체')
  })
})

describe('relay disabled without env', async () => {
  const url = await startWs({})

  it('sends no relay-token', async () => {
    const host = await client(url)
    await authJoin(host, 'tok:host', 'free')
    host.send({ type: 'native-descriptor', descriptor: descriptor(7) })
    host.send({ type: 'relay-token-request' })
    await sleep(150)
    assert.ok(!host.has(m => m.type === 'relay-token'))
  })
})
