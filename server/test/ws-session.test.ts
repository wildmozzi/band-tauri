/**
 * WS 회귀 테스트: 연주자 최대 6명 제한, 세션 재개(resumeClientId)
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta, type WsOptions } from '../src/services/wsService'
import { authJoin, connect, engineKey, sleep, testVerifier, type TestClient } from './helpers'

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

/** 새 연결에서 resumeClientId로 auth */
async function resume(url: string, token: string | null, resumeClientId: string) {
  const c = await client(url)
  c.send(token ? { type: 'auth', accessToken: token, resumeClientId } : { type: 'auth', resumeClientId })
  await c.waitFor(m => m.type === 'auth-result')
  return c
}

async function terminate(c: TestClient) {
  c.ws.terminate()
  await sleep(50)
}

after(async () => {
  for (const c of openClients) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

describe('performer limit (max 6)', async () => {
  const url = await startWs()

  it('rejects the 7th performer with room-full-performers', async () => {
    const players: TestClient[] = []
    for (let i = 0; i < 7; i++) {
      const c = await client(url)
      await authJoin(c, `tok:p${i}`, 'free')
      players.push(c)
    }
    for (let i = 0; i < 6; i++) {
      players[i].send({ type: 'start-performing', instrument: 'guitar' })
      assert.ok(await players[6].waitFor(m => m.type === 'performer-updated' && m.oderId === players[i].id && m.isPerforming === true))
    }

    const seventh = players[6]
    seventh.send({ type: 'start-performing', instrument: 'bass' })
    assert.ok(await seventh.waitFor(m => m.type === 'error' && m.code === 'room-full-performers'))
    await sleep(50)
    assert.ok(!players[0].has(m => m.type === 'performer-updated' && m.oderId === seventh.id))

    // 이미 연주 중인 사람의 재전송(악기 갱신)은 허용
    players[0].send({ type: 'start-performing', instrument: 'keys' })
    assert.ok(await seventh.waitFor(m => m.type === 'performer-updated' && m.oderId === players[0].id && m.instrument === 'keys'))
    assert.ok(!players[0].has(m => m.code === 'room-full-performers'))

    // 한 명이 내려오면 7번째가 올라갈 수 있다
    players[1].send({ type: 'stop-performing' })
    await players[0].waitFor(m => m.type === 'performer-updated' && m.oderId === players[1].id && m.isPerforming === false)
    seventh.send({ type: 'start-performing', instrument: 'bass' })
    assert.ok(await players[0].waitFor(m => m.type === 'performer-updated' && m.oderId === seventh.id && m.isPerforming === true))
  })
})

describe('session resume', async () => {
  const url = await startWs({ resumeGraceMs: 400 })

  it('keeps clientId, room and performing state when the same user reconnects within the grace period', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:peer1', 'free')
    const a = await client(url)
    await authJoin(a, 'tok:alice', 'free')
    a.send({ type: 'start-performing', instrument: 'drums' })
    assert.ok(await peer.waitFor(m => m.type === 'performer-updated' && m.oderId === a.id))
    const oldId = a.id

    await terminate(a)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId), 'no leave during grace period')

    const a2 = await resume(url, 'tok:alice', oldId)
    assert.ok(a2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    assert.ok(await a2.waitFor(m => m.type === 'room-state' && m.peerIds.includes(peer.id)))

    // 클라이언트의 rejoin은 브로드캐스트 없이 상태만 다시 받는다
    a2.send({ type: 'join', roomId: 'free', isRejoin: true })
    assert.ok(await a2.waitFor(m => m.type === 'room-state' && a2.inbox.filter(x => x.type === 'room-state').length >= 2))

    // 연주 상태 유지: 연주자 전용 메시지가 이전 id로 중계된다
    peer.send({ type: 'start-performing', instrument: 'bass' })
    await a2.waitFor(m => m.type === 'performer-updated' && m.oderId === peer.id)
    a2.send({ type: 'native-descriptor', descriptor: { ssrc: 7, key: engineKey(7), candidates: [] } })
    assert.ok(await peer.waitFor(m => m.type === 'native-descriptor' && m.from === oldId))

    // 새로 들어온 사람에게도 이전 id로 연주 중으로 보인다
    const late = await client(url)
    await authJoin(late, 'tok:late', 'free')
    const state = late.inbox.find(m => m.type === 'room-state')
    assert.ok(state.participants.some((p: any) => p.oderId === oldId && p.isPerforming === true))

    await sleep(500) // 유예시간이 지나도 재개된 세션은 유지
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
    // 최초 입장 때의 1회만 (재개/rejoin으로 추가 joined 없음)
    assert.equal(peer.inbox.filter(m => m.type === 'participant-joined' && m.participant.oderId === oldId).length, 1)
  })

  it('sends leave after the grace period and no longer resumes', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:peer2', 'free')
    const b = await client(url)
    await authJoin(b, 'tok:bob', 'free')
    const oldId = b.id
    await terminate(b)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
    assert.ok(await peer.waitFor(m => m.type === 'participant-left' && m.oderId === oldId, 1500))

    const b2 = await resume(url, 'tok:bob', oldId)
    assert.ok(!b2.has(m => m.resumed === true))
    assert.notEqual(b2.id, oldId)
  })

  it('does not let a different user take over the session', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:peer3', 'free')
    const c = await client(url)
    await authJoin(c, 'tok:carol', 'free')
    const oldId = c.id
    await terminate(c)

    const thief = await resume(url, 'tok:mallory', oldId)
    assert.ok(!thief.has(m => m.resumed === true))
    assert.ok(thief.has(m => m.type === 'auth-result' && m.userId === 'mallory'))

    const anon = await resume(url, null, oldId)
    assert.ok(!anon.has(m => m.resumed === true))

    // 원래 사용자는 여전히 재개 가능
    const c2 = await resume(url, 'tok:carol', oldId)
    assert.ok(c2.has(m => m.type === 'welcome' && m.clientId === oldId && m.resumed === true))
    await sleep(500)
    assert.ok(!peer.has(m => m.type === 'participant-left' && m.oderId === oldId))
  })

  it('anonymous viewers are not suspended (leave is immediate)', async () => {
    const peer = await client(url)
    await authJoin(peer, 'tok:peer4', 'free')
    const anon = await client(url)
    await authJoin(anon, null, 'free')
    const oldId = anon.id
    anon.ws.terminate()
    assert.ok(await peer.waitFor(m => m.type === 'participant-left' && m.oderId === oldId, 300))
  })
})

describe('H1: rejoin with a new clientId is announced', async () => {
  const url = await startWs({ resumeGraceMs: 100 })

  it('sends participant-joined (rejoin:true) when a user rejoins with a new id after the grace period', async () => {
    const performer = await client(url)
    await authJoin(performer, 'tok:h1perf', 'free')
    performer.send({ type: 'start-performing', instrument: 'guitar' })
    const viewer = await client(url)
    await authJoin(viewer, 'tok:h1viewer', 'free')
    const oldId = viewer.id

    await terminate(viewer)
    assert.ok(await performer.waitFor(m => m.type === 'participant-left' && m.oderId === oldId, 1500))

    // 유예 만료 후 재접속: 새 id, 클라이언트는 isRejoin으로 join
    const again = await client(url)
    await authJoin(again, 'tok:h1viewer', 'free', { isRejoin: true })
    assert.notEqual(again.id, oldId)
    assert.ok(await performer.waitFor(m => m.type === 'participant-joined' && m.participant.oderId === again.id && m.rejoin === true),
      'performer must learn the new viewer id so the broadcaster can offer to it')
  })

  it('does not re-announce a resumed session (same id) on rejoin', async () => {
    const watcher = await client(url)
    await authJoin(watcher, 'tok:h1w', 'free')
    const a = await client(url)
    await authJoin(a, 'tok:h1a', 'free')
    const oldId = a.id
    await terminate(a)
    const a2 = await resume(url, 'tok:h1a', oldId)
    a2.send({ type: 'join', roomId: 'free', isRejoin: true })
    await a2.waitFor(m => m.type === 'room-state' && a2.inbox.filter(x => x.type === 'room-state').length >= 2)
    await sleep(50)
    assert.equal(watcher.inbox.filter(m => m.type === 'participant-joined' && m.participant.oderId === oldId).length, 1)
  })

  it('a first join is announced with rejoin:false', async () => {
    const watcher = await client(url)
    await authJoin(watcher, 'tok:h1x', 'free')
    const b = await client(url)
    await authJoin(b, 'tok:h1b', 'free')
    assert.ok(await watcher.waitFor(m => m.type === 'participant-joined' && m.participant.oderId === b.id && m.rejoin === false))
  })
})
