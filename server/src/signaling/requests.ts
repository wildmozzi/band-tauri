import type { InstrumentChangeRequest, PerformRequest } from './protocol'

/**
 * 참여 요청·악기 변경 요청 (설계 docs/server-architecture.md 2.1절 I, 계층 2).
 * 요청 맵의 소유자. S2에서는 요청 분기(if 사슬)가 그대로라 맵을 내보낸다(S3에서 handle*로, S4에서 감춤).
 */
export function createRequests() {
  const performRequests = new Map<string, PerformRequest[]>()
  const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()
  return { performRequests, instrumentChangeRequests }
}

export type Requests = ReturnType<typeof createRequests>
