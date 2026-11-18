/**
 * UDP 릴레이 토큰: 서명/검증, Rust 릴레이(relay/src/token.rs)와의 바이트 호환, WS 발급 권한, 회수(RelayRevoke, relay-design 2.5)
 */
import { createSocket } from 'dgram'
import { describe, it, after } from 'node:test'
import { readFileSync } from 'fs'
import { join } from 'path'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta } from '../src/services/wsService'
import { deriveRelayBindKey, relayConfigFromEnv, signRelayRevoke, signRelayToken, verifyRelayRevoke, verifyRelayToken } from '../src/services/relayToken'
import { parseRelayAddr } from '../src/services/relayRevoke'
import { authJoin, connect, engineKey, sleep, testVerifier, waitFor, type TestClient } from './helpers'

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
      assert.equal(deriveRelayBindKey(t, v.secret).toString('hex'), v.bindKeyHex, `${v.name}: bind key`)
    }
  })

  it('matches the shared revoke vectors with the Rust relay byte-for-byte (2.5)', () => {
    // relay/src/token.rs tests::cross_language_revoke_vectors 도 같은 파일을 읽는다
    const fixture = JSON.parse(readFileSync(join(__dirname, '../../docs/overhaul/relay-token-vectors.json'), 'utf8'))
    assert.ok(fixture.revokes.length >= 2)
    for (const v of fixture.revokes) {
      const claims = { roomId: v.roomId, memberId: v.memberId, cutoff: v.cutoff }
      const p = signRelayRevoke(claims, v.secret)
      assert.equal(p.toString('hex'), v.hex, v.name)
      assert.deepEqual(verifyRelayRevoke(p, v.secret), claims, v.name)
    }
  })

  it('revoke: forged or tampered packets do not verify', () => {
    const p = signRelayRevoke({ roomId: 'room-1', memberId: 'user-a', cutoff: NOW + 120 }, SECRET)
    assert.equal(verifyRelayRevoke(p, 'another-secret-another-secret-another'), null)
    for (const idx of [3, 4, 11, 13, p.length - 33, p.length - 1]) {
      const f = Buffer.from(p)
      f[idx] ^= 1
      assert.equal(verifyRelayRevoke(f, SECRET), null, 'byte ' + idx)
    }
    assert.equal(verifyRelayRevoke(p.subarray(0, p.length - 1), SECRET), null)
    assert.throws(() => signRelayRevoke({ roomId: '', memberId: 'u', cutoff: 1 }, SECRET))
    assert.throws(() => signRelayRevoke({ roomId: 'r', memberId: 'x'.repeat(65), cutoff: 1 }, SECRET))
    assert.throws(() => signRelayRevoke({ roomId: 'r', memberId: 'u', cutoff: -1 }, SECRET))
  })

  it('parses RELAY_ADDR for the revoke sender', () => {
    assert.deepEqual(parseRelayAddr('relay.example:5000'), { host: 'relay.example', port: 5000 })
    assert.deepEqual(parseRelayAddr('1.2.3.4:5000'), { host: '1.2.3.4', port: 5000 })
    assert.deepEqual(parseRelayAddr('[2001:db8::1]:5000'), { host: '2001:db8::1', port: 5000 })
    assert.equal(parseRelayAddr('relay.example'), null)
    assert.equal(parseRelayAddr('relay.example:0'), null)
    assert.equal(parseRelayAddr('relay.example:70000'), null)
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

const ROOMS: Record<string, RoomMeta> = { free: { hostId: 'host', freeJoin: true }, free2: { hostId: 'host', freeJoin: true } }
const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

/** relaySender 기본값은 버림(relay.example로 실제 UDP·DNS를 쓰지 않게). null이면 실제 UDP 송신기(services/relayRevoke) */
async function startWs(env: Record<string, string>, relaySender: ((p: Buffer) => void) | null = () => {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test', ...env },
    ...(relaySender ? { relaySender } : {}),
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
    // Bind MAC 키(2.1.1): 그 토큰에서 파생, 32바이트
    assert.equal(msg.bindKey, deriveRelayBindKey(Buffer.from(msg.token, 'base64url'), SECRET).toString('base64url'))
    assert.equal(Buffer.from(msg.bindKey, 'base64url').length, 32)
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

describe('relay token revoke over WS (P2-14c, relay-design 2.5)', async () => {
  const packets: Buffer[] = []
  const url = await startWs({ RELAY_SECRET: SECRET, RELAY_ADDR: 'relay.example:5000' }, p => packets.push(p))
  const revokes = () => packets.map(p => verifyRelayRevoke(p, SECRET))
  const lastToken = (c: TestClient) => c.inbox.filter(m => m.type === 'relay-token').at(-1)

  async function performerWithToken(token: string, ssrc: number) {
    const c = await client(url)
    await authJoin(c, token, 'free')
    c.send({ type: 'start-performing', instrument: 'bass' })
    c.send({ type: 'native-descriptor', descriptor: descriptor(ssrc) })
    assert.ok(await c.waitFor(m => m.type === 'relay-token'))
    return c
  }

  it('stop-performing and native-leave revoke at once (cutoff = last issued expiry); a new start gets a token past the cutoff', async () => {
    const host = await client(url)
    await authJoin(host, 'tok:host', 'free')
    const p = await performerWithToken('tok:rv1', 0x501)
    const n = packets.length
    p.send({ type: 'stop-performing' })
    assert.ok(await waitFor(() => packets.length === n + 1))
    assert.deepEqual(revokes()[n], { roomId: 'free', memberId: 'rv1', cutoff: lastToken(p).expiresAt })
    // 다시 연주를 시작하면 새 토큰의 만료는 cutoff보다 늦다
    p.send({ type: 'start-performing', instrument: 'bass' })
    p.send({ type: 'native-descriptor', descriptor: descriptor(0x501) })
    assert.ok(await p.waitFor(() => p.inbox.filter(m => m.type === 'relay-token').length === 2))
    assert.ok(lastToken(p).expiresAt > revokes()[n]!.cutoff)
    p.send({ type: 'native-leave' })
    assert.ok(await waitFor(() => packets.length === n + 2))
    assert.deepEqual(revokes()[n + 1], { roomId: 'free', memberId: 'rv1', cutoff: lastToken(p).expiresAt })
  })

  it('demote by the host revokes the target', async () => {
    const host = await client(url)
    await authJoin(host, 'tok:host', 'free')
    const p = await performerWithToken('tok:rv2', 0x502)
    const n = packets.length
    host.send({ type: 'demote-performer', targetId: p.id })
    assert.ok(await p.waitFor(m => m.type === 'demoted-to-viewer'))
    assert.ok(await waitFor(() => packets.length === n + 1))
    assert.deepEqual(revokes()[n], { roomId: 'free', memberId: 'rv2', cutoff: lastToken(p).expiresAt })
  })

  it('leave and room move revoke; clients without a token never trigger one', async () => {
    const p = await performerWithToken('tok:rv3', 0x503)
    const viewer = await client(url)
    await authJoin(viewer, 'tok:rv-viewer', 'free')
    const n = packets.length
    viewer.send({ type: 'leave' })
    p.send({ type: 'leave' })
    assert.ok(await waitFor(() => packets.length === n + 1))
    await sleep(50)
    assert.equal(packets.length, n + 1, '토큰을 받은 적 없는 관람자는 회수 없음')
    assert.deepEqual(revokes()[n], { roomId: 'free', memberId: 'rv3', cutoff: lastToken(p).expiresAt })

    const mover = await performerWithToken('tok:rv4', 0x504)
    mover.send({ type: 'join', roomId: 'free2' })
    assert.ok(await waitFor(() => revokes().some(r => r?.memberId === 'rv4')))
    assert.equal(revokes().find(r => r?.memberId === 'rv4')?.roomId, 'free')
  })

  it('a duplicate connection of the same user does not revoke (the user is still there)', async () => {
    const first = await performerWithToken('tok:rv5', 0x505)
    const n = packets.length
    const second = await client(url)
    await authJoin(second, 'tok:rv5', 'free')
    assert.ok(await waitFor(() => first.ws.readyState !== first.ws.OPEN))
    await sleep(50)
    assert.equal(packets.length, n)
  })
})

describe('relay token revoke reaches the relay over real UDP', async () => {
  const sock = createSocket('udp4')
  await new Promise<void>(r => sock.bind(0, '127.0.0.1', () => r()))
  const got: Buffer[] = []
  sock.on('message', m => got.push(m))
  after(() => sock.close())
  const url = await startWs({ RELAY_SECRET: SECRET, RELAY_ADDR: '127.0.0.1:' + sock.address().port }, null)

  it('sends a signed RelayRevoke to RELAY_ADDR (and once more for loss)', async () => {
    const p = await client(url)
    await authJoin(p, 'tok:host', 'free')
    p.send({ type: 'native-descriptor', descriptor: descriptor(0x601) })
    assert.ok(await p.waitFor(m => m.type === 'relay-token'))
    const exp = p.inbox.find(m => m.type === 'relay-token').expiresAt
    p.send({ type: 'native-leave' })
    assert.ok(await waitFor(() => got.length >= 1, 2000))
    assert.deepEqual(verifyRelayRevoke(got[0], SECRET), { roomId: 'free', memberId: 'host', cutoff: exp })
    assert.ok(await waitFor(() => got.length >= 2, 3000), '1초 뒤 한 번 더')
    assert.deepEqual(got[1], got[0])
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
