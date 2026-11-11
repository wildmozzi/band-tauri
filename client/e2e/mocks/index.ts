/**
 * 테스트 하나에 쓰는 가짜 백엔드 전체. installMocks(context)가 다음 순서로 라우트를 건다
 * (Playwright는 나중에 건 라우트를 먼저 본다):
 *   1) guard: 미리보기 서버(127.0.0.1)와 mock 호스트가 아닌 모든 HTTP/WS 요청을 차단·기록 → 테스트 실패
 *   2) Supabase(Auth/REST/Realtime), 서버 API, 시그널링 WS, 토스 SDK 스크립트
 */
import type { BrowserContext } from '@playwright/test'
import { E2E_BASE_URL, MOCKED_HOSTS, SUPABASE_STORAGE_KEY, TOSS_SCRIPT_HOST } from '../env'
import { seedTables, type FixtureUser } from '../fixtures'
import { FakeApi } from './api'
import type { Violations } from './http'
import { FakeSignaling } from './signaling'
import { FakeSupabase, sessionJson } from './supabase'

export type { Participant, FakeRoomConfig } from './signaling'
export { RATE_LIMIT_RETRY_MS } from './signaling'

/**
 * 토스 결제 SDK 대체 스크립트. 결제창 대신 successUrl로 바로 이동한다(토스가 붙이는 paymentKey·orderId·amount 포함).
 * 실제 토스 서버·결제창에는 접속하지 않는다.
 */
const TOSS_STUB = `
window.TossPayments = function (clientKey) {
  return {
    requestPayment: function (a, b) {
      var p = typeof a === 'string' ? b : a;
      var sep = p.successUrl.indexOf('?') >= 0 ? '&' : '?';
      window.__e2eTossRequest = { clientKey: clientKey, params: p };
      window.location.href = p.successUrl + sep + 'paymentKey=pk_e2e_test&orderId=' + encodeURIComponent(p.orderId) + '&amount=' + p.amount;
      return new Promise(function () {});
    }
  };
};
`

export interface MockBackend {
  supabase: FakeSupabase
  api: FakeApi
  signaling: FakeSignaling
  violations: Violations
  /** 다음 페이지 로드부터 user로 로그인된 상태 (supabase-js 세션을 localStorage에 한 번 심는다) */
  loginAs(user: FixtureUser): Promise<void>
}

export async function installMocks(context: BrowserContext): Promise<MockBackend> {
  const violations: Violations = []
  const appHost = new URL(E2E_BASE_URL).host

  // 1) guard (가장 먼저 = 가장 나중에 평가)
  await context.route(
    url => url.host !== appHost && !MOCKED_HOSTS.has(url.host),
    route => {
      violations.push(`외부 요청 차단: ${route.request().method()} ${route.request().url()}`)
      return route.abort('blockedbyclient')
    },
  )
  await context.routeWebSocket(
    url => !MOCKED_HOSTS.has(url.host),
    ws => {
      violations.push(`외부 WebSocket 차단: ${ws.url()}`)
      return ws.close({ code: 1008, reason: 'e2e guard' })
    },
  )

  // 2) mock들
  const supabase = new FakeSupabase(seedTables(), violations)
  await supabase.install(context)
  const api = new FakeApi(violations, supabase.db)
  await api.install(context)
  const signaling = new FakeSignaling(supabase.db, violations)
  await signaling.install(context)
  await context.route(
    url => url.host === TOSS_SCRIPT_HOST,
    route => route.fulfill({ status: 200, contentType: 'application/javascript', body: TOSS_STUB }),
  )

  return {
    supabase,
    api,
    signaling,
    violations,
    async loginAs(user) {
      await context.addInitScript(
        ([key, session]) => {
          // 한 번만 심는다: 테스트 중 로그아웃하면 새로고침해도 로그아웃 상태가 유지되게
          if (sessionStorage.getItem('e2e-session-seeded')) return
          sessionStorage.setItem('e2e-session-seeded', '1')
          localStorage.setItem(key, JSON.stringify(session))
        },
        [SUPABASE_STORAGE_KEY, sessionJson(user)] as const,
      )
    },
  }
}
