/**
 * 방 입장 접근 제어 (P2-6): 비밀번호 방, 비공개 방(초대 링크/방 코드).
 * 비밀번호 검증과 실패 제한은 서버가 한다. 여기는 입력 해석·에러 표시·세션 내 비밀번호 기억만.
 */

export const ROOM_PASSWORD_MIN = 4
export const ROOM_PASSWORD_MAX = 64

const UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i

/** 방 코드 또는 초대 링크에서 방 id를 꺼낸다. 알아볼 수 없으면 null */
export function parseRoomCode(input: string): string | null {
  const text = input.trim()
  if (!text) return null
  const fromPath = text.match(new RegExp(`/rooms/(${UUID_RE.source})`, 'i'))
  if (fromPath) return fromPath[1].toLowerCase()
  const exact = text.match(new RegExp(`^${UUID_RE.source}$`, 'i'))
  return exact ? exact[0].toLowerCase() : null
}

/** 초대 링크. 데스크톱 앱(tauri://)에서는 웹 주소(VITE_PUBLIC_WEB_URL)를 쓴다 */
export function inviteLink(roomId: string, webBase: string | undefined, origin: string): string {
  const base = webBase || (/^https?:/.test(origin) ? origin : '')
  return base ? `${base.replace(/\/+$/, '')}/rooms/${roomId}` : roomId
}

/** 방 비밀번호 입력 검증 (서버와 같은 규칙). 문제 없으면 null */
export function validateRoomPasswordInput(password: string): string | null {
  if (password.length < ROOM_PASSWORD_MIN) return `비밀번호는 ${ROOM_PASSWORD_MIN}자 이상이어야 합니다.`
  if (password.length > ROOM_PASSWORD_MAX) return `비밀번호는 ${ROOM_PASSWORD_MAX}자 이하여야 합니다.`
  return null
}

export type JoinErrorCode = 'password-required' | 'password-invalid' | 'password-rate-limited'

export interface JoinError {
  code: JoinErrorCode
  roomId: string | null
  remainingAttempts: number | null
  retryAfterMs: number | null
}

const JOIN_ERROR_CODES = new Set<string>(['password-required', 'password-invalid', 'password-rate-limited'])

/** 서버 error 메시지 중 입장(비밀번호) 관련이면 JoinError로 */
export function joinErrorFromSignal(p: { type: string; [k: string]: unknown }): JoinError | null {
  if (p.type !== 'error' || typeof p.code !== 'string' || !JOIN_ERROR_CODES.has(p.code)) return null
  return {
    code: p.code as JoinErrorCode,
    roomId: typeof p.roomId === 'string' ? p.roomId : null,
    remainingAttempts: typeof p.remainingAttempts === 'number' ? p.remainingAttempts : null,
    retryAfterMs: typeof p.retryAfterMs === 'number' ? p.retryAfterMs : null,
  }
}

/** 사용자에게 보일 문구 */
export function describeJoinError(e: JoinError): string {
  switch (e.code) {
    case 'password-required':
      return '비밀번호가 걸린 합주실입니다. 비밀번호를 입력하세요.'
    case 'password-invalid':
      return e.remainingAttempts !== null
        ? `비밀번호가 올바르지 않습니다. (남은 시도 ${e.remainingAttempts}회)`
        : '비밀번호가 올바르지 않습니다.'
    case 'password-rate-limited': {
      const min = e.retryAfterMs !== null ? Math.max(1, Math.ceil(e.retryAfterMs / 60_000)) : null
      return min !== null
        ? `비밀번호를 너무 많이 틀렸습니다. 약 ${min}분 뒤에 다시 시도하세요.`
        : '비밀번호를 너무 많이 틀렸습니다. 잠시 후 다시 시도하세요.'
    }
  }
}

// --- 세션 동안 비밀번호 기억 (재연결 시 다시 join 하려면 필요). 탭을 닫으면 사라진다 ---

const STORAGE_PREFIX = 'band:room-pw:'

type StorageLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

function storage(): StorageLike | null {
  try {
    return typeof sessionStorage !== 'undefined' ? sessionStorage : null
  } catch {
    return null
  }
}

export function rememberRoomPassword(roomId: string, password: string, s: StorageLike | null = storage()): void {
  try {
    s?.setItem(STORAGE_PREFIX + roomId, password)
  } catch {
    // 저장 불가(사생활 보호 모드 등): 이번 연결 동안만 메모리에 있다
  }
}

export function recallRoomPassword(roomId: string, s: StorageLike | null = storage()): string | null {
  try {
    return s?.getItem(STORAGE_PREFIX + roomId) ?? null
  } catch {
    return null
  }
}

export function forgetRoomPassword(roomId: string, s: StorageLike | null = storage()): void {
  try {
    s?.removeItem(STORAGE_PREFIX + roomId)
  } catch {
    // 무시
  }
}
