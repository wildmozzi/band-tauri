import type { ReactElement } from 'react'
import type { User } from '@supabase/supabase-js'
import { Navigate, useLocation } from 'react-router-dom'
import { AuthChecking } from '../../components/AuthChecking'
import { useAuth } from '../../contexts/useAuth'
import { safeRedirectPath } from '../../lib/redirect'

/**
 * 로그인이 필요한 페이지의 공통 가드 (P3-10f, P3-12 ⑩ 결제 수정과 같은 방식).
 *
 * - 사용자가 있으면 통과(`user`).
 * - 사용자가 없고 세션 복원(loading) 중이면 기다린다 — 복원 전(user=null)에 /auth로 보내면
 *   로그인 상태로 주소창·새로고침으로 들어왔을 때 원래 페이지에 도달하지 못한다.
 * - 복원이 끝났는데도 없으면 <Navigate>로 /auth에 보내고, 현재 경로(쿼리·해시 포함)를 state.from으로 넘긴다.
 *   Auth는 로그인 후 safeRedirectPath(from)으로 돌아온다.
 *
 * 렌더 중 navigate() 대신 `gate`(확인 중 표시 또는 <Navigate>)를 돌려준다. 페이지는 모든 훅을 부른 뒤
 * `if (gate) return gate`로 끝낸다.
 *
 * 로그아웃 중(signOut)에는 user가 남아 있는 동안 통과, user가 비면 확인 중 → /auth 순서가 된다.
 */
export type RequireAuthResult =
  | { user: User; gate: null }
  | { user: null; gate: ReactElement }

export function useRequireAuth(): RequireAuthResult {
  const { user, loading } = useAuth()
  const location = useLocation()

  if (user) return { user, gate: null }
  if (loading) return { user: null, gate: <AuthChecking /> }
  const from = safeRedirectPath(`${location.pathname}${location.search}${location.hash}`, '/')
  return { user: null, gate: <Navigate to="/auth" replace state={{ from }} /> }
}
