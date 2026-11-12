/**
 * P1-16: 서버가 아직 닫힘을 모르는(반쯤 열린) 옛 연결에서의 세션 재개.
 * 클라이언트는 수신 없음(15초 + ping 5초)으로 먼저 끊김을 알고 새 소켓으로 resumeClientId auth를 보낸다.
 * 서버 하트비트는 그보다 늦게 알기 때문에 옛 Client는 suspended가 아니라 clients에 살아 있다.
 * 같은 JWT userId면 옛 소켓을 끊고 그 Client에 붙인다(같은 clientId, resumed:true, 참가자에게 left/joined 없음).
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import { WebSocket, type WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { authJoin, connect, engineKey, sleep, testVerifier, waitFor, type TestClient } from './helpers'

const ROOMS: Record<string, RoomMeta> = {
  free: { hostId: 'host', freeJoin: true },
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const openClients: TestClient[] = []

async function startWs(limits: WsOptions['limits'] = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits,
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

/** 옛 소켓은 그대로 둔 채(닫지 않음) 새 연결에서 resumeClientId로 auth */
async function resumeLive(url: string, token: string | null, resumeClientId: string) {
  const c = await client(url)
  c.send(token ? { type: 'auth', accessToken: token, resumeClientId } : { type: 'auth', resumeClientId })
  await c.waitFor(m => m.type === 'auth-result')
  return c
}

const leftOrJoined = (id: string) => (m: any) =>
  (m.type === 'participant-left' && m.oderId === id) || (m.type === 'participant-joined' && m.participant?.oderId === id)

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('P1-16: resume while the old connection is still open on the server', async () => {
  // 하트비트를 길게 두어 테스트 동안 서버가 옛 소켓의 끊김을 스스로 감지하지 않게 한다
  const url = await startWs({ resumeGraceMs: 300, heartbeatMs: 60_000 })

  it('takes over the live session: same clientId, resumed:true, old socket closed, no left/joined to peers', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:live-peer', 'free')
    const a = await client(url)
    await authJoin(a, 'tok:live-alice', 'free')
    a.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await peer.waitFor(m => m.type === 'performer-updated' && m.oderId === a.id))
    const oldId = a.id
    const joinedBefore = peer.inbox.filter(leftOrJoined(oldId)).length

    // 옛 소켓을 닫지 않은 채 새 연결로 재개
    const a2 = await resumeLive(url, 'tok:live-alice', oldId)
    assert.ok(a2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    assert.ok(a2.has(m => m.type === 'auth-result' && m.userId === 'live-alice' && m.resumed === true))
    assert.ok(await a2.waitFor(m => m.type === 'room-state' && m.peerIds.includes(peer.id)))

    // 서버가 옛 소켓을 끊었다
    assert.ok(await waitFor(() => a.ws.readyState === WebSocket.CLOSED, 2000), 'old socket closed by the server')

    // 옛 소켓의 close가 유예·퇴장을 일으키지 않는다: 유예시간이 지나도 참가자에게 left/joined 없음
    await sleep(500)
    assert.equal(peer.inbox.filter(leftOrJoined(oldId)).length, joinedBefore, 'no participant-left/joined for the resumed id')

    // 연주 상태 유지: 새 연결이 옛 clientId로 descriptor를 중계한다(descriptor는 연주자끼리만)
    peer.send({ type: 'start-performing', instrument: 'bass' })
    assert.ok(await a2.waitFor(m => m.type === 'performer-updated' && m.oderId === peer.id))
    a2.send({ type: 'native-descriptor', descriptor: { ssrc: 11, key: engineKey(11), candidates: [] } })
    assert.ok(await peer.waitFor(m => m.type === 'native-descriptor' && m.from === oldId))

    // 새로 들어온 사람에게도 옛 id로 연주 중으로 보인다(중복 Client 없음)
    const late = await client(url)
    await authJoin(late, 'tok:live-late', 'free')
    const state = late.inbox.find(m => m.type === 'room-state')
    assert.equal(state.participants.filter((p: any) => p.oderId === oldId).length, 1)
    assert.ok(state.participants.some((p: any) => p.oderId === oldId && p.isPerforming === true))
    assert.ok(!state.participants.some((p: any) => p.oderId === a2.id && a2.id !== oldId))
  })

  it('the resumed connection still suspends normally when it drops later', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:live-peer2', 'free')
    const b = await client(url)
    await authJoin(b, 'tok:live-bob', 'free')
    const oldId = b.id

    const b2 = await resumeLive(url, 'tok:live-bob', oldId)
    assert.ok(b2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    assert.ok(await waitFor(() => b.ws.readyState === WebSocket.CLOSED, 2000))

    // 새 연결이 끊기면 평소처럼 유예 → 재개 가능
    b2.ws.terminate()
    await sleep(50)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
    const b3 = await resumeLive(url, 'tok:live-bob', oldId)
    assert.ok(b3.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    await sleep(400)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
  })

  it('another user token (or no token) cannot take over a live session', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:live-peer3', 'free')
    const c = await client(url)
    await authJoin(c, 'tok:live-carol', 'free')
    const oldId = c.id

    const thief = await resumeLive(url, 'tok:live-mallory', oldId)
    assert.ok(!thief.has(m => m.resumed === true))
    assert.notEqual(thief.id, oldId)
    assert.ok(thief.has(m => m.type === 'auth-result' && m.userId === 'live-mallory'))

    const anon = await resumeLive(url, null, oldId)
    assert.ok(!anon.has(m => m.resumed === true))
    assert.notEqual(anon.id, oldId)

    // 원래 연결은 그대로 살아 있고 방에 남아 있다
    await sleep(100)
    assert.equal(c.ws.readyState, WebSocket.OPEN)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
    c.send({ type: 'chat', message: 'still here' })
    assert.ok(await peer.waitFor(m => m.type === 'chat' && m.from === oldId))
  })
})
