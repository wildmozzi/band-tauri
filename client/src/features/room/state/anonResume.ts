/**
 * 익명 관람자 세션 재개 토큰 (P2-18c).
 * 서버는 익명 auth-result에 1회용 resumeToken을 싣는다(받을 때마다 새 것, 이전 것은 무효). 시그널링이 잠깐 끊겼다 다시 붙을 때
 * `{ type: 'auth', resumeClientId, resumeToken, resumeRoomId }`를 보내면 서버가 같은 clientId로 재개한다(유예시간 안, 같은 방일 때만).
 * 재개되면 연주자 앱은 끊긴 줄 모르므로(participant-left/joined 없음) 다시 offer하지 않고, 유지해 둔 관람 수신이 이어진다.
 *
 * 저장은 sessionStorage(탭 단위) — 다른 탭과 섞이지 않고 탭을 닫으면 사라진다. 저장소를 못 쓰면(사생활 보호 모드 등)
 * 메모리 값만 쓴다. 토큰은 받은 clientId에 묶어 두고, 다른 clientId로 재개를 요청할 때는 쓰지 않는다.
 */

export interface AnonResume {
  clientId: string
  token: string
}

const STORAGE_KEY = 'band.signaling.anonResume'

function storage(): Storage | null {
  try {
    return typeof sessionStorage !== 'undefined' ? sessionStorage : null
  } catch {
    return null
  }
}

export function loadAnonResume(): AnonResume | null {
  try {
    const raw = storage()?.getItem(STORAGE_KEY)
    if (!raw) return null
    const v: unknown = JSON.parse(raw)
    if (v && typeof v === 'object' && typeof (v as AnonResume).clientId === 'string' && typeof (v as AnonResume).token === 'string') {
      return { clientId: (v as AnonResume).clientId, token: (v as AnonResume).token }
    }
  } catch {
    // 깨진 값·저장소 접근 실패: 없는 것으로 본다
  }
  return null
}

/** null이면 지운다(로그인 사용자로 판정됨, 새 입장) */
export function saveAnonResume(v: AnonResume | null): void {
  try {
    const s = storage()
    if (!s) return
    if (v) s.setItem(STORAGE_KEY, JSON.stringify(v))
    else s.removeItem(STORAGE_KEY)
  } catch {
    // 저장 실패는 무시 (메모리 값으로 계속)
  }
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
