/**
 * 시그널링 상태 기계 경합 (시그널링 상태 점검 fix/d-signaling-state, #199).
 * - free_join 방에서 참여 요청을 보낸 뒤 승인 없이 바로 연주를 시작하면, 대기 중인 참여 요청을 지우고 방장에게 알린다
 * - start-performing이 방 정보 조회(await)를 기다리는 사이 방장이 내리면, 그 시작을 취소한다
 *   (전에는 방장은 demote-not-performer를 받고 대상은 곧이어 연주를 시작했다)
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket } from '../src/services/wsService'
import { ROOM_META_TTL_MS } from '../src/signaling/protocol'
import { connect, testVerifier, waitFor, type TestClient } from './helpers'
import { PROBE_ROOM, settle } from './wsSettle'

const servers: { server: Server; wss: WebSocketServer }[] = []
const opened: TestClient[] = []

after(() => {
  for (const c of opened) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

/** 방 정보 조회를 하나씩 붙잡을 수 있는 서버 (free_join 방, 방장 'host') */
async function startWs() {
  const gate = { armed: false, waiting: [] as (() => void)[] }
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits: { msgBurst: 1000, msgPerSec: 1000, joinBurst: 1000, joinPerSec: 1000, authIpBurst: 1000, authIpPerSec: 1000 },
    deps: {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async (roomId) => {
        if (roomId === PROBE_ROOM) return null
        if (gate.armed) {
          gate.armed = false // 다음 조회 하나만 붙잡는다
          await new Promise<void>(r => gate.waiting.push(r))
        }
        return { hostId: 'host', freeJoin: true }
      },
      updateParticipants: async () => {},
    },
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return { url: `ws://127.0.0.1:${(server.address() as { port: number }).port}`, gate }
}

let roomSeq = 0
async function enter(url: string, token: string, roomId: string) {
  const c = await connect(url)
  opened.push(c)
  c.send({ type: 'auth', accessToken: token })
  c.send({ type: 'join', roomId })
  assert.ok(await c.waitFor(m => m.type === 'room-state'))
  return c
}

describe('free_join: 참여 요청 뒤 바로 연주 시작', () => {
  it('연주를 시작하면 대기 중인 내 참여 요청을 지우고 방장에게 perform-request-cancelled를 보낸다', async () => {
    const { url } = await startWs()
    const roomId = `free-${++roomSeq}`
    const host = await enter(url, 'tok:host', roomId)
    const user = await enter(url, 'tok:user', roomId)
    user.send({ type: 'request-perform', instrument: 'guitar' })
    assert.ok(await host.waitFor(m => m.type === 'perform-request-received' && m.request.oderId === user.id))

    user.send({ type: 'start-performing', instrument: 'guitar' })
    assert.ok(await host.waitFor(m => m.type === 'performer-updated' && m.oderId === user.id && m.isPerforming === true))
    await settle(user)
    assert.ok(host.has(m => m.type === 'perform-request-cancelled' && m.oderId === user.id), '방장 화면에서 요청이 지워진다')

    // 늦게 들어온 방장도 지운 요청을 받지 않는다
    const lateHost = await enter(url, 'tok:host', roomId)
    assert.ok(!lateHost.has(m => m.type === 'perform-request-received'))
  })
})

describe('start-performing의 방 정보 조회 중 내리기 (경합)', () => {
  it('조회를 기다리는 동안 방장이 내리면 그 시작은 취소되고 대상은 demoted-to-viewer를 받는다', async () => {
    const { url, gate } = await startWs()
    const roomId = `free-${++roomSeq}`
    const host = await enter(url, 'tok:host', roomId)
    const user = await enter(url, 'tok:user', roomId)
    const realNow = Date.now
    try {
      // 방 정보 캐시(30초)가 지난 시점: start-performing은 DB를 다시 읽는다 → 그 조회를 붙잡는다
      const skew = ROOM_META_TTL_MS + 1000
      Date.now = () => realNow() + skew
      gate.armed = true
      user.send({ type: 'start-performing', instrument: 'guitar' })
      assert.ok(await waitFor(() => gate.waiting.length === 1))

      // 다른 사람의 입장(캐시 없이 새로 읽음)이 캐시를 새로 채운다 → 방장 확인은 붙잡힌 조회를 기다리지 않는다
      await enter(url, 'tok:third', roomId)
      host.send({ type: 'demote-performer', targetId: user.id })
      await settle(host)
      assert.ok(!host.has(m => m.type === 'error' && m.code === 'demote-not-performer'), '시작 중인 대상도 내릴 수 있다')
      assert.ok(await user.waitFor(m => m.type === 'demoted-to-viewer'))

      // 붙잡았던 조회가 끝나도 연주는 시작되지 않는다
      gate.waiting.shift()!()
      await settle(user)
      await settle(host)
      assert.ok(!host.has(m => m.type === 'performer-updated' && m.oderId === user.id && m.isPerforming === true))
      const late = await enter(url, 'tok:late', roomId)
      assert.equal(late.inbox.find(m => m.type === 'room-state').participants.find((p: any) => p.oderId === user.id).isPerforming, false)

      // 내린 뒤 다시 시작하면(free_join) 평소처럼 연주자가 된다
      user.send({ type: 'start-performing', instrument: 'guitar' })
      assert.ok(await host.waitFor(m => m.type === 'performer-updated' && m.oderId === user.id && m.isPerforming === true))
    } finally {
      Date.now = realNow
    }
  })
})
