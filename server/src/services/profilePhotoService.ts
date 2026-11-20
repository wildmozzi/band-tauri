import type { SupabaseClient } from '@supabase/supabase-js'
import {
  AVATAR_MAX_BYTES,
  AVATAR_TYPES,
  LEGACY_PROFILE_PHOTO_KEY,
  PROFILE_PHOTO_URL_KEY,
  avatarViewUrl,
} from '../lib/avatarStorage'
import type { StorageAdapter } from '../storage/types'
import { PENDING_PREFIX, VerifiedKeys, uploadedAtOfKey, verifyPostImageUpload } from './postImageService'

/**
 * 프로필 사진 (P2-12): 업로드 확인 → app_metadata에 URL만 기록(user_metadata의 예전 base64는 지움) → 예전 사진 정리.
 * 신원은 requireAuth가 JWT로 확정한 userId만 쓴다(본인 폴더 key만 통과 — verifyPostImageUpload).
 */

/** 프로필 사진 기록 (auth app_metadata — 서버 service_role만 쓸 수 있음). url=null이면 사진 없음 */
export interface ProfilePhotoMetadata {
  set(userId: string, url: string | null): Promise<void>
}

/**
 * 관리자 갱신 내용: app_metadata에 새 URL(리뷰 M1 — 사용자가 덮어쓸 수 없는 곳) + user_metadata의 예전 base64 키 삭제.
 * GoTrue 관리자 갱신은 두 metadata를 최상위 키 단위로 병합하고 null 값 키는 지운다 → 다른 키(닉네임·소개·provider)는 그대로.
 */
export function profilePhotoMetadataPatch(url: string | null): {
  app_metadata: Record<string, string | null>
  user_metadata: Record<string, null>
} {
  return { app_metadata: { [PROFILE_PHOTO_URL_KEY]: url }, user_metadata: { [LEGACY_PROFILE_PHOTO_KEY]: null } }
}

export function supabaseProfilePhotoMetadata(sb: SupabaseClient): ProfilePhotoMetadata {
  return {
    async set(userId, url) {
      const { error } = await sb.auth.admin.updateUserById(userId, profilePhotoMetadataPatch(url))
      if (error) throw new Error(error.message)
    },
  }
}

export const AVATAR_LIMITS = { maxBytes: AVATAR_MAX_BYTES, types: AVATAR_TYPES }

export type AvatarResult =
  | { ok: true; url: string | null; removed: number }
  | { ok: false; status: number; error: string }

/** key 순서: 서버가 key에 넣은 업로드 시각(ms), 같으면 key 문자열 */
function compareAvatarKeys(a: string, b: string): number {
  const d = (uploadedAtOfKey(a) ?? 0) - (uploadedAtOfKey(b) ?? 0)
  return d !== 0 ? d : a < b ? -1 : a > b ? 1 : 0
}

/** 사용자 폴더의 최종 경로 사진 key (pending 제외) */
async function listFinalAvatars(storage: StorageAdapter, userId: string): Promise<string[]> {
  return (await storage.list(`${userId}/`)).map(o => o.key).filter(k => !k.startsWith(PENDING_PREFIX))
}

/**
 * 사용자 폴더의 사진을 지운다 (best-effort — 실패는 로그만, 다음 업로드·삭제 때 다시 시도).
 * keep이 있으면 **keep보다 먼저 올라온 것만** 지운다(리뷰 L1: 동시에 끝난 더 새 업로드를 지우지 않게).
 */
async function removeOlderAvatars(storage: StorageAdapter, userId: string, keep: string | null): Promise<number> {
  try {
    const old = (await listFinalAvatars(storage, userId)).filter(k => keep === null || compareAvatarKeys(k, keep) < 0)
    if (old.length === 0) return 0
    await storage.delete(old)
    return old.length
  } catch (e) {
    console.warn(`[Avatar] 예전 사진 정리 실패 (${userId}):`, (e as Error).message)
    return 0
  }
}

/**
 * 업로드 완료: 검증(크기·형식·매직 바이트, pending → 최종 경로) → 기록(app_metadata URL) → 더 오래된 사진 삭제.
 * 기록이 실패하면 새 사진은 남지만(참조 없음) 다음 업로드·삭제 때 정리된다.
 *
 * 동시·재호출(리뷰 L1):
 *  - 검증 캐시(VerifiedKeys)를 쓰지 않는다 — 재호출이면 최종 경로를 HEAD로 다시 본다(이미 지워진 사진이면 404).
 *  - 같은 사용자 폴더에 이 key보다 새 사진이 이미 있으면 기록하지 않고 409 (예전 업로드가 새 사진을 덮지 않게).
 *  - 정리는 이 key보다 오래된 것만 → 겹친 두 complete가 서로의 사진을 지우지 않는다.
 *  남는 경합: 두 complete가 "새 사진 확인"과 "기록" 사이에서 정확히 엇갈리면 기록이 오래된 쪽일 수 있다(사진은 둘 다 남아 깨지지 않음,
 *  다음 업로드 때 정리). 사용자 한 명이 동시에 두 번 올리는 경우라 이 이상은 막지 않는다.
 */
export async function completeAvatarUpload(
  storage: StorageAdapter,
  meta: ProfilePhotoMetadata,
  userId: string,
  key: string,
): Promise<AvatarResult> {
  const r = await verifyPostImageUpload(storage, userId, key, new VerifiedKeys(), AVATAR_LIMITS)
  if (!r.ok) return { ok: false, status: r.status, error: r.error }
  if ((await listFinalAvatars(storage, userId)).some(k => compareAvatarKeys(k, key) > 0)) {
    return { ok: false, status: 409, error: '더 최근에 올린 프로필 사진이 있습니다.' }
  }
  const url = avatarViewUrl(storage, key)
  await meta.set(userId, url)
  const removed = await removeOlderAvatars(storage, userId, key)
  return { ok: true, url, removed }
}

/** 사진 지우기: 기록을 비우고(예전 base64 포함) 사용자 폴더의 사진을 지운다 */
export async function removeAvatar(storage: StorageAdapter, meta: ProfilePhotoMetadata, userId: string): Promise<AvatarResult> {
  await meta.set(userId, null)
  const removed = await removeOlderAvatars(storage, userId, null)
  return { ok: true, url: null, removed }
}
