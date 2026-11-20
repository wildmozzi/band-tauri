/**
 * 파일 업로드 실패 (게시글 이미지·프로필 사진 공통). message는 사용자에게 그대로 보일 문구.
 * kind로 원인을 구분한다(#197 리뷰 L-3 — 전에는 클라이언트 검사 실패와 네트워크 실패가 같은 status 0이었다):
 * - invalid: 보내기 전 클라이언트 검사(형식·용량)에 걸림
 * - network: 응답을 받지 못함(오프라인·서버 다운)
 * - server: 서버·저장소가 거절함(status에 HTTP 상태)
 */
export type UploadErrorKind = 'invalid' | 'network' | 'server'

export class UploadError extends Error {
  constructor(message: string, readonly kind: UploadErrorKind, readonly status: number | null = null) {
    super(message)
    this.name = 'UploadError'
  }
}

export const UPLOAD_NETWORK_ERROR = '서버에 연결할 수 없습니다. 인터넷 연결을 확인하고 다시 시도해주세요.'

/** fetch가 응답 없이 실패하면(오프라인·서버 다운) 사용자 문구의 UploadError로 — 아니면 화면에 "Failed to fetch"가 그대로 뜬다 */
export async function fetchForUpload(fetchImpl: typeof fetch, url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetchImpl(url, init)
  } catch {
    throw new UploadError(UPLOAD_NETWORK_ERROR, 'network')
  }
}
