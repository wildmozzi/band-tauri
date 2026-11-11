/**
 * 가짜 서버 HTTP API (Express 대신). 테스트가 필요한 경로만 답하고, 모르는 경로는 위반으로 기록한다.
 * 핸들러는 테스트가 on()으로 덮어쓸 수 있다(예: 결제 승인 503).
 */
import type { BrowserContext, Route } from '@playwright/test'
import { API_URL } from '../env'
import { STEM_HISTORY } from '../fixtures'
import { fulfillJson, fulfillPreflight, type Violations } from './http'
import type { MemoryPostgrest } from './postgrest'
import { userFromToken } from './supabase'

export interface ApiCall {
  method: string
  path: string
  body: unknown
  userId: string | null
}

export type ApiHandler = (call: ApiCall, route: Route) => Promise<void> | void

const PLAN_PRICES = { standard: 2900, pro: 6900 } as const

export class FakeApi {
  readonly calls: ApiCall[] = []
  private handlers = new Map<string, ApiHandler>()

  constructor(private readonly violations: Violations, private readonly db: MemoryPostgrest) {
    // 게시글 작성·수정 (P3-10d, server/src/routes/posts.ts의 응답 형태). 작성자는 토큰 사용자,
    // 이미지는 본인 폴더의 서버 조회 URL만(실제 검증 규칙은 server/test/posts.test.ts가 본다)
    const ownImage = (userId: string, url: string) => url.startsWith(`${API_URL}/api/storage/post-images/${userId}/`)
    this.on('POST /api/posts', (call, route) => {
      if (!call.userId) return fulfillJson(route, 401, { success: false, error: '로그인이 필요합니다.' })
      const body = call.body as { title: string; content: string; tags?: string[]; images?: string[]; instrument?: string; category?: string }
      const images = body.images ?? []
      const rejected = images.filter(u => !ownImage(call.userId!, u)).map(url => ({ url, reason: '이 서비스에 올린 이미지만 붙일 수 있습니다.' }))
      if (rejected.length) return fulfillJson(route, 422, { success: false, error: '저장할 수 없는 이미지가 있습니다.', rejected })
      const profile = this.db.table('profiles').find(p => p.id === call.userId)
      const post = {
        id: `e2e-post-${this.db.table('posts').length + 1}`,
        author_id: call.userId,
        author_name: (profile?.nickname as string | undefined) ?? 'User',
        author_role: 'Session Member',
        title: body.title.trim(),
        content: body.content.trim(),
        category: body.category ?? 'general',
        instrument: body.instrument ?? 'other',
        tags: body.tags ?? [],
        images,
        likes_count: 0,
        comments_count: 0,
        views_count: 0,
        created_at: new Date().toISOString(),
        updated_at: null,
      }
      this.db.table('posts').unshift(post)
      return fulfillJson(route, 201, { success: true, post })
    })
    this.on('PUT /api/posts/*', (call, route) => {
      if (!call.userId) return fulfillJson(route, 401, { success: false, error: '로그인이 필요합니다.' })
      const id = decodeURIComponent(call.path.slice('/api/posts/'.length))
      const post = this.db.table('posts').find(p => p.id === id)
      if (!post) return fulfillJson(route, 404, { success: false, error: '게시글을 찾을 수 없습니다.' })
      if (post.author_id !== call.userId) return fulfillJson(route, 403, { success: false, error: '작성자만 수정할 수 있습니다.' })
      Object.assign(post, call.body as object, { updated_at: new Date().toISOString() })
      return fulfillJson(route, 200, { success: true, post })
    })

    // 기본 응답들
    this.on('GET /api/payment/subscription', (call, route) =>
      call.userId
        ? fulfillJson(route, 200, { success: true, user: { id: call.userId, planType: 'free' } })
        : fulfillJson(route, 401, { success: false, error: '로그인이 필요합니다.' }),
    )
    this.on('POST /api/payment/orders', (call, route) => {
      const planType = (call.body as { planType: 'standard' | 'pro' }).planType
      return fulfillJson(route, 201, {
        success: true,
        order: {
          orderId: `order_e2e_${planType}_1`,
          planType,
          amount: PLAN_PRICES[planType],
          expiresAt: new Date(Date.now() + 30 * 60_000).toISOString(),
        },
      })
    })
    this.on('GET /api/music/stem-history', (call, route) =>
      call.userId
        ? fulfillJson(route, 200, { success: true, separations: STEM_HISTORY })
        : fulfillJson(route, 401, { success: false, error: '로그인이 필요합니다.' }),
    )
  }

  /** "METHOD /path" 핸들러 등록(덮어쓰기). path 끝의 `*`는 접두사 일치 */
  on(key: string, handler: ApiHandler) {
    this.handlers.set(key, handler)
  }

  callsTo(method: string, path: string): ApiCall[] {
    return this.calls.filter(c => c.method === method && (path.endsWith('*') ? c.path.startsWith(path.slice(0, -1)) : c.path === path))
  }

  async install(context: BrowserContext) {
    const host = new URL(API_URL).host
    await context.route(url => url.host === host, route => this.handle(route))
  }

  private async handle(route: Route) {
    const req = route.request()
    if (req.method() === 'OPTIONS') return fulfillPreflight(route)
    const url = new URL(req.url())
    const auth = req.headers()['authorization']
    const call: ApiCall = {
      method: req.method(),
      path: url.pathname,
      body: req.postData() ? req.postDataJSON() : undefined,
      userId: userFromToken(auth?.startsWith('Bearer ') ? auth.slice(7) : undefined)?.id ?? null,
    }
    this.calls.push(call)
    const exact = this.handlers.get(`${call.method} ${call.path}`)
    const prefix = [...this.handlers.entries()].find(
      ([k]) => k.endsWith('*') && `${call.method} ${call.path}`.startsWith(k.slice(0, -1)),
    )?.[1]
    const handler = exact ?? prefix
    if (handler) return handler(call, route)
    this.violations.push(`서버 API mock 미처리: ${call.method} ${call.path}`)
    return fulfillJson(route, 501, { success: false, error: 'e2e mock: unhandled' })
  }
}
