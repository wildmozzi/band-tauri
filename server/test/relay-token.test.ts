/**
 * UDP 릴레이 토큰: 서명/검증, Rust 릴레이(relay/src/token.rs)와의 바이트 호환, WS 발급 권한
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta } from '../src/services/wsService'
import { relayConfigFromEnv, signRelayToken, verifyRelayToken } from '../src/services/relayToken'
import { authJoin, connect, sleep, testVerifier, type TestClient } from './helpers'

const SECRET = 'relay-test-secret-0123456789abcdef!!'
const NOW = 1_800_000_000

describe('relay token format', () => {
  const claims = { roomId: 'room-1', clientId: 'client-a', ssrc: 0xdeadbeef, expiresAt: NOW + 600 }

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
    assert.throws(() => signRelayToken({ ...claims, clientId: 'x'.repeat(65) }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, ssrc: 2 ** 32 }, SECRET))
    assert.throws(() => signRelayToken({ ...claims, ssrc: 1.5 }, SECRET))
  })

  it('matches the Rust relay test vector byte-for-byte', () => {
    const t = signRelayToken(
      { roomId: 'room-abc', clientId: '11111111-2222-3333-4444-555555555555', ssrc: 0x01020304, expiresAt: 1_700_000_000 },
      'band-relay-test-vector-secret-32bytes!!',
    )
    // relay/src/token.rs tests::TEST_VECTOR_HEX 와 동일해야 한다
    assert.equal(
      t.toString('hex'),
      '01000000006553f1000102030408726f6f6d2d6162632431313131313131312d323232322d333333332d343434342d35353535353535353535353537cb2b9be3db194a0018849fd9df099457dbdc9f39aef51c5f621ba02d8f05b4',
    )
  })

  it('reads config from env only when complete', () => {
    assert.equal(relayConfigFromEnv({}), null)
    assert.equal(relayConfigFromEnv({ RELAY_SECRET: 'short', RELAY_ADDR: 'relay:5000' }), null)
    assert.equal(relayConfigFromEnv({ RELAY_SECRET: SECRET }), null)
    assert.deepEqual(relayConfigFromEnv({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' }), {
      secret: SECRET,
      addr: 'relay.example:5000',
      ttlSec: 600,
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

const descriptor = (ssrc: number) => ({ ssrc, key: 'AAAA', candidates: [] })

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('relay-token issuance over WS', async () => {
  const url = await startWs({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' })

  it('issues a token bound to room, server clientId, and descriptor ssrc to a performer', async () => {
    const host = await client(url)
    await authJoin(host, 'tok:host', 'free')
    host.send({ type: 'native-descriptor', descriptor: descriptor(0x1234abcd) })
    assert.ok(await host.waitFor(m => m.type === 'relay-token'))
    const msg = host.inbox.find(m => m.type === 'relay-token')
    assert.equal(msg.addr, 'relay.example:5000')
    assert.equal(msg.ssrc, 0x1234abcd)
    const claims = verifyRelayToken(Buffer.from(msg.token, 'base64url'), SECRET, Math.floor(Date.now() / 1000))
    assert.deepEqual(claims, { roomId: 'free', clientId: host.id, ssrc: 0x1234abcd, expiresAt: msg.expiresAt })
    assert.ok(msg.expiresAt > Date.now() / 1000 + 500)

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

  it('ignores invalid ssrc but still relays the descriptor', async () => {
    const host = await client(url)
    const peer = await client(url)
    await authJoin(host, 'tok:host', 'free')
    await authJoin(peer, 'tok:p2', 'free')
    peer.send({ type: 'start-performing', instrument: 'bass' })
    await sleep(50)
    host.send({ type: 'native-descriptor', descriptor: { ssrc: -1, key: 'AAAA', candidates: [] } })
    assert.ok(await peer.waitFor(m => m.type === 'native-descriptor' && m.from === host.id))
    await sleep(50)
    assert.ok(!host.has(m => m.type === 'relay-token'))
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
