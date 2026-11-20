import type { QueryClient, QueryKey } from '@tanstack/react-query'

/**
 * queryKey에 맞는 쿼리들을, 그 쿼리를 보는 화면(observer)이 모두 떠난 뒤 캐시에서 지운다.
 * 지금 아무도 안 보는 쿼리는 바로 지운다.
 *
 * 삭제한 글·방의 상세 캐시를 지울 때 쓴다. 상세 화면이 아직 마운트된 채로 removeQueries를 부르면,
 * 지우기 전에 그려 둔 렌더의 effect(useQuery의 setOptions)가 뒤늦게 돌면서 같은 키로 새 쿼리를 만들고
 * 곧바로 조회한다 — 이미 지운 행이라 PostgREST .single()이 406을 낸다(P3-21 발견). navigate 직후에도
 * 화면은 바로 내려가지 않으므로(전환 렌더·지연 로딩) "navigate 뒤에 지우기"로는 막히지 않는다.
 */
export function removeQueriesWhenUnobserved(qc: QueryClient, queryKey: QueryKey): void {
  const cache = qc.getQueryCache()
  const pending = new Set(cache.findAll({ queryKey }))

  const removeIdle = (): boolean => {
    for (const query of pending) {
      if (query.getObserversCount() === 0) {
        pending.delete(query)
        cache.remove(query)
      }
    }
    return pending.size === 0
  }

  if (removeIdle()) return
  const unsubscribe = cache.subscribe(event => {
    if (event.type !== 'observerRemoved' || !pending.has(event.query)) return
    if (removeIdle()) unsubscribe()
  })
}
