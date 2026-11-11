/**
 * API 서버 주소 (끝 슬래시 없음). 서비스 모듈(music/payment/room/storage/community)이 공유한다 (P3-10g).
 * - VITE_API_URL이 있으면 그 값
 * - 없거나 'auto'면 현재 페이지 호스트의 8080 포트 (로컬 개발)
 * 호출할 때마다 읽는다(테스트의 vi.stubEnv 반영).
 */
export function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') {
    return `${window.location.protocol}//${window.location.hostname}:8080`
  }
  return envUrl.replace(/\/+$/, '')
}
