#!/usr/bin/env node
// 운영 스모크 점검 (배포 뒤 읽기 전용 확인) — docs/RELEASE.md 4절 "배포 뒤".
//
//   npm run smoke:prod
//   npm run smoke:prod -- --web https://indigomuse.art --server https://band-server.fly.dev
//   SMOKE_WEB_URL=… SMOKE_SERVER_URL=… npm run smoke:prod
//
// GET/HEAD/OPTIONS만 보낸다. 로그인·토큰·비밀값을 쓰지 않는다. 쓰기 요청은 하지 않는다.
// 리다이렉트는 따라가지 않는다(설정된 주소가 직접 응답하는지 본다).
// 종료 코드: 0 전부 통과, 1 실패가 하나라도 있음, 2 인자 오류.
// 판정은 아래 순수 함수(judge*)가 하고, 단위 시험은 smoke-prod.test.mjs(가짜 응답)다. CI에서는 실제 주소로 돌리지 않는다.
import { pathToFileURL } from 'node:url'

export const DEFAULT_WEB = 'https://indigomuse.art'
export const DEFAULT_SERVER = 'https://band-server.fly.dev'
/** 허용되면 안 되는 임의 오리진 (.invalid는 존재할 수 없는 TLD, RFC 2606) */
export const FOREIGN_ORIGIN = 'https://example.invalid'

/** 공개 응답에 나오면 안 되는 개인정보·비밀 필드 이름(소문자 비교) */
export const SENSITIVE_KEYS = [
  'email', 'phone', 'phone_number', 'phonenumber', 'birth_date', 'birthdate', 'gender', 'name_real',
  'password', 'password_hash', 'passwordhash', 'token', 'access_token', 'accesstoken', 'refresh_token',
  'refreshtoken', 'secret', 'service_key', 'servicekey', 'api_key', 'apikey', 'payment_key', 'paymentkey',
]

/** 인증 없이 GET하면 401이어야 하는 경로 (router.use(requireAuth) 뒤 또는 핸들러 안에서 401) */
export const AUTH_REQUIRED_GETS = [
  '/api/rooms',                    // rooms 라우터 전체 requireAuth (방 생성·비밀번호)
  '/api/posts',                    // posts 라우터 전체 requireAuth (글쓰기·수정)
  '/api/payment/subscription',
  '/api/payment/orders/smoke-nonexistent',
  '/api/music/stem-history',
  '/api/music/file/smoke-nonexistent.pdf',
]

/** 쓰기 API: 임의 오리진의 프리플라이트에 허용 헤더가 나오면 안 된다 (요청 자체는 OPTIONS만) */
export const WRITE_ENDPOINTS = [
  { path: '/api/rooms', method: 'POST' },
  { path: '/api/posts', method: 'POST' },
  { path: '/api/payment/orders', method: 'POST' },
  { path: '/api/storage/post-images/upload-url', method: 'POST' },
  { path: '/api/storage/avatars/upload-url', method: 'POST' },
  { path: '/api/music/upload-score', method: 'POST' },
]

// ---------- 판정 (순수 함수) ----------
// res 형태: { status: number, headers: { [소문자 이름]: string }, body?: string }
// 반환: { ok: boolean, detail: string }

const pass = (detail) => ({ ok: true, detail })
const fail = (detail) => ({ ok: false, detail })
const h = (res, name) => res.headers?.[name.toLowerCase()]
const statusText = (res) => (h(res, 'location') ? `${res.status} → ${h(res, 'location')}` : String(res.status))

function parseJson(body) {
  try { return { value: JSON.parse(body ?? '') } } catch { return { error: true } }
}

export function judgeHealth(res) {
  if (res.status !== 200) return fail(`status ${statusText(res)} (200 기대)`)
  const j = parseJson(res.body)
  if (j.error) return fail('JSON이 아님')
  if (j.value?.status !== 'ok') return fail(`status 필드 ${JSON.stringify(j.value?.status)} ("ok" 기대)`)
  return pass('200, status ok')
}

export function judgeOpsHidden(res) {
  if (res.status === 401) return pass('토큰 없이 401')
  if (res.status === 404) return fail('404 — OPS_TOKEN이 없거나 짧아 경로가 꺼져 있다(배포 인원 확인 불가)')
  if (res.status >= 200 && res.status < 300) return fail(`${res.status} — 토큰 없이 이용 현황이 공개됨`)
  return fail(`status ${statusText(res)} (401 기대)`)
}

/** 허용 오리진: ACAO가 정확히 그 오리진. '*'는 실패 */
export function judgeCorsAllowed(res, origin) {
  const acao = h(res, 'access-control-allow-origin')
  if (res.status >= 400) return fail(`status ${statusText(res)}`)
  if (acao === origin) return pass(`ACAO = ${origin}`)
  return fail(`ACAO ${acao === undefined ? '없음' : JSON.stringify(acao)} (${origin} 기대 — fly secrets CORS_ORIGINS 확인)`)
}

/** 임의 오리진: ACAO가 없어야 한다('*'나 그 오리진이면 실패). 상태 코드는 상관없다(204/200이어도 브라우저가 막는다) */
export function judgeCorsRejected(res, origin) {
  const acao = h(res, 'access-control-allow-origin')
  if (acao === undefined || acao === '') return pass('ACAO 없음')
  return fail(`ACAO ${JSON.stringify(acao)} — ${origin}이 허용됨`)
}

const isHtml = (res) => /^text\/html\b/i.test(h(res, 'content-type') ?? '')
const isNoCache = (res) => /\b(no-cache|no-store|max-age=0)\b/i.test(h(res, 'cache-control') ?? '')

export function judgeWebHtml(res, { requireNoCache = true } = {}) {
  if (res.status !== 200) return fail(`status ${statusText(res)} (200 기대)`)
  if (!isHtml(res)) return fail(`content-type ${JSON.stringify(h(res, 'content-type'))} (text/html 기대)`)
  if (requireNoCache && !isNoCache(res)) return fail(`cache-control ${JSON.stringify(h(res, 'cache-control'))} (no-cache 기대 — 배포 뒤 옛 HTML이 남는다)`)
  return pass(`200 html, cache-control ${h(res, 'cache-control')}`)
}

/** HTML에서 엔트리 모듈 스크립트(/assets/*.js) 경로 */
export function findEntryScript(html) {
  const tags = String(html ?? '').match(/<script\b[^>]*>/gi) ?? []
  for (const tag of tags) {
    const src = /\bsrc\s*=\s*["']([^"']+)["']/i.exec(tag)?.[1]
    if (src && /^\/assets\/[^?#]+\.js(\?|#|$)/.test(src)) return src
  }
  return null
}

export function judgeAsset(res) {
  if (res.status !== 200) return fail(`status ${statusText(res)} (200 기대)`)
  const ct = h(res, 'content-type') ?? ''
  if (!/javascript/i.test(ct)) return fail(`content-type ${JSON.stringify(ct)} (javascript 기대)`)
  const cc = h(res, 'cache-control') ?? ''
  if (!/\bimmutable\b/i.test(cc)) return fail(`cache-control ${JSON.stringify(cc)} (immutable 기대)`)
  return pass(`200 js, ${cc}`)
}

/** 없는 자산은 404여야 한다 (index.html로 rewrite되면 200 html → 실패: 깨진 청크가 HTML로 와서 모듈 오류가 된다) */
export function judgeMissingAsset(res) {
  if (res.status === 404) return pass('404')
  if (res.status === 200 && isHtml(res)) return fail('200 html — SPA rewrite가 /assets까지 먹는다(vercel.json)')
  return fail(`status ${statusText(res)} (404 기대)`)
}

/** JSON 안 어디에든 민감 키가 있으면 그 경로들 */
export function findSensitiveKeys(value, keys = SENSITIVE_KEYS, path = '$') {
  const found = []
  if (Array.isArray(value)) {
    value.forEach((v, i) => found.push(...findSensitiveKeys(v, keys, `${path}[${i}]`)))
  } else if (value && typeof value === 'object') {
    for (const [k, v] of Object.entries(value)) {
      const p = `${path}.${k}`
      if (keys.includes(k.toLowerCase())) found.push(p)
      found.push(...findSensitiveKeys(v, keys, p))
    }
  }
  return found
}

/** GET /api/payment/plans (server/src/routes/payment.ts — 유일한 익명 공개 JSON API) */
export function judgePlans(res) {
  if (res.status !== 200) return fail(`status ${statusText(res)} (200 기대)`)
  const j = parseJson(res.body)
  if (j.error) return fail('JSON이 아님')
  const v = j.value
  if (v?.success !== true || !v.plans || typeof v.plans !== 'object') return fail('{ success: true, plans: {…} } 형태가 아님')
  const ids = Object.keys(v.plans)
  if (!ids.includes('free')) return fail(`free 플랜 없음 (${ids.join(', ')})`)
  for (const id of ids) {
    const p = v.plans[id]
    if (typeof p?.name !== 'string' || typeof p?.price !== 'number' || !Array.isArray(p?.features)) {
      return fail(`plans.${id}에 name/price/features 형태가 아님`)
    }
  }
  const leaked = findSensitiveKeys(v)
  if (leaked.length) return fail(`민감 필드 노출: ${leaked.join(', ')}`)
  return pass(`200, 플랜 ${ids.join('/')}, 민감 필드 없음`)
}

export function judgeAuthRequired(res) {
  if (res.status === 401 || res.status === 403) return pass(String(res.status))
  if (res.status >= 200 && res.status < 300) return fail(`${res.status} — 인증 없이 열려 있음`)
  return fail(`status ${statusText(res)} (401 기대)`)
}

// ---------- 실행 ----------

export function parseArgs(argv, env = process.env) {
  const out = { web: env.SMOKE_WEB_URL || DEFAULT_WEB, server: env.SMOKE_SERVER_URL || DEFAULT_SERVER, timeoutMs: 15000 }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    const next = () => {
      const v = argv[++i]
      if (v === undefined) throw new Error(`${a} 뒤에 값이 필요하다`)
      return v
    }
    if (a === '--web') out.web = next()
    else if (a === '--server') out.server = next()
    else if (a === '--timeout') out.timeoutMs = Number(next())
    else if (a === '-h' || a === '--help') out.help = true
    else throw new Error(`알 수 없는 인자: ${a}`)
  }
  for (const k of ['web', 'server']) {
    let u
    try { u = new URL(out[k]) } catch { throw new Error(`--${k} 주소가 올바르지 않다: ${out[k]}`) }
    if (u.protocol !== 'https:' && u.protocol !== 'http:') throw new Error(`--${k}는 http(s) 주소여야 한다`)
    out[k] = u.origin
  }
  if (!(out.timeoutMs > 0)) throw new Error('--timeout은 양수(ms)')
  return out
}

async function request(url, { method = 'GET', headers = {}, timeoutMs, readBody = true }) {
  if (!['GET', 'HEAD', 'OPTIONS'].includes(method)) throw new Error(`읽기 전용 스크립트: ${method} 금지`)
  const r = await fetch(url, { method, headers, redirect: 'manual', signal: AbortSignal.timeout(timeoutMs) })
  const hdrs = {}
  r.headers.forEach((v, k) => { hdrs[k.toLowerCase()] = v })
  const body = readBody && method !== 'HEAD' ? await r.text() : (await r.body?.cancel(), undefined)
  return { status: r.status, headers: hdrs, body }
}

function preflightHeaders(origin, method = 'GET') {
  return { Origin: origin, 'Access-Control-Request-Method': method, 'Access-Control-Request-Headers': 'authorization,content-type' }
}

export async function runSmoke({ web, server, timeoutMs }, log = console.log) {
  const results = []
  const check = async (name, fn) => {
    let r
    try { r = await fn() } catch (e) { r = fail(`요청 실패: ${e?.cause?.code ?? e?.name ?? ''} ${e?.message ?? e}`.trim()) }
    results.push({ name, ...r })
    log(`${r.ok ? 'PASS' : 'FAIL'}  ${name} — ${r.detail}`)
  }
  const get = (url, headers) => request(url, { headers, timeoutMs })
  const options = (url, headers) => request(url, { method: 'OPTIONS', headers, timeoutMs, readBody: false })

  log(`웹 ${web} · 서버 ${server}\n`)

  // 1. 헬스
  await check('서버 /health', async () => judgeHealth(await get(`${server}/health`)))
  // 2. 운영 경로 비공개
  await check('서버 /api/ops/activity 비공개', async () => judgeOpsHidden(await get(`${server}/api/ops/activity`)))
  // 3. CORS (securityConfig.ts: CORS_ORIGINS 목록 + Tauri 오리진만 허용)
  await check(`CORS GET /health Origin=${web} 허용`, async () => judgeCorsAllowed(await get(`${server}/health`, { Origin: web }), web))
  await check(`CORS 프리플라이트 /api/rooms Origin=${web} 허용`, async () =>
    judgeCorsAllowed(await options(`${server}/api/rooms`, preflightHeaders(web, 'POST')), web))
  await check(`CORS GET /health Origin=${FOREIGN_ORIGIN} 거부`, async () =>
    judgeCorsRejected(await get(`${server}/health`, { Origin: FOREIGN_ORIGIN }), FOREIGN_ORIGIN))
  await check(`CORS 프리플라이트 /api/rooms Origin=${FOREIGN_ORIGIN} 거부`, async () =>
    judgeCorsRejected(await options(`${server}/api/rooms`, preflightHeaders(FOREIGN_ORIGIN, 'POST')), FOREIGN_ORIGIN))

  // 4. 웹
  let html = null
  await check('웹 /', async () => {
    const res = await get(`${web}/`)
    if (res.status === 200) html = res.body
    return judgeWebHtml(res)
  })
  for (const p of ['/community', '/rooms']) {
    await check(`웹 ${p} (SPA 깊은 경로)`, async () => judgeWebHtml(await get(`${web}${p}`)))
  }
  await check('웹 엔트리 JS', async () => {
    const src = findEntryScript(html)
    if (!src) return fail(html == null ? '/ 를 못 읽어 건너뜀' : 'HTML에서 /assets/*.js 스크립트를 못 찾음')
    const r = judgeAsset(await request(`${web}${src}`, { timeoutMs, readBody: false }))
    return { ...r, detail: `${src} ${r.detail}` }
  })
  await check('웹 없는 /assets/x.js', async () => judgeMissingAsset(await get(`${web}/assets/smoke-nonexistent-x.js`)))

  // 5. 익명 공개 API
  await check('서버 GET /api/payment/plans (공개, 민감 필드 없음)', async () => judgePlans(await get(`${server}/api/payment/plans`)))

  // 6. 인증이 필요한 경로가 열려 있지 않은지 (쓰기는 하지 않는다)
  for (const p of AUTH_REQUIRED_GETS) {
    await check(`서버 GET ${p} 인증 필요`, async () => judgeAuthRequired(await get(`${server}${p}`)))
  }
  for (const { path, method } of WRITE_ENDPOINTS) {
    await check(`서버 ${method} ${path} 프리플라이트 Origin=${FOREIGN_ORIGIN} 거부`, async () =>
      judgeCorsRejected(await options(`${server}${path}`, preflightHeaders(FOREIGN_ORIGIN, method)), FOREIGN_ORIGIN))
  }

  const failed = results.filter(r => !r.ok)
  log(`\n${results.length - failed.length}/${results.length} 통과${failed.length ? ` — 실패 ${failed.length}` : ''}`)
  return { results, failed: failed.length }
}

const HELP = `운영 스모크 점검 (읽기 전용: GET/HEAD/OPTIONS, 토큰 없음)
  npm run smoke:prod -- [--web <url>] [--server <url>] [--timeout <ms>]
  환경변수: SMOKE_WEB_URL, SMOKE_SERVER_URL (기본 ${DEFAULT_WEB}, ${DEFAULT_SERVER})
  종료 코드: 0 통과, 1 실패 있음, 2 인자 오류`

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  let args
  try {
    args = parseArgs(process.argv.slice(2))
  } catch (e) {
    console.error(`${e.message}\n\n${HELP}`)
    process.exit(2)
  }
  if (args.help) {
    console.log(HELP)
    process.exit(0)
  }
  const { failed } = await runSmoke(args)
  process.exit(failed ? 1 : 0)
}
