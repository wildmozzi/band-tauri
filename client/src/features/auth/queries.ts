/**
 * 인증 데이터 접근 + 사용자 전환 시 Query 캐시 초기화 (P3-3d).
 * AuthContext는 Supabase를 직접 부르지 않고 authApi를 쓴다.
 *
 * 캐시 초기화 규칙
 *   첫 인증 확인(로딩 끝)까지의 사용자 값은 기준점일 뿐 초기화하지 않는다.
 *   그 뒤 사용자 id가 바뀌면(로그아웃 A→없음, 로그인 없음→B, 계정 전환 A→B) 사용자별 서버 상태를 비운다:
 *     - mutation 캐시 비움
 *     - 비활성 쿼리 제거(이전 사용자의 내 방·좋아요 여부·프로필·오디오 설정 등)
 *     - 활성 쿼리는 invalidateQueries로 데이터는 둔 채 새 세션으로 백그라운드 재조회
 *       (진행 중인 조회는 취소하지 않는다 — 로그인 직후 새로 마운트된 쿼리의 이중 조회 방지)
 *   사용자와 무관한 키(USER_AGNOSTIC_KEYS: 방 상세, 공개 방 목록)는 건드리지 않는다.
 *     합주실에 있는 동안 다른 탭 로그아웃·세션 만료가 와도 라이브 화면이 로딩으로 바뀌거나 목록으로 튕기지 않게.
 *     공개 목록은 publicRooms로 남의/내 비공개 방을 모두 거르므로 사용자별 차이가 화면에 나오지 않는다.
 *   queryClient.clear()·resetQueries를 쓰지 않는 이유: clear는 마운트된 관찰자가 다시 읽지 않고,
 *   reset은 활성 화면의 데이터를 버려 로딩 상태로 되돌린다.
 *   토큰 갱신(TOKEN_REFRESHED)처럼 같은 사용자의 user 객체만 바뀌면 초기화하지 않는다.
 */
import { useEffect, useRef } from 'react'
import { useQueryClient, type QueryClient } from '@tanstack/react-query'
import type { Session, User } from '@supabase/supabase-js'
import { supabase } from '../../shared/lib/supabaseClient'

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
  const userScoped = (query: { queryKey: readonly unknown[] }) => !isUserAgnosticKey(query.queryKey)
  qc.getMutationCache().clear()
  qc.removeQueries({ type: 'inactive', predicate: userScoped })
  return qc.invalidateQueries({ type: 'active', predicate: userScoped }, { cancelRefetch: false })
}

/** 사용자와 무관해 계정이 바뀌어도 그대로 두는 쿼리 키 접두사 */
export const USER_AGNOSTIC_KEYS: readonly (readonly unknown[])[] = [
  ['rooms', 'detail'],
  ['rooms', 'list'],
]

export function isUserAgnosticKey(queryKey: readonly unknown[]): boolean {
  return USER_AGNOSTIC_KEYS.some(prefix => prefix.every((part, i) => queryKey[i] === part))
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
