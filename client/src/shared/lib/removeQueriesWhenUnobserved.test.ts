import { QueryClient, QueryObserver } from '@tanstack/react-query'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { removeQueriesWhenUnobserved } from './removeQueriesWhenUnobserved'

const clients: QueryClient[] = []
function client() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  clients.push(qc)
  return qc
}
afterEach(() => {
  for (const qc of clients.splice(0)) qc.clear()
})

describe('removeQueriesWhenUnobserved', () => {
  it('아무도 안 보는 쿼리는 바로 지우고, 다른 키는 둔다', () => {
    const qc = client()
    qc.setQueryData(['post', 'A', { userId: null }], 1)
    qc.setQueryData(['post', 'A', { userId: 'u' }], 2)
    qc.setQueryData(['post', 'B'], 3)

    removeQueriesWhenUnobserved(qc, ['post', 'A'])

    expect(qc.getQueryCache().findAll({ queryKey: ['post', 'A'] })).toHaveLength(0)
    expect(qc.getQueryData(['post', 'B'])).toBe(3)
  })

  it('보는 화면이 있으면 그 화면이 떠난 뒤에 지운다 — 그사이 다시 조회하지 않는다', async () => {
    const qc = client()
    const queryFn = vi.fn().mockResolvedValue('post')
    const observer = new QueryObserver(qc, { queryKey: ['post', 'A'], queryFn, staleTime: Infinity })
    const unsubscribe = observer.subscribe(() => {})
    await vi.waitFor(() => expect(qc.getQueryData(['post', 'A'])).toBe('post'))

    removeQueriesWhenUnobserved(qc, ['post', 'A'])
    // 아직 보고 있다: 캐시가 남아 있어 옵션이 다시 적용돼도(지우기 전 렌더의 effect) 새 쿼리를 만들어 조회하지 않는다
    expect(qc.getQueryData(['post', 'A'])).toBe('post')
    observer.setOptions({ queryKey: ['post', 'A'], queryFn, staleTime: Infinity })
    expect(queryFn).toHaveBeenCalledTimes(1)

    unsubscribe()
    expect(qc.getQueryCache().find({ queryKey: ['post', 'A'] })).toBeUndefined()
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('비교: 보는 중에 removeQueries로 지우면 같은 옵션 재적용만으로 다시 조회한다 (고친 버그)', async () => {
    const qc = client()
    const queryFn = vi.fn().mockResolvedValue('post')
    const observer = new QueryObserver(qc, { queryKey: ['post', 'A'], queryFn, staleTime: Infinity })
    const unsubscribe = observer.subscribe(() => {})
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1))

    qc.removeQueries({ queryKey: ['post', 'A'] })
    observer.setOptions({ queryKey: ['post', 'A'], queryFn, staleTime: Infinity })
    expect(queryFn).toHaveBeenCalledTimes(2)
    unsubscribe()
  })
})
