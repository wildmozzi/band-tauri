/**
 * 예전 base64 프로필 사진 정리 (P2-12 리뷰 M2).
 *
 * 예전 클라이언트는 사진을 base64로 `user_metadata.profile_photo`에 넣었다. 사진이 크면 JWT가 서버 토큰 상한(8KB)을 넘어
 * 우리 서버 API가 모두 401이 된다 → 새 업로드 경로(/api/storage/avatars)로도 지울 수 없다.
 * 그래서 로그인·세션 복원 때 클라이언트가 Supabase(GoTrue)에 직접 `profile_photo: null`을 쓴다 — 우리 서버를 거치지 않고,
 * 사용자 자신의 user_metadata라 허용된다(GoTrue는 null 키를 지운다). 그 뒤 새 토큰을 받는다.
 * 새 클라이언트는 이 키를 읽지도 쓰지도 않으므로 값이 있으면 언제나 지운다.
 */
import { useEffect, useRef } from 'react'
import type { User } from '@supabase/supabase-js'
import { profileApi } from './profileQueries'

export const LEGACY_PROFILE_PHOTO_KEY = 'profile_photo'

export function hasLegacyProfilePhoto(meta: Record<string, unknown> | null | undefined): boolean {
  return meta?.[LEGACY_PROFILE_PHOTO_KEY] !== undefined && meta?.[LEGACY_PROFILE_PHOTO_KEY] !== null
}

/** user_metadata에서 예전 사진 키를 지우고 새 토큰을 받는다. 실패는 로그만(다음 로그인 때 다시) */
export async function clearLegacyProfilePhoto(): Promise<void> {
  await profileApi.updateUserMetadata({ [LEGACY_PROFILE_PHOTO_KEY]: null })
  await profileApi.refreshSession()
}

/** 로그인한 사용자에게 예전 사진이 있으면 사용자마다 한 번 정리한다 */
export function useLegacyProfilePhotoCleanup(user: User | null) {
  const tried = useRef(new Set<string>())
  const userId = user?.id
  const legacy = hasLegacyProfilePhoto(user?.user_metadata)
  useEffect(() => {
    if (!userId || !legacy || tried.current.has(userId)) return
    tried.current.add(userId)
    clearLegacyProfilePhoto().catch(err => console.warn('[auth] 예전 프로필 사진 정리 실패 — 다음 로그인 때 다시 시도:', err))
  }, [userId, legacy])
}
