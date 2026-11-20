/**
 * 프로필 사진 업로드 (P2-12). 서버가 발급한 서명 PUT URL로 올리고 서버 확인(/complete)을 거친다.
 * 서버가 확인 뒤 auth user_metadata.profile_photo_url에 조회 URL만 기록한다(예전 base64 키는 지움) —
 * 클라이언트는 사진을 metadata에 넣지 않는다(JWT가 커져 모든 요청·WS 인증이 무거워지고 서버 토큰 상한을 넘었다).
 */
import { getApiUrl } from '../../shared/lib/apiUrl'
import { authHeaders } from '../../shared/lib/authHeaders'
import { fetchForUpload, UploadError } from '../../shared/lib/uploadError'

/** 서버 상한과 같게 유지 (server/src/lib/avatarStorage.ts). 서버가 최종 판단한다 */
export const AVATAR_MAX_BYTES = 2 * 1024 * 1024
export const AVATAR_TYPES = ['image/jpeg', 'image/png', 'image/webp'] as const

/** 업로드 전 검사. 문제가 있으면 사용자에게 보일 메시지 */
export function checkAvatar(file: File): string | null {
  if (!(AVATAR_TYPES as readonly string[]).includes(file.type)) return '프로필 사진은 jpg, png, webp 이미지만 올릴 수 있습니다.'
  if (file.size <= 0 || file.size > AVATAR_MAX_BYTES) return `프로필 사진은 ${AVATAR_MAX_BYTES / 1024 / 1024}MB 이하만 올릴 수 있습니다.`
  return null
}

async function postJson(fetchImpl: typeof fetch, url: string, body: unknown) {
  const res = await fetchForUpload(fetchImpl, url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...(await authHeaders()) },
    body: JSON.stringify(body),
  })
  const data = (await res.json().catch(() => ({}))) as Record<string, unknown>
  return { res, data }
}

/** 업로드 → 서버가 기록한 조회 URL(절대 URL) */
export async function uploadAvatar(file: File, fetchImpl: typeof fetch = fetch): Promise<string> {
  const problem = checkAvatar(file)
  if (problem) throw new UploadError(problem, 'invalid')

  const apiBase = getApiUrl()
  const issued = await postJson(fetchImpl, `${apiBase}/api/storage/avatars/upload-url`, { contentType: file.type, size: file.size })
  const upload = issued.data.upload as { url: string; method: 'PUT'; headers: Record<string, string> } | undefined
  if (!issued.res.ok || !issued.data.success || !upload || typeof issued.data.key !== 'string') {
    throw new UploadError((issued.data.error as string) || `업로드 URL 발급 실패 (${issued.res.status})`, 'server', issued.res.status)
  }

  const put = await fetchForUpload(fetchImpl, upload.url, { method: upload.method, headers: upload.headers, body: file })
  if (!put.ok) throw new UploadError(`사진 업로드 실패 (${put.status})`, 'server', put.status)

  const done = await postJson(fetchImpl, `${apiBase}/api/storage/avatars/complete`, { key: issued.data.key })
  if (!done.res.ok || !done.data.success || typeof done.data.url !== 'string') {
    throw new UploadError((done.data.error as string) || `사진 확인 실패 (${done.res.status})`, 'server', done.res.status)
  }
  return new URL(done.data.url, apiBase.endsWith('/') ? apiBase : `${apiBase}/`).toString()
}
