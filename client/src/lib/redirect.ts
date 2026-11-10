/**
 * 로그인 화면이 돌아갈 경로. 앱 안 경로('/...')만 허용하고, 외부 주소나 '//host'(프로토콜 상대 URL)는 홈으로 바꾼다.
 */
export function safeRedirectPath(from: unknown, fallback = '/'): string {
  if (typeof from !== 'string') return fallback
  if (!from.startsWith('/') || from.startsWith('//') || from.startsWith('/\\')) return fallback
  return from
}
