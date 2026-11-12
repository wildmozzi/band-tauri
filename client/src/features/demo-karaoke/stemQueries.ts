/**
 * 음원 분리(스템) 히스토리 서버 상태 (TanStack Query). 서버 API(/api/music/*) 경유.
 *
 * 쿼리 키
 *   ['stems', 'history', userId]  내 분리 기록
 *
 * 무효화 규칙
 *   히스토리는 주로 명시적으로 다시 읽는다: 새로고침 버튼, 분리 성공 후 무효화.
 *     스템 URL은 서버가 준 서명 URL(기본 12시간 만료, P3-10)이라 1시간 지나면 stale →
 *     다음에 화면을 열 때(마운트) 다시 읽어 만료된 URL을 쓰지 않는다 (포커스 재조회는 전역 설정으로 꺼져 있음)
 *     조회 실패는 throw로 남겨(빈 목록 성공으로 캐시하지 않음) 다음에 열 때 다시 읽는다
 *   삭제 성공 → 캐시에서 제거
 *   로그아웃 → 히스토리 캐시 제거(clearStemHistory)
 *   업로드 전 중복 검사는 fetchFreshStemHistory로 항상 서버 최신값을 읽는다.
 *     실패하면 캐시를 그대로 두고 캐시값(없으면 빈 목록)으로 검사한다
 */
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { deleteStemSeparation, fetchStemHistory, type StemSeparation } from './musicApi'

async function loadStemHistory(): Promise<StemSeparation[]> {
  try {
    return await fetchStemHistory()
  } catch (error) {
    console.error('Failed to fetch stem history:', error)
    throw error
  }
}

/** 서버 서명 URL 만료(STEM_URL_TTL_SEC 기본 12시간)보다 충분히 짧게 */
export const STEM_HISTORY_STALE_MS = 60 * 60 * 1000

export const stemKeys = {
  all: ['stems'] as const,
  history: (userId: string | null | undefined) => [...stemKeys.all, 'history', userId ?? null] as const,
}

export function useStemHistoryQuery(userId: string | null | undefined, enabled: boolean) {
  return useQuery({
    queryKey: stemKeys.history(userId),
    queryFn: loadStemHistory,
    enabled,
    staleTime: STEM_HISTORY_STALE_MS,
    retry: false, // 기존과 같이 1회만 요청(새로고침 버튼으로 재시도)
  })
}

/** 캐시와 무관하게 서버에서 다시 읽고 캐시도 갱신한다 */
export async function fetchFreshStemHistory(qc: QueryClient, userId: string | null | undefined): Promise<StemSeparation[]> {
  const key = stemKeys.history(userId)
  try {
    return await qc.fetchQuery({ queryKey: key, queryFn: loadStemHistory, staleTime: 0, retry: false })
  } catch {
    // 실패해도 캐시 데이터는 유지된다(fetchQuery는 오류 시 data를 덮지 않음)
    return qc.getQueryData<StemSeparation[]>(key) ?? []
  }
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
