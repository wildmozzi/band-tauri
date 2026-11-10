/**
 * 인증 데이터 접근 + 사용자 전환 시 Query 캐시 초기화 (P3-3d).
 * AuthContext는 Supabase를 직접 부르지 않고 authApi를 쓴다.
 *
 * 캐시 초기화 규칙
 *   첫 인증 확인(로딩 끝)까지의 사용자 값은 기준점일 뿐 초기화하지 않는다.
 *   그 뒤 사용자 id가 바뀌면(로그아웃 A→없음, 로그인 없음→B, 계정 전환 A→B) 사용자별 서버 상태를 비운다:
 *     - mutation 캐시 비움
 *     - 비활성 쿼리 제거(이전 사용자의 내 방·좋아요 여부·프로필·오디오 설정 등)
 *     - 활성 쿼리는 resetQueries로 데이터를 버리고 새 세션으로 다시 읽음
 *   queryClient.clear()를 쓰지 않는 이유: 마운트된 useQuery 관찰자가 지워진 쿼리를 다시 읽지 않아
 *   화면이 다음 렌더까지 빈 채로 남는다. (RLS 결과가 사용자마다 다른 키 없는 쿼리 — 방 목록 등 — 도 함께 초기화된다)
 *   토큰 갱신(TOKEN_REFRESHED)처럼 같은 사용자의 user 객체만 바뀌면 초기화하지 않는다.
 */
import { useEffect, useRef } from 'react'
import { useQueryClient, type QueryClient } from '@tanstack/react-query'
import type { Session, User } from '@supabase/supabase-js'
import { supabase } from '../../lib/supabaseClient'

function requireClient() {
  if (!supabase) throw new Error('Supabase not configured')
  return supabase
}

export interface SignUpMetadata {
  name: string
  nickname: string
  birth_date: string
  gender?: string
  phone: string | null
}

export const authApi = {
  /** 현재 사용자 (Supabase 미설정이면 null) */
  async getUser(): Promise<User | null> {
    if (!supabase) return null
    const { data } = await supabase.auth.getUser()
    return data.user ?? null
  },

  /** 인증 상태 변경 구독. 반환값을 호출하면 해제 */
  onAuthStateChange(cb: (session: Session | null) => void): () => void {
    if (!supabase) return () => {}
    const { data: listener } = supabase.auth.onAuthStateChange((_event, session) => cb(session))
    return () => listener?.subscription.unsubscribe()
  },

  /** 가입 (메타데이터에 프로필 정보 포함). 만들어진 사용자, 없으면 오류 */
  async signUp(email: string, password: string, metadata: SignUpMetadata): Promise<User> {
    const { data, error } = await requireClient().auth.signUp({ email, password, options: { data: { ...metadata } } })
    if (error) throw error
    if (!data.user) throw new Error('회원가입에 실패했습니다.')
    return data.user
  },

  async signIn(email: string, password: string): Promise<void> {
    const { error } = await requireClient().auth.signInWithPassword({ email, password })
    if (error) throw error
  },

  async signOut(): Promise<void> {
    const { error } = await requireClient().auth.signOut()
    if (error) throw error
  },
}

/**
 * 이전 → 다음 사용자 id에서 캐시를 비워야 하는가.
 * prev === undefined: 아직 첫 인증 확인 전(기준점 없음) → 비우지 않는다.
 */
export function shouldResetUserCache(prev: string | null | undefined, next: string | null): boolean {
  return prev !== undefined && prev !== next
}

/** 사용자별 서버 상태 초기화 (규칙은 파일 상단) */
export function resetUserScopedCache(qc: QueryClient): Promise<void> {
  qc.getMutationCache().clear()
  qc.removeQueries({ type: 'inactive' })
  return qc.resetQueries({ type: 'active' })
}

/** AuthProvider에서 호출: 사용자 id가 바뀌면 Query 캐시 초기화 */
export function useResetQueryCacheOnUserChange(userId: string | null, authLoading: boolean) {
  const qc = useQueryClient()
  const prevRef = useRef<string | null | undefined>(undefined)

  useEffect(() => {
    if (prevRef.current === undefined) {
      // 첫 인증 확인이 끝나면 그 사용자를 기준점으로 삼는다
      if (!authLoading) prevRef.current = userId
      return
    }
    if (shouldResetUserCache(prevRef.current, userId)) {
      void resetUserScopedCache(qc).catch(err => console.warn('[auth] 쿼리 캐시 초기화 실패:', err))
    }
    prevRef.current = userId
  }, [userId, authLoading, qc])
}
