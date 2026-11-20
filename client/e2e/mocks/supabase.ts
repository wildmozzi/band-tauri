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
/** hold() 키: REST 테이블 이름과 겹치지 않는 가짜 이름 */
const AUTH_USER_HOLD = 'auth:user'

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
    user_metadata: {
      nickname: user.nickname,
      name: user.name,
      ...(user.bio ? { bio: user.bio } : {}),
      ...(user.instagram ? { instagram: user.instagram } : {}),
    },
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

  private holds: { method: string; table: string; released: Promise<void>; waiting: number; onArrive: () => void }[] = []

  /**
   * (method, table) REST 요청을 release() 전까지 붙잡는다 — 서버 응답 전 화면(낙관적 업데이트)을 검사할 때.
   * 붙잡힌 요청은 DB에 아직 반영되지 않는다. waiting()은 지금까지 붙잡힌 요청 수.
   * arrived()는 첫 요청이 붙잡히면 끝난다 — 클릭 직후 요청이 mock에 닿기 전에 waiting()을 동기로 보면 경합이라
   * (PR #25: 좋아요 테스트 flaky) 반드시 이걸 기다린 뒤에 검사한다.
   */
  hold(method: string, table: string): { release: () => void; waiting: () => number; arrived: () => Promise<void> } {
    let release!: () => void
    let onArrive!: () => void
    const arrived = new Promise<void>(r => (onArrive = r))
    const entry = { method, table, released: new Promise<void>(r => (release = r)), waiting: 0, onArrive }
    this.holds.push(entry)
    return {
      release: () => {
        this.holds = this.holds.filter(h => h !== entry)
        release()
      },
      waiting: () => entry.waiting,
      arrived: () => arrived,
    }
  }

  /** GET /auth/v1/user(세션 복원 시 사용자 조회)를 release() 전까지 붙잡는다 — 인증 확인 중 화면을 검사할 때 */
  holdAuthUser(): { release: () => void; waiting: () => number } {
    return this.hold('GET', AUTH_USER_HOLD)
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
          hold.onArrive()
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
      const hold = this.holds.find(h => h.method === 'GET' && h.table === AUTH_USER_HOLD)
      if (hold) {
        hold.waiting++
        await hold.released
      }
      const user = userFromToken(bearer(route))
      if (!user) return fulfillJson(route, 401, { code: 401, error_code: 'bad_jwt', msg: 'invalid JWT' })
      return fulfillJson(route, 200, authUserJson(user))
    }
    if (path === 'logout') return route.fulfill({ status: 204, headers: corsHeaders() })
    this.violations.push(`Supabase Auth mock 미처리: ${req.method()} ${url.pathname}${url.search}`)
    return fulfillJson(route, 404, { msg: 'e2e mock: unhandled auth path' })
  }

  /** 지금 구독 중인 postgres_changes 채널(조인 응답 때 준 바인딩 id·테이블) */
  private realtimeSubs: { ws: WebSocketRoute; topic: string; joinRef: string | null; asArray: boolean; bindings: { id: number; table: string }[] }[] = []

  /**
   * 테이블 변경을 구독자에게 보낸다(실제 Realtime의 postgres_changes 메시지 형식). 보낸 채널 수를 돌려준다.
   * 메모리 DB는 직접 고친 뒤 부른다 — 앱은 이 알림을 받고 다시 조회한다.
   */
  pushTableChange(table: string, type: 'INSERT' | 'UPDATE' | 'DELETE', record: Row): number {
    let sent = 0
    for (const sub of this.realtimeSubs) {
      const ids = sub.bindings.filter(b => b.table === table || b.table === '*').map(b => b.id)
      if (ids.length === 0) continue
      const payload = {
        ids,
        data: { schema: 'public', table, commit_timestamp: new Date().toISOString(), type, columns: [], record, old_record: {}, errors: null },
      }
      sub.ws.send(JSON.stringify(sub.asArray ? [sub.joinRef, null, sub.topic, 'postgres_changes', payload] : { join_ref: sub.joinRef, ref: null, topic: sub.topic, event: 'postgres_changes', payload }))
      sent++
    }
    return sent
  }

  /** 지금 postgres_changes를 구독 중인 채널 수 */
  realtimeSubscriptions(table: string): number {
    return this.realtimeSubs.filter(s => s.bindings.some(b => b.table === table)).length
  }

  /**
   * Realtime(Phoenix) 최소 구현: join/heartbeat/access_token에 ok로 답하고, postgres_changes 구독을 기억한다(pushTableChange).
   * vsn 1.0.0(JSON 객체)과 2.0.0(배열) 직렬화를 모두 받는다.
   */
  private handleRealtime(ws: WebSocketRoute) {
    ws.onClose(() => {
      this.realtimeSubs = this.realtimeSubs.filter(s => s.ws !== ws)
    })
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
        const changes = (config?.postgres_changes ?? []).map((pc, i) => ({ ...pc, id: 1000 + i }))
        response = { postgres_changes: changes }
        this.realtimeSubs = this.realtimeSubs.filter(s => !(s.ws === ws && s.topic === msg.topic))
        if (changes.length > 0) {
          this.realtimeSubs.push({
            ws,
            topic: msg.topic,
            joinRef: msg.join_ref ?? null,
            asArray,
            bindings: changes.map(c => ({ id: c.id, table: String((c as Row).table ?? '*') })),
          })
        }
      }
      if (msg.event === 'phx_leave') this.realtimeSubs = this.realtimeSubs.filter(s => !(s.ws === ws && s.topic === msg.topic))
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
