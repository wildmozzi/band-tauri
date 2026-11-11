/** VITE_*_URL 값이 없거나 'auto'일 때 쓰는 기본 서버 주소: 현재 페이지 호스트의 8080 포트 (로컬 개발) */
export function localServerUrl(): string {
  return `${window.location.protocol}//${window.location.hostname}:8080`
}

/**
 * API 서버 주소 (끝 슬래시 없음). 서비스 모듈(music/payment/room/storage/community)이 공유한다 (P3-10g).
 * - VITE_API_URL이 있으면 그 값
 * - 없거나 'auto'면 localServerUrl()
 * 호출할 때마다 읽는다(테스트의 vi.stubEnv 반영).
 */
export function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') return localServerUrl()
  return envUrl.replace(/\/+$/, '')
}

/**
 * API 서버를 설정했는지 (VITE_API_URL이 비어 있지 않음 — 'auto' 포함).
 * 설정 안 했으면 서버 조회를 건너뛰는 화면(구독 정보 등)이 쓴다. getApiUrl()은 미설정이어도 기본 주소를 돌려주므로 대신 쓸 수 없다.
 */
export function isApiUrlConfigured(): boolean {
  return Boolean(import.meta.env.VITE_API_URL)
}

/**
 * 시그널링 WebSocket 주소.
 * - VITE_SIGNALING_URL이 있으면 그 값
 * - 없거나 'auto'면 localServerUrl()의 http(s)를 ws(s)로 바꾼 값
 */
export function getSignalingUrl(): string {
  const envUrl = import.meta.env.VITE_SIGNALING_URL
  if (!envUrl || envUrl === 'auto') return localServerUrl().replace(/^http/, 'ws')
  return envUrl
}
