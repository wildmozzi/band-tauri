import { PasswordAttemptLimiter, ScryptBusyError, ipBucket, verifyRoomPassword, type ScryptGate } from '../lib/roomPassword'
import { send } from './registry'
import type { WebSocket } from 'ws'
import type { Client, WsLimits } from './types'

/**
 * 방 비밀번호 검증 (설계 docs/server-architecture.md 2.1절 E, 계층 2).
 * lib/roomPassword의 실패 횟수 제한·scrypt 게이트를 WS 에러 응답으로 잇는 얇은 층. passwordLimiter의 소유자.
 */
export function createPassword(limits: WsLimits, scryptGate: ScryptGate) {
  const passwordLimiter = new PasswordAttemptLimiter({
    maxFailures: limits.passwordMaxFailures,
    maxFailuresPerIp: limits.passwordMaxFailuresPerIp,
    maxFailuresPerRoom: limits.passwordMaxFailuresPerRoom,
    windowMs: limits.passwordWindowMs,
  })

  /** 방 비밀번호 검증 + 실패 횟수 제한. 통과하면 true, 아니면 에러를 보내고 false */
  // ws는 연결 클로저가 캡처하던 이 연결의 소켓을 호출부에서 그대로 받는다. client.ws는 resume 뒤 새 연결의 소켓으로
  // 바뀔 수 있으므로(끊긴 연결의 대기 중 join 응답이 새 소켓으로 가면 안 됨) 대신 쓰지 않는다.
  const checkRoomPassword = async (ws: WebSocket, client: Client, roomId: string, hash: string, password: unknown): Promise<boolean> => {
    const id = client.id
    if (typeof password !== 'string' || password.length === 0) {
      send(ws, { type: 'error', code: 'password-required', roomId, message: '비밀번호가 필요한 방입니다.' })
      return false
    }
    // 익명은 IP(/64) 단위로 묶는다 (재접속·주소 회전으로 횟수를 초기화하지 못하게)
    const subject = client.userId ? `u:${client.userId}` : `anon:${ipBucket(client.ip)}`
    const attempt = passwordLimiter.begin(roomId, subject, client.ip)
    if (!attempt.ok) {
      send(ws, { type: 'error', code: 'password-rate-limited', roomId, retryAfterMs: attempt.retryAfterMs, message: '비밀번호를 너무 많이 틀렸습니다. 잠시 후 다시 시도하세요.' })
      return false
    }
    let valid: boolean
    try {
      valid = await verifyRoomPassword(password, hash, scryptGate)
    } catch (err) {
      if (!(err instanceof ScryptBusyError)) throw err
      passwordLimiter.cancelled(roomId, subject, client.ip)
      send(ws, { type: 'error', code: 'password-busy', roomId, retryAfterMs: 2000, message: '서버가 혼잡합니다. 잠시 후 다시 시도하세요.' })
      return false
    }
    if (valid) {
      passwordLimiter.succeeded(roomId, subject, client.ip)
      return true
    }
    console.warn(`[JOIN] ${id.slice(0, 8)} wrong password for room ${roomId.slice(0, 8)} (remaining ${attempt.remaining})`)
    send(ws, { type: 'error', code: 'password-invalid', roomId, remainingAttempts: attempt.remaining, message: '비밀번호가 올바르지 않습니다.' })
    return false
  }

  return { checkRoomPassword }
}

export type Password = ReturnType<typeof createPassword>
