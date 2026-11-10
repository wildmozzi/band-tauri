/**
 * P2-6: 방 비밀번호/비공개 방
 * - 해시(scrypt) 왕복, 평문 미저장
 * - WS join 비밀번호 검증, 실패 횟수 제한, 방장 우회, 세션 내 재입장
 * - POST /api/rooms, PUT /api/rooms/:id/password (방장 = 토큰 userId, 해시만 저장)
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import {
  hashRoomPassword,
  verifyRoomPassword,
  validateRoomPassword,
  PasswordAttemptLimiter,
  ScryptGate,
  ScryptBusyError,
  SCRYPT_PARAMS,
  MAX_SCRYPT_MEMORY,
  defaultScryptGate,
  ipBucket,
  scryptMemory,
} from '../src/lib/roomPassword'
import type { NewRoom, RoomRepository, RoomRow } from '../src/repositories/roomRepository'
import { createRoomService } from '../src/services/roomService'
import { authJoin, bearer, connect, sleep, testVerifier, type TestClient } from './helpers'

describe('room password hashing', () => {
  it('round-trips and never embeds the plaintext', async () => {
    const hash = await hashRoomPassword('jam-session-42')
    assert.match(hash, /^scrypt\$\d+\$\d+\$\d+\$[\w-]+\$[\w-]+$/)
    assert.ok(!hash.includes('jam-session-42'))
    assert.equal(await verifyRoomPassword('jam-session-42', hash), true)
    assert.equal(await verifyRoomPassword('jam-session-43', hash), false)
    assert.equal(await verifyRoomPassword('', hash), false)
  })

  it('uses a fresh salt per hash', async () => {
    assert.notEqual(await hashRoomPassword('same'), await hashRoomPassword('same'))
  })

  it('rejects malformed or hostile hashes without throwing', async () => {
    for (const bad of ['', 'plain', 'bcrypt$x', 'scrypt$3$8$1$c2FsdA$aGFzaA', 'scrypt$1073741824$8$1$c2FsdA$aGFzaA', 'scrypt$16$8$1$$']) {
      assert.equal(await verifyRoomPassword('x', bad), false, bad)
    }
  })

  it('validates length (min 6, review M1)', () => {
    assert.ok(validateRoomPassword('abcde'))
    assert.equal(validateRoomPassword('abcdef'), null)
    assert.ok(validateRoomPassword('x'.repeat(65)))
    assert.ok(validateRoomPassword(1234))
  })

  it('normalizes to NFC so composed/decomposed Hangul match (review L5)', async () => {
    const nfc = '합주실비번'.normalize('NFC')
    const nfd = nfc.normalize('NFD')
    assert.notEqual(nfc, nfd)
    const hash = await hashRoomPassword(nfd)
    assert.equal(await verifyRoomPassword(nfc, hash), true)
  })
})

describe('scrypt memory bound (review H1)', () => {
  it('default params use at most 16MiB per call (OWASP-equivalent N=2^14, r=8, p=5)', async () => {
    assert.deepEqual(SCRYPT_PARAMS, { N: 1 << 14, r: 8, p: 5 })
    assert.ok(scryptMemory(SCRYPT_PARAMS.N, SCRYPT_PARAMS.r) <= 16 * 1024 * 1024)
    assert.match(await hashRoomPassword('abcdefgh'), /^scrypt\$16384\$8\$5\$/)
  })

  it('refuses stored hashes whose parameters would exceed the memory cap', async () => {
    const tooBig = 'scrypt$' + (1 << 16) + '$8$1$c2FsdA$aGFzaA' // 64MiB
    assert.ok(scryptMemory(1 << 16, 8) > MAX_SCRYPT_MEMORY)
    assert.equal(await verifyRoomPassword('x', tooBig), false)
  })

  it('runs at most maxConcurrent at once and rejects immediately when the queue is full', async () => {
    const gate = new ScryptGate(2, 3)
    let release!: () => void
    const blocker = new Promise<void>(r => { release = r })
    const jobs = Array.from({ length: 5 }, () => gate.run(() => blocker))
    await sleep(5)
    assert.equal(gate.running, 2)
    assert.equal(gate.pending, 3)
    await assert.rejects(gate.run(async () => 1), ScryptBusyError)
    release()
    await Promise.all(jobs)
    assert.equal(gate.peak, 2)
    assert.equal(gate.running, 0)
  })

  it('20 concurrent verifications through the shared gate never run more than 2 scrypt at once', async () => {
    const hash = await hashRoomPassword('concurrency')
    const gate = new ScryptGate(2, 32)
    const results = await Promise.all(Array.from({ length: 20 }, (_, i) => verifyRoomPassword(i % 2 ? 'concurrency' : 'nope-nope', hash, gate)))
    assert.equal(results.filter(Boolean).length, 10)
    assert.equal(gate.peak, 2)
    assert.ok(defaultScryptGate.maxConcurrent <= 2)
  })
})

describe('ipBucket (review M1)', () => {
  it('groups IPv6 addresses by /64 and keeps IPv4 as is', () => {
    assert.equal(ipBucket('203.0.113.7'), '203.0.113.7')
    assert.equal(ipBucket('::ffff:203.0.113.7'), '203.0.113.7')
    assert.equal(ipBucket('2001:db8:1:2:aaaa::1'), '2001:db8:1:2::/64')
    assert.equal(ipBucket('2001:0db8:0001:0002:ffff:ffff:ffff:ffff'), '2001:db8:1:2::/64')
    assert.equal(ipBucket('2001:db8::1'), '2001:db8:0:0::/64')
    assert.equal(ipBucket('fe80::1%eth0'), 'fe80:0:0:0::/64')
    assert.notEqual(ipBucket('2001:db8:1:3::1'), ipBucket('2001:db8:1:2::1'))
    assert.equal(ipBucket('unknown'), 'unknown')
  })
})

describe('PasswordAttemptLimiter', () => {
  it('counts IPv6 addresses in the same /64 together', () => {
    const l = new PasswordAttemptLimiter({ maxFailures: 100, maxFailuresPerIp: 2, maxFailuresPerRoom: 1000, windowMs: 60_000 })
    assert.equal(l.begin('r', 'u:a', '2001:db8:1:2::1').ok, true)
    assert.equal(l.begin('r', 'u:b', '2001:db8:1:2::2').ok, true)
    const locked = l.begin('r', 'u:c', '2001:db8:1:2:dead:beef::9')
    assert.equal(locked.ok, false)
    assert.equal(locked.ok === false && locked.scope, 'ip')
    assert.equal(l.begin('r', 'u:c', '2001:db8:1:3::1').ok, true)
  })

  it('caps total failures per room across users and networks', () => {
    const l = new PasswordAttemptLimiter({ maxFailures: 100, maxFailuresPerIp: 100, maxFailuresPerRoom: 3, windowMs: 60_000 })
    for (let i = 0; i < 3; i++) assert.equal(l.begin('r', `u:${i}`, `198.51.100.${i}`).ok, true)
    const locked = l.begin('r', 'u:x', '192.0.2.1')
    assert.equal(locked.ok === false && locked.scope, 'room')
    assert.equal(l.begin('other-room', 'u:x', '192.0.2.1').ok, true)
  })

  it('cancelled() refunds an attempt that was never verified', () => {
    const l = new PasswordAttemptLimiter({ maxFailures: 1, maxFailuresPerIp: 1, maxFailuresPerRoom: 1, windowMs: 60_000 })
    assert.equal(l.begin('r', 'u:a', 'ip').ok, true)
    l.cancelled('r', 'u:a', 'ip')
    assert.equal(l.begin('r', 'u:a', 'ip').ok, true)
  })

  it('locks a user after maxFailures and resets after the window', () => {
    let now = 0
    const l = new PasswordAttemptLimiter({ maxFailures: 3, maxFailuresPerIp: 100, maxFailuresPerRoom: 1000, windowMs: 1000 }, () => now)
    assert.deepEqual(l.begin('r', 'u:a', 'ip1'), { ok: true, remaining: 2 })
    assert.deepEqual(l.begin('r', 'u:a', 'ip1'), { ok: true, remaining: 1 })
    assert.deepEqual(l.begin('r', 'u:a', 'ip1'), { ok: true, remaining: 0 })
    const locked = l.begin('r', 'u:a', 'ip1')
    assert.equal(locked.ok, false)
    // 다른 방, 다른 사용자는 영향 없음
    assert.equal(l.begin('r2', 'u:a', 'ip1').ok, true)
    assert.equal(l.begin('r', 'u:b', 'ip1').ok, true)
    now = 1001
    assert.equal(l.begin('r', 'u:a', 'ip1').ok, true)
  })

  it('limits per IP across users, and success refunds only that attempt', () => {
    const l = new PasswordAttemptLimiter({ maxFailures: 10, maxFailuresPerIp: 2, maxFailuresPerRoom: 1000, windowMs: 60_000 })
    assert.equal(l.begin('r', 'u:a', 'ip').ok, true)
    l.succeeded('r', 'u:a', 'ip')
    assert.equal(l.begin('r', 'u:b', 'ip').ok, true)
    assert.equal(l.begin('r', 'u:c', 'ip').ok, true)
    assert.equal(l.begin('r', 'u:d', 'ip').ok, false)
    assert.equal(l.begin('r', 'u:d', 'other-ip').ok, true)
  })
})

// ---------------------------------------------------------------- WS

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []
const ROOMS: Record<string, RoomMeta> = {}

async function startWs(options: WsOptions): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, options)
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

const backend = (limits: WsOptions['limits'] = {}): WsOptions => ({
  env: { NODE_ENV: 'test' },
  limits: { joinBurst: 100, joinPerSec: 100, ...limits },
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

/** auth 후 join 응답(room-state 또는 error)을 기다려 돌려준다 */
async function tryJoin(c: TestClient, roomId: string, password?: string, token?: string | null) {
  if (token !== undefined) c.send(token ? { type: 'auth', accessToken: token } : { type: 'auth' })
  const before = c.inbox.length
  c.send({ type: 'join', roomId, ...(password !== undefined ? { password } : {}) })
  const isReply = (m: any) => m.type === 'room-state' || (m.type === 'error' && m.code !== 'auth-required')
  const ok = await c.waitFor(m => c.inbox.indexOf(m) >= before && isReply(m))
  assert.ok(ok, 'join reply timed out')
  return c.inbox.slice(before).find(isReply)
}

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('WS join: room password', () => {
  let url = ''
  before(async () => {
    ROOMS.secret = { hostId: 'host', freeJoin: true, passwordHash: await hashRoomPassword('open-sesame') }
    ROOMS.lobby = { hostId: 'host', freeJoin: true }
    url = await startWs(backend({ passwordMaxFailures: 3, passwordMaxFailuresPerIp: 50 }))
  })

  it('requires a password, rejects a wrong one, accepts the right one', async () => {
    const c = await client(url)
    const host = await client(url)
    await authJoin(host, 'tok:host', 'secret')

    const noPw = await tryJoin(c, 'secret', undefined, 'tok:alice')
    assert.equal(noPw.code, 'password-required')
    const wrong = await tryJoin(c, 'secret', 'nope')
    assert.equal(wrong.code, 'password-invalid')
    assert.equal(wrong.remainingAttempts, 2)
    assert.equal(wrong.roomId, 'secret')
    await sleep(30)
    assert.ok(!host.has(m => m.type === 'participant-joined' && m.participant?.oderId === c.id), 'failed join must not be announced')

    const ok = await tryJoin(c, 'secret', 'open-sesame')
    assert.equal(ok.type, 'room-state')
    assert.ok(await host.waitFor(m => m.type === 'participant-joined' && m.participant?.oderId === c.id))
    // 이미 방에 있는 연결의 재 join(세션 재개 후 클라이언트 rejoin)은 다시 묻지 않는다
    const again = await tryJoin(c, 'secret')
    assert.equal(again.type, 'room-state')
  })

  it('lets the host in without a password', async () => {
    const host = await client(url)
    const r = await tryJoin(host, 'secret', undefined, 'tok:host')
    assert.equal(r.type, 'room-state')
    assert.equal(r.isHost, true)
  })

  it('does not trust client-sent isHost/userId to bypass the password', async () => {
    const c = await client(url)
    c.send({ type: 'auth', accessToken: 'tok:mallory' })
    c.send({ type: 'join', roomId: 'secret', isHost: true, userId: 'host' })
    assert.ok(await c.waitFor(m => m.type === 'error' && m.code === 'password-required'))
  })

  it('anonymous viewers also need the password', async () => {
    const anon = await client(url)
    const r = await tryJoin(anon, 'secret', undefined, null)
    assert.equal(r.code, 'password-required')
    const ok = await tryJoin(anon, 'secret', 'open-sesame')
    assert.equal(ok.type, 'room-state')
  })

  it('rate-limits repeated failures, even if the right password comes next', async () => {
    const c = await client(url)
    await tryJoin(c, 'secret', 'bad1', 'tok:bruteforcer')
    await tryJoin(c, 'secret', 'bad2')
    const third = await tryJoin(c, 'secret', 'bad3')
    assert.equal(third.remainingAttempts, 0)
    const locked = await tryJoin(c, 'secret', 'open-sesame')
    assert.equal(locked.code, 'password-rate-limited')
    assert.ok(locked.retryAfterMs > 0)

    // 재접속해도 같은 사용자면 계속 잠김
    const c2 = await client(url)
    const stillLocked = await tryJoin(c2, 'secret', 'open-sesame', 'tok:bruteforcer')
    assert.equal(stillLocked.code, 'password-rate-limited')

    // 다른 사용자는 영향 없음, 비밀번호 없는 방도 영향 없음
    const other = await client(url)
    assert.equal((await tryJoin(other, 'secret', 'open-sesame', 'tok:bob')).type, 'room-state')
    assert.equal((await tryJoin(c2, 'lobby')).type, 'room-state')
  })

  it('a failed join keeps the client in its current room', async () => {
    const host = await client(url)
    const c = await client(url)
    await authJoin(host, 'tok:host', 'lobby')
    assert.equal((await tryJoin(c, 'lobby', undefined, 'tok:carol')).type, 'room-state')
    const r = await tryJoin(c, 'secret', 'wrong')
    assert.equal(r.code, 'password-invalid')
    await sleep(30)
    assert.ok(!host.has(m => m.type === 'participant-left' && m.oderId === c.id))
  })
})

describe('WS join: scrypt busy / fail-closed (review H1, L1)', () => {
  it('rejects with password-busy without consuming an attempt when the scrypt queue is full', async () => {
    ROOMS.busy = { hostId: 'host', freeJoin: true, passwordHash: await hashRoomPassword('busy-room') }
    const gate = new ScryptGate(1, 0)
    const url = await startWs({ ...backend({ passwordMaxFailures: 1 }), scryptGate: gate })
    let release!: () => void
    const occupied = gate.run(() => new Promise<void>(r => { release = r }))
    const c = await client(url)
    const r = await tryJoin(c, 'busy', 'busy-room', 'tok:busy-user')
    assert.equal(r.code, 'password-busy')
    assert.ok(r.retryAfterMs > 0)
    release()
    await occupied
    // 시도 횟수가 차감되지 않았으므로 (한도 1) 맞는 비밀번호로 들어갈 수 있다
    assert.equal((await tryJoin(c, 'busy', 'busy-room')).type, 'room-state')
  })

  it('holds non-host joins while has_password is set but the hash is not stored yet', async () => {
    ROOMS.pending = { hostId: 'host', freeJoin: true, hasPassword: true, passwordHash: null }
    const url = await startWs(backend())
    const c = await client(url)
    assert.equal((await tryJoin(c, 'pending', undefined, 'tok:early-bird')).code, 'password-busy')
    const host = await client(url)
    assert.equal((await tryJoin(host, 'pending', undefined, 'tok:host')).type, 'room-state')
  })
})

describe('WS join: per-IP limit across accounts', () => {
  it('locks the IP after maxFailuresPerIp failures from different users', async () => {
    ROOMS.ipcheck = { hostId: 'host', freeJoin: true, passwordHash: await hashRoomPassword('right-one') }
    const url = await startWs(backend({ passwordMaxFailures: 10, passwordMaxFailuresPerIp: 2 }))
    const a = await client(url)
    const b = await client(url)
    const c = await client(url)
    assert.equal((await tryJoin(a, 'ipcheck', 'x1', 'tok:ip-a')).code, 'password-invalid')
    assert.equal((await tryJoin(b, 'ipcheck', 'x2', 'tok:ip-b')).code, 'password-invalid')
    assert.equal((await tryJoin(c, 'ipcheck', 'right-one', 'tok:ip-c')).code, 'password-rate-limited')
    // 방장은 제한과 무관
    const host = await client(url)
    assert.equal((await tryJoin(host, 'ipcheck', undefined, 'tok:host')).type, 'room-state')
  })
})

describe('WS perform requests (P2-6 UX)', () => {
  it('tells the requester whether the host is online and delivers queued requests when the host joins', async () => {
    ROOMS.approval = { hostId: 'host', freeJoin: false }
    const url = await startWs(backend())
    const m = await client(url)
    await authJoin(m, 'tok:member', 'approval')
    m.send({ type: 'request-perform', instrument: 'guitar' })
    assert.ok(await m.waitFor(x => x.type === 'perform-request-sent' && x.hostOnline === false))
    // 재전송은 중복 요청을 만들지 않고 대기 상태만 다시 알려준다
    m.send({ type: 'request-perform', instrument: 'guitar' })
    await m.waitFor(() => m.inbox.filter(x => x.type === 'perform-request-sent').length === 2)

    const host = await client(url)
    await authJoin(host, 'tok:host', 'approval')
    assert.ok(await host.waitFor(x => x.type === 'perform-request-received' && x.request.oderId === m.id))
    assert.equal(host.inbox.filter(x => x.type === 'perform-request-received').length, 1)

    host.send({ type: 'reject-perform', targetId: m.id, reason: '  지금은 드럼만 받아요  ' })
    assert.ok(await m.waitFor(x => x.type === 'perform-request-rejected' && x.reason === '지금은 드럼만 받아요' && x.instrument === 'guitar'))

    m.send({ type: 'request-perform', instrument: 'drums' })
    assert.ok(await host.waitFor(x => x.type === 'perform-request-received' && x.request.instrument === 'drums'))
    assert.ok(await m.waitFor(x => x.type === 'perform-request-sent' && x.hostOnline === true))
    host.send({ type: 'reject-perform', targetId: m.id, reason: '   ' })
    assert.ok(await m.waitFor(x => x.type === 'perform-request-rejected' && x.reason === '방장이 요청을 거절했습니다.'))
  })

  it('sends request-perform-error to anonymous requesters', async () => {
    ROOMS.approval2 = { hostId: 'host', freeJoin: false }
    const url = await startWs(backend())
    const anon = await client(url)
    await authJoin(anon, null, 'approval2')
    anon.send({ type: 'request-perform', instrument: 'guitar' })
    assert.ok(await anon.waitFor(x => x.type === 'request-perform-error' && x.code === 'login-required'))
  })
})

// ---------------------------------------------------------------- HTTP

class MemoryRoomRepository implements RoomRepository {
  rooms = new Map<string, RoomRow>()
  secrets = new Map<string, string>()
  failSecretWrite = false
  private seq = 0

  async findMeta(roomId: string): Promise<RoomMeta | null> {
    const r = this.rooms.get(roomId)
    if (!r) return null
    return { hostId: r.host_id, freeJoin: r.free_join !== false, isPrivate: r.is_private === true, hasPassword: r.has_password === true, passwordHash: this.secrets.get(roomId) ?? null }
  }
  async updateParticipantCount() {}
  async resetAllParticipantCounts() {}
  async create(room: NewRoom, opts: { hasPassword?: boolean } = {}): Promise<RoomRow> {
    const id = `00000000-0000-4000-8000-${String(++this.seq).padStart(12, '0')}`
    const row: RoomRow = {
      id, host_id: room.hostId, title: room.title, description: room.description, genre: room.genre,
      max_participants: room.maxParticipants, tags: room.tags, free_join: room.freeJoin, is_private: room.isPrivate,
      instrument_slots: room.instrumentSlots, has_password: opts.hasPassword === true, status: 'open',
    }
    this.rooms.set(id, row)
    return row
  }
  async remove(roomId: string) {
    this.rooms.delete(roomId)
    this.secrets.delete(roomId)
  }
  async setPasswordHash(roomId: string, hash: string | null) {
    if (this.failSecretWrite) throw new Error('db down')
    if (hash) this.secrets.set(roomId, hash)
    else this.secrets.delete(roomId)
    const r = this.rooms.get(roomId)
    if (r) r.has_password = !!hash
  }
}

describe('room routes', () => {
  let server: Server
  let url = ''
  const repo = new MemoryRoomRepository()

  before(async () => {
    const auth = await import('../src/lib/auth')
    auth.setTokenVerifier(testVerifier)
    const { createApp } = await import('../src/app')
    server = createApp({ roomService: createRoomService(repo) }).listen(0)
    await new Promise(r => server.once('listening', r))
    url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
  })
  after(() => server?.close())

  const req = (method: string, path: string, body: unknown, headers: Record<string, string> = {}) =>
    fetch(`${url}${path}`, { method, headers: { 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) })

  it('requires auth', async () => {
    assert.equal((await req('POST', '/api/rooms', { title: 'x' })).status, 401)
    assert.equal((await req('PUT', '/api/rooms/00000000-0000-4000-8000-000000000001/password', { password: null })).status, 401)
  })

  it('creates a private room with a hashed password; host is the token user', async () => {
    const r = await req('POST', '/api/rooms', {
      title: '  비밀 합주  ', isPrivate: true, password: 'letmein', host_id: 'victim', hostId: 'victim',
      instrumentSlots: [{ instrument: 'guitar', count: 2 }, { instrument: 'bass', count: 0 }],
    }, bearer('alice'))
    assert.equal(r.status, 201)
    const { room } = await r.json() as { room: RoomRow }
    assert.equal(room.host_id, 'alice')
    assert.equal(room.title, '비밀 합주')
    assert.equal(room.is_private, true)
    assert.equal(room.has_password, true)
    assert.deepEqual(room.instrument_slots, [{ instrument: 'guitar', count: 2 }])
    assert.ok(!JSON.stringify(room).includes('letmein'), 'response must not echo the password')
    assert.ok(!('password_hash' in room))
    const hash = repo.secrets.get(room.id)!
    assert.ok(hash && !hash.includes('letmein'))
    assert.equal(await verifyRoomPassword('letmein', hash), true)
  })

  it('creates an open room without a secret', async () => {
    const r = await req('POST', '/api/rooms', { title: 'open jam' }, bearer('alice'))
    assert.equal(r.status, 201)
    const { room } = await r.json() as { room: RoomRow }
    assert.equal(room.is_private, false)
    assert.ok(!repo.secrets.has(room.id))
  })

  it('validates input', async () => {
    assert.equal((await req('POST', '/api/rooms', { title: '' }, bearer('alice'))).status, 400)
    assert.equal((await req('POST', '/api/rooms', { title: 'x', password: 'abc' }, bearer('alice'))).status, 400)
    assert.equal((await req('POST', '/api/rooms', { title: 'x', maxParticipants: 999 }, bearer('alice'))).status, 400)
  })

  it('rolls the room back if storing the hash fails', async () => {
    const before = repo.rooms.size
    repo.failSecretWrite = true
    try {
      assert.equal((await req('POST', '/api/rooms', { title: 'x', password: 'abcdef' }, bearer('alice'))).status, 500)
    } finally {
      repo.failSecretWrite = false
    }
    assert.equal(repo.rooms.size, before)
  })

  it('only the host can set or clear the password', async () => {
    const created = await (await req('POST', '/api/rooms', { title: 'pw' }, bearer('alice'))).json() as { room: RoomRow }
    const path = `/api/rooms/${created.room.id}/password`
    assert.equal((await req('PUT', path, { password: 'abcd1234' }, bearer('mallory'))).status, 403)
    assert.ok(!repo.secrets.has(created.room.id))

    const set = await req('PUT', path, { password: 'abcd1234' }, bearer('alice'))
    assert.equal(set.status, 200)
    assert.deepEqual(await set.json(), { success: true, hasPassword: true })
    assert.equal(await verifyRoomPassword('abcd1234', repo.secrets.get(created.room.id)!), true)

    const clear = await req('PUT', path, { password: null }, bearer('alice'))
    assert.deepEqual(await clear.json(), { success: true, hasPassword: false })
    assert.ok(!repo.secrets.has(created.room.id))

    assert.equal((await req('PUT', '/api/rooms/00000000-0000-4000-8000-999999999999/password', { password: null }, bearer('alice'))).status, 404)
    assert.equal((await req('PUT', '/api/rooms/not-a-uuid/password', { password: null }, bearer('alice'))).status, 400)
  })

  it('returns 503 when no backend is configured', async () => {
    const { createApp } = await import('../src/app')
    const s = createApp({ roomService: createRoomService(null) }).listen(0)
    await new Promise(r => s.once('listening', r))
    try {
      const r = await fetch(`http://127.0.0.1:${(s.address() as { port: number }).port}/api/rooms`, {
        method: 'POST', headers: { 'Content-Type': 'application/json', ...bearer('alice') }, body: JSON.stringify({ title: 'x' }),
      })
      assert.equal(r.status, 503)
    } finally {
      s.close()
    }
  })
})
