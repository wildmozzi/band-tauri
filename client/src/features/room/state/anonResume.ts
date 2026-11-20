/**
 * 익명 관람자 세션 재개 토큰 (P2-18c).
 * 서버는 익명 auth-result에 1회용 resumeToken을 싣는다(받을 때마다 새 것, 이전 것은 무효). 시그널링이 잠깐 끊겼다 다시 붙을 때
 * `{ type: 'auth', resumeClientId, resumeToken, resumeRoomId }`를 보내면 서버가 같은 clientId로 재개한다(유예시간 안, 같은 방일 때만).
 * 재개되면 연주자 앱은 끊긴 줄 모르므로(participant-left/joined 없음) 다시 offer하지 않고, 유지해 둔 관람 수신이 이어진다.
 *
 * 토큰은 페이지 메모리에만 둔다(저장소에 쓰지 않는다, #228 리뷰 L-1). 새로고침·새 탭은 재개하지 않는다: 새 페이지에는 관람 수신 연결이
 * 없는데 재개되면 연주자 앱이 옛 연결을 그대로 두어 다시 offer하지 않으므로(소리 없음), 새 clientId로 들어가 새 offer를 받는 편이 낫다.
 * 그래서 페이지를 내릴 때는 연결을 1000으로 닫아 서버가 유예하지 않게 한다(useSignaling의 pagehide, 서버 resume.graceFor).
 * 토큰은 받은 clientId에 묶어 두고, 다른 clientId로 재개를 요청할 때는 쓰지 않는다.
 */

export interface AnonResume {
  clientId: string
  token: string
}

/**
 * 재연결 때 보낼 익명 재개 auth. 로그인 토큰이 없고, 이전 clientId와 그 clientId로 받은 토큰, 지금 방이 모두 있을 때만.
 * 아니면 null(평소 익명 auth).
 */
export function anonResumeAuth(
  resumeClientId: string | null,
  roomId: string | null,
  saved: AnonResume | null,
): { type: 'auth'; resumeClientId: string; resumeToken: string; resumeRoomId: string } | null {
  if (!resumeClientId || !roomId || !saved || saved.clientId !== resumeClientId) return null
  return { type: 'auth', resumeClientId, resumeToken: saved.token, resumeRoomId: roomId }
}
