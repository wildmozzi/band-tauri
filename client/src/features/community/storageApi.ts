/**
 * 파일 저장소 API (P3-10). 게시글 이미지는 서버가 발급한 서명 PUT URL로 올리고, 서버 확인(/complete)을 거친다
 * (클라이언트가 Supabase Storage에 직접 쓰지 않는다 — 저장소는 서버 설정 STORAGE_DRIVER로 바뀐다).
 */
import { getApiUrl } from '../../shared/lib/apiUrl'
import { authHeaders } from '../../shared/lib/authHeaders'

/** 서버 상한과 같게 유지 (server/src/lib/imageStorage.ts). 서버가 최종 판단한다 */
export const POST_IMAGE_MAX_BYTES = 5 * 1024 * 1024
export const POST_IMAGE_TYPES = ['image/jpeg', 'image/png', 'image/webp', 'image/gif'] as const

/** 이미지 업로드 실패. message는 사용자에게 그대로 보일 문구(서버가 준 이유 — 용량·형식·일일 한도·로그인 등) */
export class ImageUploadError extends Error {
  constructor(message: string, readonly status: number) {
    super(message)
    this.name = 'ImageUploadError'
  }
}

const NETWORK_ERROR = '서버에 연결할 수 없습니다. 인터넷 연결을 확인하고 다시 시도해주세요.'

/** fetch가 응답 없이 실패하면(오프라인·서버 다운) 사용자 문구로 */
async function fetchOrNetworkError(fetchImpl: typeof fetch, url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetchImpl(url, init)
  } catch {
    throw new ImageUploadError(NETWORK_ERROR, 0)
  }
}

interface UploadUrlResponse {
  success: boolean
  error?: string
  key: string
  upload: { url: string; method: 'PUT'; headers: Record<string, string>; expiresAt: string }
}

/** 업로드 전 검사. 문제가 있으면 사용자에게 보일 메시지 */
export function checkPostImage(file: File): string | null {
  if (!(POST_IMAGE_TYPES as readonly string[]).includes(file.type)) {
    return `${file.name}: jpg, png, webp, gif 이미지만 올릴 수 있습니다.`
  }
  if (file.size <= 0 || file.size > POST_IMAGE_MAX_BYTES) {
    return `${file.name}: 이미지는 ${POST_IMAGE_MAX_BYTES / 1024 / 1024}MB 이하만 올릴 수 있습니다.`
  }
  return null
}

/** 서버 응답의 조회 URL을 절대 URL로 (서버가 상대 경로를 줄 수 있다 — 저장값은 항상 절대 URL) */
export function absoluteImageUrl(url: string, apiBase: string = getApiUrl()): string {
  return new URL(url, apiBase.endsWith('/') ? apiBase : `${apiBase}/`).toString()
}

/** 게시글 이미지 업로드 → 게시글에 저장할 조회 URL */
export async function uploadPostImage(file: File, fetchImpl: typeof fetch = fetch): Promise<string> {
  const problem = checkPostImage(file)
  if (problem) throw new ImageUploadError(problem, 0)

  const apiBase = getApiUrl()
  const res = await fetchOrNetworkError(fetchImpl, `${apiBase}/api/storage/post-images/upload-url`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...(await authHeaders()) },
    body: JSON.stringify({ contentType: file.type, size: file.size }),
  })
  const data = (await res.json().catch(() => ({}))) as Partial<UploadUrlResponse>
  if (!res.ok || !data.success || !data.upload || !data.key) {
    throw new ImageUploadError(data.error || `이미지 업로드 URL 발급 실패 (${res.status})`, res.status)
  }

  const put = await fetchOrNetworkError(fetchImpl, data.upload.url, { method: data.upload.method, headers: data.upload.headers, body: file })
  if (!put.ok) throw new ImageUploadError(`이미지 업로드 실패 (${put.status})`, put.status)

  // 서버가 올라간 파일의 크기·형식·내용을 확인한다 (실패하면 서버가 지움)
  const done = await fetchOrNetworkError(fetchImpl, `${apiBase}/api/storage/post-images/complete`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...(await authHeaders()) },
    body: JSON.stringify({ key: data.key }),
  })
  const doneData = (await done.json().catch(() => ({}))) as { success?: boolean; url?: string; error?: string }
  if (!done.ok || !doneData.success) {
    throw new ImageUploadError(doneData.error || `이미지 확인 실패 (${done.status})`, done.status)
  }

  if (!doneData.url) throw new ImageUploadError('이미지 확인 응답에 URL이 없습니다.', done.status)
  return absoluteImageUrl(doneData.url, apiBase)
}
