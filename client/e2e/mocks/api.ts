/**
 * 가짜 서버 HTTP API (Express 대신). 테스트가 필요한 경로만 답하고, 모르는 경로는 위반으로 기록한다.
 * 핸들러는 테스트가 on()으로 덮어쓸 수 있다(예: 결제 승인 503).
 */
import type { BrowserContext, Route } from '@playwright/test'
import { API_URL } from '../env'
import { STEM_HISTORY } from '../fixtures'
import { fulfillJson, fulfillPreflight, type Violations } from './http'
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

  constructor(private readonly violations: Violations) {
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
