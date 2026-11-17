/**
 * 방송 담당 선정·관람자 계산 (순수 로직, IO 없음). 합주실 스토어가 웹·데스크톱 모두에서 쓴다.
 * 연주 세션 상태기계(sessionLogic, 데스크톱 전용 지연 묶음)와 따로 둔다(웹 관람 묶음 분리).
 */

// 설계 6절

export interface PerformerInfo {
  id: string
  isHost: boolean
}

/**
 * 방송 담당 연주자 = 호스트가 (앱) 연주자면 호스트, 아니면 연주자 중 id 사전순 최소.
 * 연주자는 데스크톱 앱 전용이므로 연주자 = 앱 연주자.
 * 모든 연주자가 같은 입력(서버가 준 연주자 목록)으로 같은 결과를 내야 한다.
 */
export function selectBroadcaster(performers: PerformerInfo[]): string | null {
  if (performers.length === 0) return null
  const hosts = performers.filter((p) => p.isHost).map((p) => p.id).sort()
  if (hosts.length > 0) return hosts[0]
  return performers.map((p) => p.id).sort()[0]
}

/** 관람자 = 방 참여자 중 연주자가 아닌 사람 (나 제외) */
export function computeViewers(participants: Iterable<string>, performers: Iterable<string>, selfId: string | null): string[] {
  const perf = new Set(performers)
  return [...new Set(participants)].filter((id) => id !== selfId && !perf.has(id)).sort()
}

/** 방송 담당 인계 계산: 이전/현재 담당자와 내 id로 내가 할 일 */
export function broadcastTransition(
  prevBroadcaster: string | null,
  nextBroadcaster: string | null,
  selfId: string | null,
): 'start' | 'stop' | 'keep' | 'none' {
  if (!selfId) return 'none'
  const was = prevBroadcaster === selfId
  const is = nextBroadcaster === selfId
  if (!was && is) return 'start'
  if (was && !is) return 'stop'
  if (was && is) return 'keep'
  return 'none'
}
