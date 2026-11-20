/**
 * 로그인 화면이 돌아갈 경로. 같은 출처의 앱 안 경로만 허용하고, 그 밖은 fallback으로 바꾼다.
 * - 문자열 검사만으로는 '/\t/evil.example'처럼 브라우저 URL 파서가 지우는 제어 문자로 우회될 수 있어
 *   (1) 허용 문자 화이트리스트(경로·쿼리·해시에 쓰는 출력 가능 ASCII, 백슬래시 제외)로 먼저 거르고
 *   (2) URL로 파싱해 출처가 현재 출처와 같을 때만 pathname+search+hash를 돌려준다.
 */
const ALLOWED = /^\/[A-Za-z0-9\-._~!$&'()*+,;=:@/?#%]*$/

export function safeRedirectPath(
  from: unknown,
  fallback = '/rooms',
  origin: string = typeof window !== 'undefined' ? window.location.origin : 'http://localhost',
): string {
  if (typeof from !== 'string') return fallback
  if (!ALLOWED.test(from) || from.startsWith('//')) return fallback
  let url: URL
  try {
    url = new URL(from, origin)
  } catch {
    return fallback
  }
  if (url.origin !== new URL(origin).origin) return fallback
  return url.pathname + url.search + url.hash
}
