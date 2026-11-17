/**
 * auth 증폭 방지 (서버 보안 점검): auth마다 Supabase getUser(네트워크 왕복)를 부르므로
 * - 연결별 auth 버킷(authBurst/authPerSec)으로 한 연결이 부를 수 있는 횟수를 묶고
 * - 같은 토큰은 짧게 캐시해(cachedTokenVerifier) 여러 연결·반복 auth가 getUser 한 번으로 끝나게 한다.
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { createServer, type Server } from 'http'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, type WsOptions } from '../src/services/wsService'
import { cachedTokenVerifier } from '../src/signaling/auth'
import { DEFAULT_LIMITS } from '../src/signaling/types'
import { connect, sleep, testVerifier, waitFor, type TestClient } from './helpers'

const servers: { server: Server; wss: WebSocketServer }[] = []
const opened: TestClient[] = []

after(() => {
  for (const c of opened) c.ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

async function startWs(verifyToken: (t: string) => Promise<string | null>, limits: WsOptions['limits'] = {}): Promise<string> {
  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test' },
    limits,
    deps: { hasBackend: true, verifyToken, fetchRoomMeta: async () => null, updateParticipants: async () => {} },
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  return `ws://127.0.0.1:${(server.address() as { port: number }).port}`
}

/** JWT 모양 토큰(서명 없음): jwtExpiryMs가 exp를 읽는다 */
const jwt = (sub: string, expSec: number) => `h.${Buffer.from(JSON.stringify({ sub, exp: expSec })).toString('base64url')}.s`

describe('cachedTokenVerifier', () => {
  it('shares one in-flight lookup and reuses a success until the TTL', async () => {
    let calls = 0
    let t = 1_000_000
    const v = cachedTokenVerifier(async (tok) => { calls++; await sleep(5); return testVerifier(tok) }, { ttlMs: 1000, now: () => t })
    const [a, b] = await Promise.all([v('tok:u'), v('tok:u')])
    assert.deepEqual([a, b, calls], ['u', 'u', 1])
    assert.equal(await v('tok:u'), 'u')
    assert.equal(calls, 1, 'cached')
    t += 1001
    assert.equal(await v('tok:u'), 'u')
    assert.equal(calls, 2, 'expired entry is looked up again')
  })

  it('does not cache failures and never past the JWT exp', async () => {
    let calls = 0
    let t = 1_000_000
    const v = cachedTokenVerifier(async (tok) => { calls++; return tok === 'bad' ? null : 'u' }, { ttlMs: 60_000, now: () => t })
    await v('bad')
    await v('bad')
    assert.equal(calls, 2, 'null is not cached')

    const tok = jwt('u', (t + 500) / 1000) // 0.5초 뒤 만료
    await v(tok)
    await v(tok)
    assert.equal(calls, 3)
    t += 600
    await v(tok)
    assert.equal(calls, 4, 'cache entry ends at the token exp')
  })

  it('evicts the oldest entry at the size cap', async () => {
    let calls = 0
    const v = cachedTokenVerifier(async (tok) => { calls++; return tok }, { max: 2 })
    await v('a'); await v('b'); await v('c') // a가 밀려남
    await v('c'); await v('b')
    assert.equal(calls, 3)
    await v('a')
    assert.equal(calls, 4)
  })
})

describe('auth rate limit per connection', () => {
  it('lets only authBurst auth messages reach the verifier; the rest are rate-limited', async () => {
    let calls = 0
    const url = await startWs(async (tok) => { calls++; return testVerifier(tok) }, { msgBurst: 1000, msgPerSec: 1000 })
    const c = await connect(url)
    opened.push(c)
    // 매번 다른 토큰(캐시를 피하는 공격 모양)
    for (let i = 0; i < 12; i++) c.send({ type: 'auth', accessToken: `tok:user${i}` })
    assert.ok(await waitFor(() => c.inbox.filter(m => m.type === 'auth-result').length >= 1))
    await c.waitFor(m => m.type === 'error' && m.code === 'rate-limited' && m.rejectedType === 'auth')
    await sleep(100)
    assert.equal(calls, DEFAULT_LIMITS.authBurst)
    assert.equal(c.closedCode, null, 'a few extra auths do not close the connection')
  })

  it('many connections with the same token cost one lookup', async () => {
    let calls = 0
    const url = await startWs(async (tok) => { calls++; await sleep(20); return testVerifier(tok) })
    const cs = await Promise.all(Array.from({ length: 10 }, () => connect(url)))
    opened.push(...cs)
    for (const c of cs) c.send({ type: 'auth', accessToken: 'tok:shared' })
    for (const c of cs) assert.ok(await c.waitFor(m => m.type === 'auth-result' && m.userId === 'shared'))
    assert.equal(calls, 1)
  })
})
