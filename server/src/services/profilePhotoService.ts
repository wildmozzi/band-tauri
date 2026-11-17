import type { SupabaseClient } from '@supabase/supabase-js'
import {
  AVATAR_MAX_BYTES,
  AVATAR_TYPES,
  LEGACY_PROFILE_PHOTO_KEY,
  PROFILE_PHOTO_URL_KEY,
  avatarViewUrl,
} from '../lib/avatarStorage'
import type { StorageAdapter } from '../storage/types'
import { PENDING_PREFIX, VerifiedKeys, verifyPostImageUpload } from './postImageService'

/**
 * 프로필 사진 (P2-12): 업로드 확인 → user_metadata에 URL만 기록 → 예전 사진 정리.
 * 신원은 requireAuth가 JWT로 확정한 userId만 쓴다(본인 폴더 key만 통과 — verifyPostImageUpload).
 */

/** auth user_metadata의 프로필 사진 기록 (서버 service_role). url=null이면 사진 없음 */
export interface ProfilePhotoMetadata {
  set(userId: string, url: string | null): Promise<void>
}

/**
 * user_metadata 갱신 내용: 새 URL + 예전 base64 키 삭제.
 * GoTrue 관리자 갱신은 user_metadata를 최상위 키 단위로 병합하고 null 값 키는 지운다 → 다른 키(닉네임·소개)는 그대로.
 */
export function profilePhotoMetadataPatch(url: string | null): Record<string, string | null> {
  return { [PROFILE_PHOTO_URL_KEY]: url, [LEGACY_PROFILE_PHOTO_KEY]: null }
}

export function supabaseProfilePhotoMetadata(sb: SupabaseClient): ProfilePhotoMetadata {
  return {
    async set(userId, url) {
      const { error } = await sb.auth.admin.updateUserById(userId, { user_metadata: profilePhotoMetadataPatch(url) })
      if (error) throw new Error(error.message)
    },
  }
}

export const AVATAR_LIMITS = { maxBytes: AVATAR_MAX_BYTES, types: AVATAR_TYPES }

export type AvatarResult =
  | { ok: true; url: string | null; removed: number }
  | { ok: false; status: number; error: string }

/** 사용자 폴더의 사진 중 keep을 뺀 나머지를 지운다 (best-effort — 실패는 로그만, 다음 업로드·삭제 때 다시 시도) */
async function removeOtherAvatars(storage: StorageAdapter, userId: string, keep: string | null): Promise<number> {
  try {
    const old = (await storage.list(`${userId}/`)).map(o => o.key).filter(k => k !== keep && !k.startsWith(PENDING_PREFIX))
    if (old.length === 0) return 0
    await storage.delete(old)
    return old.length
  } catch (e) {
    console.warn(`[Avatar] 예전 사진 정리 실패 (${userId}):`, (e as Error).message)
    return 0
  }
}

/**
 * 업로드 완료: 검증(크기·형식·매직 바이트, pending → 최종 경로) → metadata에 URL → 예전 사진 삭제.
 * metadata 기록이 실패하면 새 사진은 남지만(참조 없음) 다음 업로드·삭제 때 정리된다.
 */
export async function completeAvatarUpload(
  storage: StorageAdapter,
  meta: ProfilePhotoMetadata,
  userId: string,
  key: string,
  verified?: VerifiedKeys,
): Promise<AvatarResult> {
  const r = await verifyPostImageUpload(storage, userId, key, verified, AVATAR_LIMITS)
  if (!r.ok) return { ok: false, status: r.status, error: r.error }
  const url = avatarViewUrl(storage, key)
  await meta.set(userId, url)
  const removed = await removeOtherAvatars(storage, userId, key)
  return { ok: true, url, removed }
}

/** 사진 지우기: metadata를 비우고(예전 base64 포함) 사용자 폴더의 사진을 지운다 */
export async function removeAvatar(storage: StorageAdapter, meta: ProfilePhotoMetadata, userId: string): Promise<AvatarResult> {
  await meta.set(userId, null)
  const removed = await removeOtherAvatars(storage, userId, null)
  return { ok: true, url: null, removed }
}
