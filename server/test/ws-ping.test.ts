/**
 * 앱 수준 ping → pong (P1-15): 클라이언트의 수신 없음 감지용. 신원·인증 상태와 무관하게 답한다.
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket } from '../src/services/wsService'
import { authJoin, connect, sleep, testVerifier, type TestClient } from './helpers'

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    deps: {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async () => ({ hostId: 'host', freeJoin: true }),
      updateParticipants: async () => {},
    },
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('ping → pong', async () => {
  const url = await startWs()

  it('인증 전에도 pong으로 답하고 auth-required 오류를 보내지 않는다', async () => {
    const c = await connect(url)
    openClients.push(c)
    c.send({ type: 'ping' })
    assert.ok(await c.waitFor(m => m.type === 'pong'))
    await sleep(50)
    assert.ok(!c.has(m => m.type === 'error'))
  })

  it('방 안의 인증된 연결도 pong을 받고, 다른 참가자에게는 아무것도 가지 않는다', async () => {
    const a = await connect(url)
    const b = await connect(url)
    openClients.push(a, b)
    await authJoin(a, 'tok:a', 'r1')
    await authJoin(b, 'tok:b', 'r1')
    const before = b.inbox.length
    a.send({ type: 'ping' })
    assert.ok(await a.waitFor(m => m.type === 'pong'))
    await sleep(50)
    assert.equal(b.inbox.length, before)
  })
})
