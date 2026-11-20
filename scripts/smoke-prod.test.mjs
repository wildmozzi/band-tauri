// node --test scripts/ — 판정 로직만(가짜 응답). 네트워크를 쓰지 않는다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEFAULT_SERVER, DEFAULT_WEB, FOREIGN_ORIGIN,
  findEntryScript, findSensitiveKeys, judgeAsset, judgeAuthRequired, judgeCorsAllowed, judgeCorsRejected,
  judgeHealth, judgeMissingAsset, judgeOpsHidden, judgePlans, judgeWebHtml, parseArgs, runSmoke,
} from './smoke-prod.mjs'

const res = (status, headers = {}, body) => ({ status, headers, body })
const WEB = 'https://indigomuse.art'

test('health: 200 + status ok만 통과', () => {
  assert.equal(judgeHealth(res(200, {}, '{"status":"ok","timestamp":"x"}')).ok, true)
  assert.equal(judgeHealth(res(200, {}, '{"status":"degraded"}')).ok, false)
  assert.equal(judgeHealth(res(200, {}, '<html>')).ok, false)
  assert.equal(judgeHealth(res(503, {}, '{"status":"ok"}')).ok, false)
  assert.match(judgeHealth(res(301, { location: 'https://x/' })).detail, /301 → https:\/\/x\//)
})

test('ops/activity: 401만 통과, 404(경로 꺼짐)·200(공개)은 실패', () => {
  assert.equal(judgeOpsHidden(res(401)).ok, true)
  assert.match(judgeOpsHidden(res(404)).detail, /OPS_TOKEN/)
  assert.equal(judgeOpsHidden(res(404)).ok, false)
  assert.match(judgeOpsHidden(res(200)).detail, /공개/)
  assert.equal(judgeOpsHidden(res(500)).ok, false)
})

test('CORS 허용: ACAO가 정확히 그 오리진', () => {
  assert.equal(judgeCorsAllowed(res(204, { 'access-control-allow-origin': WEB }), WEB).ok, true)
  assert.equal(judgeCorsAllowed(res(200, { 'access-control-allow-origin': '*' }), WEB).ok, false)
  assert.equal(judgeCorsAllowed(res(200, {}), WEB).ok, false)
  assert.equal(judgeCorsAllowed(res(200, { 'access-control-allow-origin': 'https://www.indigomuse.art' }), WEB).ok, false)
  assert.equal(judgeCorsAllowed(res(500, { 'access-control-allow-origin': WEB }), WEB).ok, false)
})

test('CORS 거부: ACAO가 없어야 함(상태 코드 무관, allow-methods만 있는 것은 괜찮다)', () => {
  // cors 미들웨어는 거부한 오리진에도 204 + allow-methods를 보낸다 — 브라우저는 ACAO가 없으면 막는다
  assert.equal(judgeCorsRejected(res(204, { 'access-control-allow-methods': 'GET,POST' }), FOREIGN_ORIGIN).ok, true)
  assert.equal(judgeCorsRejected(res(200, {}), FOREIGN_ORIGIN).ok, true)
  assert.equal(judgeCorsRejected(res(200, { 'access-control-allow-origin': '*' }), FOREIGN_ORIGIN).ok, false)
  assert.equal(judgeCorsRejected(res(204, { 'access-control-allow-origin': FOREIGN_ORIGIN }), FOREIGN_ORIGIN).ok, false)
})

test('웹 HTML: 200 + text/html + no-cache', () => {
  const ok = res(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-cache' })
  assert.equal(judgeWebHtml(ok).ok, true)
  assert.equal(judgeWebHtml(res(200, { 'content-type': 'text/html', 'cache-control': 'public, max-age=3600' })).ok, false)
  assert.equal(judgeWebHtml(res(200, { 'content-type': 'application/json', 'cache-control': 'no-cache' })).ok, false)
  assert.equal(judgeWebHtml(res(404, { 'content-type': 'text/html', 'cache-control': 'no-cache' })).ok, false)
  assert.equal(judgeWebHtml(res(308, { location: '/rooms/' })).ok, false)
})

test('엔트리 스크립트 찾기: /assets/*.js 모듈만', () => {
  const html = `<html><head>
    <script>window.x=1</script>
    <script src="https://cdn.example/x.js"></script>
    <script type="module" crossorigin src="/assets/index-BsetYCAP.js"></script>
    <link rel="modulepreload" href="/assets/vendor-1.js">`
  assert.equal(findEntryScript(html), '/assets/index-BsetYCAP.js')
  assert.equal(findEntryScript("<script type=module src='/assets/a.js?v=1'></script>"), '/assets/a.js?v=1')
  assert.equal(findEntryScript('<script src="/assets/a.css"></script>'), null)
  assert.equal(findEntryScript(null), null)
})

test('자산: 200 + javascript + immutable / 없는 자산은 404', () => {
  assert.equal(judgeAsset(res(200, { 'content-type': 'application/javascript; charset=utf-8', 'cache-control': 'public, max-age=31536000, immutable' })).ok, true)
  assert.equal(judgeAsset(res(200, { 'content-type': 'text/javascript', 'cache-control': 'no-cache' })).ok, false)
  assert.equal(judgeAsset(res(200, { 'content-type': 'text/html', 'cache-control': 'immutable' })).ok, false)
  assert.equal(judgeMissingAsset(res(404, { 'content-type': 'text/plain' })).ok, true)
  assert.match(judgeMissingAsset(res(200, { 'content-type': 'text/html' })).detail, /rewrite/)
  assert.equal(judgeMissingAsset(res(200, { 'content-type': 'text/html' })).ok, false)
})

test('민감 키 찾기: 중첩·배열·대소문자', () => {
  assert.deepEqual(findSensitiveKeys({ a: [{ Email: 'x' }, { ok: 1 }], b: { phone: null, n: { access_token: 't' } } }),
    ['$.a[0].Email', '$.b.phone', '$.b.n.access_token'])
  assert.deepEqual(findSensitiveKeys({ name: '무료 플랜', features: ['email 지원'] }), []) // 값 안의 단어는 키가 아니다
  assert.deepEqual(findSensitiveKeys('str'), [])
})

test('plans: 형태와 민감 필드', () => {
  const body = {
    success: true,
    plans: {
      free: { name: '무료', price: 0, features: ['a'] },
      pro: { name: 'Pro', price: 6900, features: [] },
    },
  }
  assert.equal(judgePlans(res(200, {}, JSON.stringify(body))).ok, true)
  assert.equal(judgePlans(res(200, {}, JSON.stringify({ ...body, plans: { pro: body.plans.pro } }))).ok, false) // free 없음
  assert.equal(judgePlans(res(200, {}, JSON.stringify({ success: true, plans: { free: { name: 'x', price: '0', features: [] } } }))).ok, false)
  const leak = { ...body, owner: { email: 'a@b.c' } }
  assert.match(judgePlans(res(200, {}, JSON.stringify(leak))).detail, /\$\.owner\.email/)
  assert.equal(judgePlans(res(401, {}, '{}')).ok, false)
  assert.equal(judgePlans(res(200, {}, 'not json')).ok, false)
})

test('인증 필요: 401/403 통과, 2xx는 열림, 그 외 실패', () => {
  assert.equal(judgeAuthRequired(res(401)).ok, true)
  assert.equal(judgeAuthRequired(res(403)).ok, true)
  assert.match(judgeAuthRequired(res(200)).detail, /열려/)
  assert.equal(judgeAuthRequired(res(404)).ok, false)
  assert.equal(judgeAuthRequired(res(429)).ok, false)
})

test('인자: 기본값·환경변수·플래그·오류', () => {
  assert.deepEqual(parseArgs([], {}), { web: DEFAULT_WEB, server: DEFAULT_SERVER, timeoutMs: 15000 })
  assert.equal(parseArgs([], { SMOKE_WEB_URL: 'https://a.test/' }).web, 'https://a.test')
  const a = parseArgs(['--web', 'http://localhost:5173/x', '--server', 'https://s.test', '--timeout', '500'], { SMOKE_SERVER_URL: 'https://env.test' })
  assert.deepEqual(a, { web: 'http://localhost:5173', server: 'https://s.test', timeoutMs: 500 })
  assert.throws(() => parseArgs(['--web']), /값이 필요/)
  assert.throws(() => parseArgs(['--web', 'nope']), /올바르지 않다/)
  assert.throws(() => parseArgs(['--server', 'ftp://x']), /http/)
  assert.throws(() => parseArgs(['--bogus']), /알 수 없는/)
  assert.throws(() => parseArgs(['--timeout', '0']), /timeout/)
})

test('runSmoke: 가짜 fetch로 전체 통과, 쓰기 메서드를 보내지 않는다', async (t) => {
  const SERVER = 'https://s.test'
  const methods = new Set()
  const html = '<script type="module" src="/assets/index-abc.js"></script>'
  t.mock.method(globalThis, 'fetch', async (url, init) => {
    methods.add(init.method)
    assert.equal(init.redirect, 'manual')
    assert.equal(init.headers?.Authorization, undefined)
    const u = new URL(url)
    const origin = init.headers?.Origin
    const hdr = { 'content-type': 'application/json' }
    if (origin === WEB) hdr['access-control-allow-origin'] = WEB
    if (u.origin === WEB) {
      if (u.pathname === '/assets/index-abc.js') return new Response('x', { headers: { 'content-type': 'application/javascript', 'cache-control': 'public, max-age=31536000, immutable' } })
      if (u.pathname.startsWith('/assets/')) return new Response('nf', { status: 404, headers: { 'content-type': 'text/plain' } })
      return new Response(html, { headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-cache' } })
    }
    if (init.method === 'OPTIONS') return new Response(null, { status: 204, headers: hdr })
    if (u.pathname === '/health') return new Response('{"status":"ok"}', { headers: hdr })
    if (u.pathname === '/api/payment/plans') return new Response(JSON.stringify({ success: true, plans: { free: { name: 'f', price: 0, features: [] } } }), { headers: hdr })
    return new Response('{}', { status: 401, headers: hdr })
  })
  const lines = []
  const { results, failed } = await runSmoke({ web: WEB, server: SERVER, timeoutMs: 1000 }, l => lines.push(l))
  assert.equal(failed, 0, lines.join('\n'))
  assert.ok(results.length >= 20)
  assert.deepEqual([...methods].sort(), ['GET', 'OPTIONS'])
})

test('runSmoke: 요청 실패·CORS 열림은 실패로 센다', async (t) => {
  t.mock.method(globalThis, 'fetch', async (url, init) => {
    if (new URL(url).origin === WEB) throw new TypeError('fetch failed', { cause: { code: 'ENOTFOUND' } })
    return new Response('{"status":"ok"}', { status: init.method === 'OPTIONS' ? 204 : 200, headers: { 'access-control-allow-origin': '*' } })
  })
  const lines = []
  const { failed } = await runSmoke({ web: WEB, server: 'https://s.test', timeoutMs: 1000 }, l => lines.push(l))
  assert.ok(failed > 10)
  assert.ok(lines.some(l => l.startsWith('FAIL') && l.includes('ENOTFOUND')))
  assert.ok(lines.some(l => l.startsWith('FAIL') && l.includes('/api/ops/activity') && l.includes('공개')))
})
