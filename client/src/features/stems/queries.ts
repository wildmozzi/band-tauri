/**
 * 음원 분리(스템) 히스토리 서버 상태 (TanStack Query). 서버 API(/api/music/*) 경유.
 *
 * 쿼리 키
 *   ['stems', 'history', userId]  내 분리 기록
 *
 * 무효화 규칙
 *   히스토리는 명시적으로만 다시 읽는다(staleTime Infinity): 새로고침 버튼, 분리 성공 후 무효화
 *   삭제 성공 → 캐시에서 제거
 *   로그아웃 → 히스토리 캐시 제거(clearStemHistory)
 *   업로드 전 중복 검사는 fetchFreshStemHistory로 항상 서버 최신값을 읽는다
 */
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { deleteStemSeparation, getStemHistory, type StemSeparation } from '../../services/musicApi'

export const stemKeys = {
  all: ['stems'] as const,
  history: (userId: string | null | undefined) => [...stemKeys.all, 'history', userId ?? null] as const,
}

export function useStemHistoryQuery(userId: string | null | undefined, enabled: boolean) {
  return useQuery({
    queryKey: stemKeys.history(userId),
    queryFn: getStemHistory,
    enabled,
    staleTime: Infinity,
  })
}

/** 캐시와 무관하게 서버에서 다시 읽고 캐시도 갱신한다 */
export function fetchFreshStemHistory(qc: QueryClient, userId: string | null | undefined): Promise<StemSeparation[]> {
  return qc.fetchQuery({ queryKey: stemKeys.history(userId), queryFn: getStemHistory, staleTime: 0 })
}

export function invalidateStemHistory(qc: QueryClient, userId: string | null | undefined) {
  return qc.invalidateQueries({ queryKey: stemKeys.history(userId) })
}

export function clearStemHistory(qc: QueryClient) {
  qc.removeQueries({ queryKey: [...stemKeys.all, 'history'] })
}

/** 삭제. 서버가 실패하면 false (getStemHistory/deleteStemSeparation은 오류를 삼키고 값으로 돌려준다) */
export function useDeleteStemSeparationMutation(userId: string | null | undefined) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (separationId: string) => deleteStemSeparation(separationId),
    onSuccess: (success, separationId) => {
      if (!success) return
      qc.setQueryData<StemSeparation[]>(stemKeys.history(userId), prev => prev?.filter(s => s.id !== separationId))
    },
  })
}
