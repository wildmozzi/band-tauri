import { QueryClient } from '@tanstack/react-query'

/**
 * 앱 전역 TanStack Query 클라이언트.
 * - staleTime 30초: 같은 화면을 오가도 바로 다시 조회하지 않는다(변경은 mutation이 무효화).
 * - refetchOnWindowFocus 끔: 기존 화면은 포커스 복귀 때 다시 읽지 않았다(편집 중 폼 보호).
 * - retry 1: 일시적 네트워크 오류 한 번은 재시도. mutation은 재시도하지 않는다(중복 쓰기 방지).
 *
 * 캐시 무효화 규칙은 각 features/<domain>/queries.ts 상단 주석에 적는다.
 */
export function createAppQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        refetchOnWindowFocus: false,
        retry: 1,
      },
      mutations: {
        retry: false,
      },
    },
  })
}

export const queryClient = createAppQueryClient()
