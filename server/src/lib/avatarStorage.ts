import type { StorageAdapter } from '../storage/types'
import type { PostImageType } from './imageStorage'

/**
 * 프로필 사진 저장 규칙 (P2-12)
 *
 * 예전에는 클라이언트가 사진을 base64로 Supabase auth `user_metadata.profile_photo`에 넣었다 → 사진이 JWT 안에 들어가
 * 모든 API 요청·WS auth 메시지가 무거워지고, 서버 토큰 상한(getBearerToken 8KB)을 넘으면 인증 자체가 실패했다.
 *
 * 이제 사진은 논리 버킷 `avatars`(Supabase 드라이버: 같은 이름 버킷, s3: `avatars/` 접두사)에 두고,
 * `user_metadata`에는 서버가 정한 조회 URL만 `profile_photo_url`로 넣는다(서버가 service_role로 기록 — 클라이언트는
 * 이 키를 쓰지 않는다). key 형식·업로드 절차(pending → 검증 → 최종 경로)는 게시글 이미지와 같다.
 */

export const AVATAR_BUCKET = 'avatars'
export const AVATAR_ROUTE = '/api/storage/avatars'

/** 프로필 사진 상한 — 화면에 작게 보이므로 게시글(5MB)보다 작게 */
export const AVATAR_MAX_BYTES = 2 * 1024 * 1024
/** 정지 이미지만 (gif 제외) */
export const AVATAR_TYPES: readonly PostImageType[] = ['image/jpeg', 'image/png', 'image/webp']

/** user_metadata 키: 새 URL / 예전 base64(읽지 않고, 새로 올리거나 지울 때 서버가 지운다) */
export const PROFILE_PHOTO_URL_KEY = 'profile_photo_url'
export const LEGACY_PROFILE_PHOTO_KEY = 'profile_photo'

/** 조회 URL: 저장소 공개 URL이 있으면 그것, 없으면 서버 리다이렉트 경로 (PUBLIC_API_URL 없으면 상대 경로) */
export function avatarViewUrl(storage: StorageAdapter, key: string, publicApiUrl = process.env.PUBLIC_API_URL): string {
  const pub = storage.publicUrl(key)
  if (pub) return pub
  const base = (publicApiUrl ?? '').trim().replace(/\/+$/, '')
  return `${base}${AVATAR_ROUTE}/${key.split('/').map(encodeURIComponent).join('/')}`
}
