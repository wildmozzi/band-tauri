/**
 * 시그널링 모듈 단위 테스트 — docs/server-architecture.md 2.5절, S2.
 *
 * 소켓 없이 S2에서 꺼낸 createX 팩토리만 부른다. 통합 테스트(ws*.test.ts, room-*.test.ts)를 대체하지 않는다:
 * 이 파일은 꺼낸 함수가 캡처하던 상태를 인자로 받아도 같은 계산을 하는지만 본다.
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import type { WebSocket } from 'ws'
import type { RoomMeta } from '../src/repositories/roomRepository'
import type { Client, WsDeps } from '../src/signaling/types'
import { TokenBucket } from '../src/signaling/rateLimit'
import { createRegistry, isAuthenticated, isPerformer, send } from '../src/signaling/registry'
import { createRoomMeta } from '../src/signaling/roomMeta'
import { createRequests } from '../src/signaling/requests'
import { sleep } from './helpers'

const OPEN = 1
const CLOSED = 3

interface FakeWs { readyState: number; OPEN: number; sent: any[]; send: (s: string) => void }

function fakeWs(readyState = OPEN): FakeWs {
  const ws: FakeWs = { readyState, OPEN, sent: [], send: (s: string) => { ws.sent.push(JSON.parse(s)) } }
  return ws
}

function fakeClient(id: string, over: Partial<Client> = {}): Client & { ws: WebSocket & FakeWs } {
  return {
    id,
    ws: fakeWs() as unknown as WebSocket & FakeWs,
    ip: '10.0.0.1',
    nickname: `N-${id}`,
    isPerforming: false,
    isHost: false,
    authState: 'authenticated',
    tokenExp: null,
    nativeActive: false,
    chain: Promise.resolve(),
    pending: 0,
    isAlive: true,
    msgBucket: new TokenBucket(1, 1),
    chatBucket: new TokenBucket(1, 1),
    joinBucket: new TokenBucket(1, 1),
    dropped: 0,
    lastRateLimitNotice: 0,
    ...over,
  } as Client & { ws: WebSocket & FakeWs }
}

const sentTo = (c: Client) => (c.ws as unknown as FakeWs).sent

/** 같은 방(roomId)에 클라이언트들을 넣는다 */
function seat(reg: ReturnType<typeof createRegistry>, roomId: string, ...cs: Client[]) {
  for (const c of cs) {
    c.roomId = roomId
    reg.addClient(c)
    reg.addToRoom(roomId, c.id)
  }
}

describe('registry', () => {
  it('send skips sockets that are not open', () => {
    const open = fakeWs()
    const closed = fakeWs(CLOSED)
    send(open as unknown as WebSocket, { type: 'x' })
    send(closed as unknown as WebSocket, { type: 'x' })
    assert.deepEqual(open.sent, [{ type: 'x' }])
    assert.deepEqual(closed.sent, [])
  })

  it('isAuthenticated needs both the state and a verified userId; isPerformer = performing or host', () => {
    assert.equal(isAuthenticated(fakeClient('a', { userId: 'u' })), true)
    assert.equal(isAuthenticated(fakeClient('a')), false, 'insecure dev mode: authenticated without userId')
    assert.equal(isAuthenticated(fakeClient('a', { authState: 'anonymous', userId: 'u' })), false)
    assert.equal(isPerformer(fakeClient('a')), false)
    assert.equal(isPerformer(fakeClient('a', { isPerforming: true })), true)
    assert.equal(isPerformer(fakeClient('a', { isHost: true })), true)
  })

  it('add/remove keep clients and room members', () => {
    const reg = createRegistry()
    const a = fakeClient('a')
    seat(reg, 'r', a)
    assert.equal(reg.clients.get('a'), a)
    assert.deepEqual([...reg.rooms.get('r')!], ['a'])
    reg.removeFromRoom('r', 'a')
    assert.deepEqual([...reg.rooms.get('r')!], [], 'the empty room entry stays until leave cleanup deletes it')
    reg.removeFromRoom('missing', 'a')
    reg.removeClient('a')
    assert.equal(reg.clients.has('a'), false)
  })

  it('broadcastToRoom skips the sender and stamps oderId/from', () => {
    const reg = createRegistry()
    const a = fakeClient('a')
    const b = fakeClient('b')
    const outside = fakeClient('z')
    seat(reg, 'r', a, b)
    seat(reg, 'other', outside)
    reg.broadcastToRoom('r', 'a', { type: 'chat', from: 'forged', message: 'hi' })
    assert.deepEqual(sentTo(a), [])
    assert.deepEqual(sentTo(b), [{ type: 'chat', from: 'a', oderId: 'a', message: 'hi' }])
    assert.deepEqual(sentTo(outside), [])
  })

  it('getRoomPeer only returns another member of the sender room', () => {
    const reg = createRegistry()
    const a = fakeClient('a')
    const b = fakeClient('b')
    const z = fakeClient('z')
    seat(reg, 'r', a, b)
    seat(reg, 'other', z)
    assert.equal(reg.getRoomPeer(a, 'b'), b)
    assert.equal(reg.getRoomPeer(a, 'a'), null, 'self')
    assert.equal(reg.getRoomPeer(a, 'z'), null, 'other room')
    assert.equal(reg.getRoomPeer(a, 42), null, 'non-string target')
    assert.equal(reg.getRoomPeer(fakeClient('n'), 'b'), null, 'sender outside any room')
  })

  it('participants and findRoomHost read the room members', () => {
    const reg = createRegistry()
    const host = fakeClient('h', { isHost: true, instrument: 'drums', isPerforming: true })
    const viewer = fakeClient('v')
    seat(reg, 'r', viewer, host)
    assert.equal(reg.findRoomHost('r'), host)
    assert.equal(reg.findRoomHost('none'), null)
    assert.deepEqual(reg.getRoomParticipants('r'), [
      { oderId: 'v', nickname: 'N-v', instrument: undefined, isPerforming: false, isHost: false },
      { oderId: 'h', nickname: 'N-h', instrument: 'drums', isPerforming: true, isHost: true },
    ])
    assert.deepEqual(reg.getRoomParticipants('none'), [])
  })
})

describe('roomMeta', () => {
  const META: RoomMeta = { hostId: 'host', freeJoin: false }

  function deps(fetchRoomMeta: WsDeps['fetchRoomMeta']): WsDeps {
    return { hasBackend: true, verifyToken: async () => null, fetchRoomMeta, updateParticipants: async () => {} }
  }

  it('caches by age, joins an in-flight lookup, and maxAgeMs 0 always reads again', async () => {
    let calls = 0
    let release!: () => void
    const gate = new Promise<void>(r => { release = r })
    const meta = createRoomMeta(deps(async () => { calls++; await gate; return META }), false)
    const p1 = meta.getRoomMeta('r')
    const p2 = meta.getRoomMeta('r')
    release()
    assert.deepEqual(await Promise.all([p1, p2]), [META, META])
    assert.equal(calls, 1, 'second caller waits for the in-flight lookup')
    await meta.getRoomMeta('r')
    assert.equal(calls, 1, 'cached')
    await sleep(5) // 같은 ms면 나이 0도 캐시 적중이다(기존 동작)
    await meta.getRoomMeta('r', 0)
    assert.equal(calls, 2, 'join (maxAgeMs 0) ignores the cache')
  })

  it('does not cache a failed lookup; getRoomMetaOrNull turns it into null', async () => {
    let fail = true
    let calls = 0
    const meta = createRoomMeta(deps(async () => { calls++; if (fail) throw new Error('db down'); return META }), false)
    await assert.rejects(meta.getRoomMeta('r'), /db down/)
    assert.equal(await meta.getRoomMetaOrNull('r'), null)
    fail = false
    assert.deepEqual(await meta.getRoomMeta('r'), META)
    assert.equal(calls, 3)
  })

  it('checkHost uses the DB host, falls back to the last read value when the DB fails (stale)', async () => {
    let fail = false
    const meta = createRoomMeta(deps(async () => { if (fail) throw new Error('db down'); return META }), false)
    const host = fakeClient('h', { userId: 'host', roomId: 'r' })
    const other = fakeClient('o', { userId: 'other', roomId: 'r', isHost: true })
    assert.deepEqual(await meta.checkHost(host), { isHost: true, hostUserId: 'host', stale: false })
    assert.equal(await meta.refreshHost(other), false, 'client-held isHost is overwritten by the DB')
    assert.equal(other.isHost, false)
    fail = true
    await sleep(5)
    assert.deepEqual(await meta.checkHost(host, 0), { isHost: true, hostUserId: 'host', stale: true })
    meta.forget('r')
    assert.deepEqual(await meta.checkHost(host, 0), { isHost: false, hostUserId: null, stale: true }, 'never read: no permission')
  })

  it('checkHost keeps the join-time value outside a room or in insecure dev mode', async () => {
    const meta = createRoomMeta(deps(async () => { throw new Error('must not be called') }), true)
    const c = fakeClient('c', { roomId: 'r', isHost: true })
    assert.deepEqual(await meta.checkHost(c), { isHost: true, hostUserId: null, stale: true })
    const secure = createRoomMeta(deps(async () => { throw new Error('must not be called') }), false)
    assert.deepEqual(await secure.checkHost(fakeClient('n', { isHost: true })), { isHost: true, hostUserId: null, stale: true })
  })

  it('forget drops the cache so the next read hits the DB', async () => {
    let calls = 0
    const meta = createRoomMeta(deps(async () => { calls++; return META }), false)
    await meta.getRoomMeta('r')
    meta.forget('r')
    await meta.getRoomMeta('r')
    assert.equal(calls, 2)
  })
})

describe('requests', () => {
  it('each factory owns its own request maps', () => {
    const a = createRequests()
    const b = createRequests()
    a.performRequests.set('r', [{ oderId: 'x', nickname: 'n', instrument: 'bass', timestamp: 0 }])
    assert.equal(b.performRequests.has('r'), false)
    assert.equal(a.instrumentChangeRequests.size, 0)
  })
})
