import type { Route } from '@playwright/test'

/** mock이 처리하지 못했거나 막은 요청 기록. 테스트 끝에 비어 있어야 한다 */
export type Violations = string[]

/** 페이지(127.0.0.1)와 가짜 호스트가 다른 출처라 CORS 헤더가 필요하다 */
export function corsHeaders(): Record<string, string> {
  return {
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Expose-Headers': 'Content-Range, X-Total-Count',
  }
}

/**
 * CORS preflight 응답. Allow-Headers의 `*`는 Authorization을 포함하지 않으므로(스펙) 요청한 헤더를 그대로 돌려준다.
 */
export function fulfillPreflight(route: Route) {
  const requested = route.request().headers()['access-control-request-headers'] ?? ''
  return route.fulfill({
    status: 204,
    headers: {
      ...corsHeaders(),
      'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,HEAD,OPTIONS',
      'Access-Control-Allow-Headers': requested || '*',
      'Access-Control-Max-Age': '600',
    },
  })
}

export function fulfillJson(route: Route, status: number, body: unknown) {
  return route.fulfill({
    status,
    headers: { ...corsHeaders(), 'Content-Type': 'application/json; charset=utf-8' },
    body: JSON.stringify(body),
  })
}
