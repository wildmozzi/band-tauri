/**
 * 가짜 Supabase: Auth(GoTrue) · REST(PostgREST, 메모리 DB) · Realtime(Phoenix 채널) · Storage.
 * 실제 Supabase 프로젝트에는 어떤 요청도 가지 않는다(주소 자체가 env.ts의 가짜 호스트).
 */
import type { BrowserContext, Route, WebSocketRoute } from '@playwright/test'
import { SUPABASE_URL } from '../env'
import { USERS, type FixtureUser, type Row, type Tables } from '../fixtures'
import { MemoryPostgrest, UnsupportedQuery } from './postgrest'
import { corsHeaders, fulfillJson, fulfillPreflight, type Violations } from './http'

const TOKEN_TTL_S = 24 * 3600

function b64url(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url')
}

/** 서명 없는 가짜 JWT (형식만 맞춘다 — 클라이언트는 서명을 검증하지 않는다) */
export function fakeAccessToken(user: FixtureUser, expiresAt: number): string {
  const payload = { sub: user.id, email: user.email, role: 'authenticated', aud: 'authenticated', exp: expiresAt }
  return `${b64url({ alg: 'HS256', typ: 'JWT' })}.${b64url(payload)}.e2e-signature`
}

export function userFromToken(token: string | undefined): FixtureUser | null {
  if (!token) return null
  try {
    const payload = JSON.parse(Buffer.from(token.split('.')[1], 'base64url').toString('utf8')) as { sub?: string }
    return USERS.find(u => u.id === payload.sub) ?? null
  } catch {
    return null
  }
}

export function authUserJson(user: FixtureUser) {
  return {
    id: user.id,
    aud: 'authenticated',
    role: 'authenticated',
    email: user.email,
    email_confirmed_at: '2026-01-01T00:00:00.000Z',
    app_metadata: { provider: 'email', providers: ['email'] },
    user_metadata: { nickname: user.nickname, name: user.name },
    identities: [],
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
  }
}

/** supabase-js가 localStorage에 저장하는 세션 형식 */
export function sessionJson(user: FixtureUser) {
  const expiresAt = Math.floor(Date.now() / 1000) + TOKEN_TTL_S
  return {
    access_token: fakeAccessToken(user, expiresAt),
    token_type: 'bearer',
    expires_in: TOKEN_TTL_S,
    expires_at: expiresAt,
    refresh_token: `e2e-refresh-${user.id}`,
    user: authUserJson(user),
  }
}

function bearer(route: Route): string | undefined {
  const h = route.request().headers()['authorization']
  return h?.startsWith('Bearer ') ? h.slice(7) : undefined
}

export class FakeSupabase {
  readonly db: MemoryPostgrest

  constructor(tables: Tables, private readonly violations: Violations) {
    this.db = new MemoryPostgrest(tables, {
      // 비공개 방도 id로 조회 (마이그레이션 008의 get_room)
      get_room: (args: Row) => this.db.table('rooms').filter(r => r.id === args.p_room_id),
    })
  }

  private holds: { method: string; table: string; released: Promise<void>; waiting: number }[] = []

  /**
   * (method, table) REST 요청을 release() 전까지 붙잡는다 — 서버 응답 전 화면(낙관적 업데이트)을 검사할 때.
   * 붙잡힌 요청은 DB에 아직 반영되지 않는다. waiting()은 지금까지 붙잡힌 요청 수.
   */
  hold(method: string, table: string): { release: () => void; waiting: () => number } {
    let release!: () => void
    const entry = { method, table, released: new Promise<void>(r => (release = r)), waiting: 0 }
    this.holds.push(entry)
    return {
      release: () => {
        this.holds = this.holds.filter(h => h !== entry)
        release()
      },
      waiting: () => entry.waiting,
    }
  }

  async install(context: BrowserContext) {
    const host = new URL(SUPABASE_URL).host
    await context.route(url => url.host === host, route => this.handleHttp(route))
    await context.routeWebSocket(url => url.host === host, ws => this.handleRealtime(ws))
  }

  private async handleHttp(route: Route) {
    const req = route.request()
    if (req.method() === 'OPTIONS') return fulfillPreflight(route)
    const url = new URL(req.url())
    try {
      if (url.pathname.startsWith('/auth/v1/')) return await this.handleAuth(route, url)
      if (url.pathname.startsWith('/rest/v1/')) {
        const table = url.pathname.replace('/rest/v1/', '')
        const hold = this.holds.find(h => h.method === req.method() && h.table === table)
        if (hold) {
          hold.waiting++
          await hold.released
        }
        const res = this.db.handle({
          method: req.method(),
          url,
          headers: req.headers(),
          body: req.postData() ? req.postDataJSON() : undefined,
        })
        return route.fulfill({
          status: res.status,
          headers: { ...corsHeaders(), ...res.headers },
          body: res.body === undefined ? '' : JSON.stringify(res.body),
        })
      }
    } catch (err) {
      if (!(err instanceof UnsupportedQuery)) throw err
      this.violations.push(`Supabase mock 미지원: ${req.method()} ${url.pathname}${url.search} — ${err.message}`)
      return fulfillJson(route, 400, { message: err.message })
    }
    this.violations.push(`Supabase mock 미처리 경로: ${req.method()} ${url.pathname}`)
    return fulfillJson(route, 404, { message: 'e2e mock: unhandled' })
  }

  private async handleAuth(route: Route, url: URL) {
    const req = route.request()
    const path = url.pathname.replace('/auth/v1/', '')
    if (path === 'token' && url.searchParams.get('grant_type') === 'password') {
      const { email, password } = req.postDataJSON() as { email: string; password: string }
      const user = USERS.find(u => u.email === email && u.password === password)
      if (!user) {
        return fulfillJson(route, 400, {
          code: 400,
          error_code: 'invalid_credentials',
          msg: 'Invalid login credentials',
        })
      }
      return fulfillJson(route, 200, sessionJson(user))
    }
    if (path === 'token' && url.searchParams.get('grant_type') === 'refresh_token') {
      const { refresh_token } = req.postDataJSON() as { refresh_token: string }
      const user = USERS.find(u => refresh_token === `e2e-refresh-${u.id}`)
      if (!user) return fulfillJson(route, 400, { code: 400, error_code: 'refresh_token_not_found', msg: 'Invalid Refresh Token' })
      return fulfillJson(route, 200, sessionJson(user))
    }
    if (path === 'user' && req.method() === 'GET') {
      const user = userFromToken(bearer(route))
      if (!user) return fulfillJson(route, 401, { code: 401, error_code: 'bad_jwt', msg: 'invalid JWT' })
      return fulfillJson(route, 200, authUserJson(user))
    }
    if (path === 'logout') return route.fulfill({ status: 204, headers: corsHeaders() })
    this.violations.push(`Supabase Auth mock 미처리: ${req.method()} ${url.pathname}${url.search}`)
    return fulfillJson(route, 404, { msg: 'e2e mock: unhandled auth path' })
  }

  /**
   * Realtime(Phoenix) 최소 구현: join/heartbeat/access_token에 ok로 답한다.
   * vsn 1.0.0(JSON 객체)과 2.0.0(배열) 직렬화를 모두 받는다.
   */
  private handleRealtime(ws: WebSocketRoute) {
    ws.onMessage(message => {
      if (typeof message !== 'string') return
      let parsed: unknown
      try {
        parsed = JSON.parse(message)
      } catch {
        return
      }
      type PhxMessage = { join_ref?: string | null; ref?: string | null; topic: string; event: string; payload: unknown }
      const asArray = Array.isArray(parsed)
      const msg: PhxMessage = Array.isArray(parsed)
        ? { join_ref: parsed[0], ref: parsed[1], topic: parsed[2], event: parsed[3], payload: parsed[4] }
        : (parsed as PhxMessage)
      if (!msg.ref) return
      let response: Record<string, unknown> = {}
      if (msg.event === 'phx_join') {
        const config = (msg.payload as { config?: { postgres_changes?: Row[] } })?.config
        response = {
          postgres_changes: (config?.postgres_changes ?? []).map((pc, i) => ({ ...pc, id: 1000 + i })),
        }
      }
      const reply = { status: 'ok', response }
      ws.send(
        JSON.stringify(
          asArray
            ? [msg.join_ref ?? null, msg.ref, msg.topic, 'phx_reply', reply]
            : { join_ref: msg.join_ref, ref: msg.ref, topic: msg.topic, event: 'phx_reply', payload: reply },
        ),
      )
    })
  }
}
