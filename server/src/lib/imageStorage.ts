import { randomUUID } from 'crypto'
import type { StorageAdapter } from '../storage/types'

/**
 * 게시글 이미지 저장 규칙 (P3-10)
 *
 * 클라이언트는 Supabase에 직접 올리지 않는다. 서버가 key(사용자별 경로)를 정하고 서명 PUT URL을 준다.
 * key: `${userId}/${타임스탬프}-${uuid}.${확장자}` — 확장자는 MIME으로 서버가 정한다.
 * 조회 URL: 저장소 공개 URL(Supabase 공개 버킷·S3_PUBLIC_BASE_URL) 또는
 *          `${PUBLIC_API_URL}/api/storage/post-images/<key>` (서명 GET URL로 302).
 * 이전 Supabase 공개 URL(posts.images)은 그대로 표시된다 (클라이언트가 절대 URL을 그대로 쓴다).
 */

export const POST_IMAGE_MAX_BYTES = 5 * 1024 * 1024

export const POST_IMAGE_TYPES = {
  'image/jpeg': 'jpg',
  'image/png': 'png',
  'image/webp': 'webp',
  'image/gif': 'gif',
} as const
export type PostImageType = keyof typeof POST_IMAGE_TYPES

/** 서명 PUT URL 유효기간 (S3). Supabase는 고정 2시간 */
export const POST_IMAGE_UPLOAD_TTL_SEC = 10 * 60
/** 조회 리다이렉트가 주는 서명 GET URL 유효기간 */
export const POST_IMAGE_VIEW_TTL_SEC = 60 * 60

export const POST_IMAGE_ROUTE = '/api/storage/post-images'

const OWNER_RE = /^[A-Za-z0-9-]{1,64}$/
/** 조회 허용 파일명: 신규(서버 생성)와 이전 클라이언트 생성 이름(`${ms}-${random}.${원본 확장자}`) 모두 */
const FILE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/

export function isPostImageOwner(userId: string): boolean {
  return OWNER_RE.test(userId)
}

export function newPostImageKey(userId: string, type: PostImageType, now: Date = new Date()): string {
  if (!isPostImageOwner(userId)) throw new Error('invalid owner id')
  return `${userId}/${now.getTime()}-${randomUUID()}.${POST_IMAGE_TYPES[type]}`
}

/** 조회 경로 파라미터 → key. 형식이 아니면 null (경로 순회·다른 접두사 차단) */
export function postImageKeyFromParams(owner: unknown, file: unknown): string | null {
  if (typeof owner !== 'string' || typeof file !== 'string') return null
  if (!OWNER_RE.test(owner) || !FILE_RE.test(file) || file.includes('..')) return null
  return `${owner}/${file}`
}

/** 이미지 조회 URL: 공개 URL이 있으면 그것, 없으면 서버 리다이렉트 경로 (PUBLIC_API_URL 없으면 상대 경로) */
export function postImageViewUrl(storage: StorageAdapter, key: string, publicApiUrl = process.env.PUBLIC_API_URL): string {
  const pub = storage.publicUrl(key)
  if (pub) return pub
  const base = (publicApiUrl ?? '').trim().replace(/\/+$/, '')
  return `${base}${POST_IMAGE_ROUTE}/${key.split('/').map(encodeURIComponent).join('/')}`
}
