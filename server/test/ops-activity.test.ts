/**
 * P1-17①: 배포 전 연주 인원 확인 — GET /api/ops/activity(운영자 토큰) + scripts/ops-active.ts 판정
 * - 집계 숫자만 돌려주고 방 id·사용자 정보는 없다
 * - OPS_TOKEN 없음/짧음 → 404, 틀린 토큰 → 401, WS 서버 없음 → 503
 * - 실제 WS 서버에 방장(연주자)·관람자를 넣고 숫자가 맞는지, 나가면 0으로 돌아오는지
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { createApp } from '../src/app'
import { initializeWebSocket, wsActivity, type ActivityCounts, type RoomMeta } from '../src/services/wsService'
import { isOpsAuthorized, resolveOpsToken } from '../src/routes/ops'
import { check, evaluate, parseActivity } from '../scripts/ops-active'
import { authJoin, connect, testVerifier, waitFor, type TestClient } from './helpers'

const TOKEN = 'ops-test-token-0123456789abcdef-XYZ' // 32자 이상 (테스트 전용 값)
const ROOMS: Record<string, RoomMeta> = { 'room-secret-id': { hostId: 'host-user', freeJoin: false } }

const servers: Server[] = []
const wsServers: WebSocketServer[] = []
const clients: TestClient[] = []

after(() => {
  for (const c of clients) c.ws.terminate()
  for (const w of wsServers) w.close()
  for (const s of servers) s.close()
})

async function listen(server: Server): Promise<string> {
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push(server)
  return `127.0.0.1:${(server.address() as { port: number }).port}`
}

async function startApp(opsEnv: Record<string, string | undefined>, activity?: () => ActivityCounts | null) {
  const app = createApp({ rateLimits: false, imageStorage: null, legacyImageStorage: null, postRepository: null, opsEnv, activity })
  return `http://${await listen(createServer(app))}`
}

const get = (base: string, auth?: string) =>
  fetch(`${base}/api/ops/activity`, auth ? { headers: { Authorization: auth } } : undefined)

const COUNTS: ActivityCounts = { activeRooms: 2, performers: 3, viewers: 4, resuming: 1, connections: 9 }

describe('GET /api/ops/activity — 운영자 토큰', () => {
  it('OPS_TOKEN이 없거나 32자 미만이면 경로가 없는 것처럼 404', async () => {
    for (const env of [{}, { OPS_TOKEN: '' }, { OPS_TOKEN: 'short-token' }]) {
      const base = await startApp(env, () => COUNTS)
      const res = await get(base, 'Bearer short-token')
      assert.equal(res.status, 404)
    }
    assert.equal(resolveOpsToken({ OPS_TOKEN: 'x'.repeat(31) }), null)
    assert.equal(resolveOpsToken({ OPS_TOKEN: ` ${'x'.repeat(32)} ` }), 'x'.repeat(32))
  })

  it('토큰 없음·틀림·형식 다름 → 401, 숫자를 내주지 않는다', async () => {
    const base = await startApp({ OPS_TOKEN: TOKEN }, () => COUNTS)
    for (const auth of [undefined, 'Bearer wrong', `Bearer ${TOKEN}x`, TOKEN, `Basic ${TOKEN}`, 'Bearer ']) {
      const res = await get(base, auth)
      assert.equal(res.status, 401, `auth=${auth}`)
      const body = await res.json() as Record<string, unknown>
      assert.equal(body.performers, undefined)
    }
  })

  it('맞는 토큰 → 숫자만, no-store, safeToDeploy = 연주자 0', async () => {
    const base = await startApp({ OPS_TOKEN: TOKEN }, () => COUNTS)
    const res = await get(base, `Bearer ${TOKEN}`)
    assert.equal(res.status, 200)
    assert.equal(res.headers.get('cache-control'), 'no-store')
    const body = await res.json() as Record<string, unknown>
    assert.deepEqual(Object.keys(body).sort(), ['activeRooms', 'connections', 'performers', 'resuming', 'safeToDeploy', 'timestamp', 'viewers'])
    assert.equal(body.performers, 3)
    assert.equal(body.safeToDeploy, false)

    const idle = await startApp({ OPS_TOKEN: TOKEN }, () => ({ ...COUNTS, performers: 0 }))
    assert.equal(((await (await get(idle, `bearer ${TOKEN}`)).json()) as { safeToDeploy: boolean }).safeToDeploy, true)
  })

  it('WS 서버가 아직 없으면 503', async () => {
    const base = await startApp({ OPS_TOKEN: TOKEN })
    assert.equal((await get(base, `Bearer ${TOKEN}`)).status, 503)
  })

  it('isOpsAuthorized: Bearer 접두사 필수, 값 일치만', () => {
    assert.equal(isOpsAuthorized(`Bearer ${TOKEN}`, TOKEN), true)
    assert.equal(isOpsAuthorized(`Bearer  ${TOKEN} `, TOKEN), true)
    assert.equal(isOpsAuthorized(undefined, TOKEN), false)
    assert.equal(isOpsAuthorized(TOKEN, TOKEN), false)
    assert.equal(isOpsAuthorized(`Bearer ${TOKEN.slice(1)}`, TOKEN), false)
  })
})

describe('실제 WS 서버 집계', () => {
  it('방장(연주자)·관람자 수, 방 id·사용자 정보 없음, 나가면 0', async () => {
    const app = createApp({ rateLimits: false, imageStorage: null, legacyImageStorage: null, postRepository: null, opsEnv: { OPS_TOKEN: TOKEN } })
    const server = createServer(app)
    const wss = initializeWebSocket(server, null, {
      env: { NODE_ENV: 'test' },
      limits: { resumeGraceMs: 300 },
      deps: {
        hasBackend: true,
        verifyToken: testVerifier,
        fetchRoomMeta: async roomId => ROOMS[roomId] ?? null,
        updateParticipants: async () => {},
      },
    })
    wsServers.push(wss)
    const addr = await listen(server)
    // 앱은 index.ts처럼 WS 서버를 만든 뒤 붙인다: 여기서는 같은 서버의 wsActivity를 읽는 두 번째 앱으로 HTTP 경로까지 확인
    const base = await startApp({ OPS_TOKEN: TOKEN }, () => wsActivity(wss))

    assert.deepEqual(wsActivity(wss), { activeRooms: 0, performers: 0, viewers: 0, resuming: 0, connections: 0 })

    const host = await connect(`ws://${addr}`)
    const viewer = await connect(`ws://${addr}`)
    const lobby = await connect(`ws://${addr}`)
    clients.push(host, viewer, lobby)
    await authJoin(host, 'tok:host-user', 'room-secret-id')
    await authJoin(viewer, 'tok:viewer-user', 'room-secret-id')

    assert.deepEqual(wsActivity(wss), { activeRooms: 1, performers: 1, viewers: 1, resuming: 0, connections: 3 })

    const res = await get(base, `Bearer ${TOKEN}`)
    const text = await res.text()
    assert.equal(res.status, 200)
    for (const secret of ['room-secret-id', 'host-user', 'viewer-user', host.id, viewer.id]) {
      assert.ok(!text.includes(secret), `응답에 ${secret}이 있으면 안 된다`)
    }
    assert.equal((JSON.parse(text) as { safeToDeploy: boolean }).safeToDeploy, false)

    // 관람자 연결이 끊기면 재개 유예 동안 방에 남는다(배포하면 이 사람도 끊긴다) → resuming
    viewer.ws.terminate()
    assert.ok(await waitFor(() => wsActivity(wss)?.resuming === 1), '끊긴 관람자는 재개 대기')
    assert.deepEqual(wsActivity(wss), { activeRooms: 1, performers: 1, viewers: 1, resuming: 1, connections: 3 })

    host.send({ type: 'leave' })
    assert.ok(await waitFor(() => wsActivity(wss)?.activeRooms === 0), 'leave·유예 만료 뒤 활성 방 0')
    assert.deepEqual(wsActivity(wss), { activeRooms: 0, performers: 0, viewers: 0, resuming: 0, connections: 2 })
  })
})

describe('scripts/ops-active.ts 판정', () => {
  const a = (performers: number, viewers: number) => ({ activeRooms: performers + viewers > 0 ? 1 : 0, performers, viewers, resuming: 0, connections: performers + viewers })

  it('연주자가 있으면 1(배포 금지), 관람자만이면 0(경고), --strict면 관람자도 1', () => {
    assert.equal(evaluate(a(1, 0)).code, 1)
    assert.equal(evaluate(a(0, 2)).code, 0)
    assert.match(evaluate(a(0, 2)).lines.join('\n'), /관람자 2명/)
    assert.equal(evaluate(a(0, 2), true).code, 1)
    assert.equal(evaluate(a(0, 0), true).code, 0)
  })

  it('parseActivity: 숫자 다섯 개가 모두 0 이상 정수일 때만', () => {
    assert.ok(parseActivity({ ...COUNTS, safeToDeploy: true }))
    assert.equal(parseActivity({ ...COUNTS, performers: -1 }), null)
    assert.equal(parseActivity({ ...COUNTS, viewers: '1' }), null)
    assert.equal(parseActivity(null), null)
  })

  it('check: 환경변수 없음·401·404·연결 실패·이상한 응답은 모두 2(확인 실패 = 배포 금지)', async () => {
    const resp = (status: number, body: unknown) => async () => ({ status, json: async () => body })
    assert.equal((await check({}, [], resp(200, COUNTS))).code, 2)
    const env = { OPS_URL: 'https://example.invalid/', OPS_TOKEN: TOKEN }
    assert.equal((await check(env, [], resp(401, {}))).code, 2)
    assert.equal((await check(env, [], resp(404, {}))).code, 2)
    assert.equal((await check(env, [], resp(500, {}))).code, 2)
    assert.equal((await check(env, [], resp(200, { ok: true }))).code, 2)
    assert.equal((await check(env, [], async () => { throw new Error('ECONNREFUSED') })).code, 2)
    assert.equal((await check(env, [], resp(200, COUNTS))).code, 1)
    assert.equal((await check(env, [], resp(200, { ...COUNTS, performers: 0 }))).code, 0)
  })

  it('check: 끝 슬래시를 떼고 Bearer 토큰으로 부르며, 출력에 토큰이 없다', async () => {
    let seen: { url: string; auth: string } | null = null
    const v = await check({ OPS_URL: 'https://ops.example.invalid//', OPS_TOKEN: TOKEN }, [], async (url, init) => {
      seen = { url, auth: init.headers.Authorization }
      return { status: 200, json: async () => ({ ...COUNTS, performers: 0, viewers: 0 }) }
    })
    assert.deepEqual(seen, { url: 'https://ops.example.invalid/api/ops/activity', auth: `Bearer ${TOKEN}` })
    assert.equal(v.code, 0)
    assert.ok(!v.lines.join('\n').includes(TOKEN))
  })

  it('실제 HTTP 왕복: 서버 경로 + 스크립트', async () => {
    const base = await startApp({ OPS_TOKEN: TOKEN }, () => COUNTS)
    assert.equal((await check({ OPS_URL: base, OPS_TOKEN: TOKEN }, [])).code, 1)
    assert.equal((await check({ OPS_URL: base, OPS_TOKEN: 'wrong-but-long-enough-token-000000000' }, [])).code, 2)
  })
})
