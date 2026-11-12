import type { Ctx } from './ctx'
import { TOKEN_EXPIRY_GRACE_MS, isNonEmptyString, type Msg } from './protocol'
import { isAuthenticated, send } from './registry'
import { announceResumed, attachSession, type ResumeSvc } from './resume'
import type { Client, WsDeps } from './types'

/**
 * 인증 (설계 docs/server-architecture.md 2.1절 B, 계층 3 — 계층 안 순서 membership < resume < auth).
 * `auth` 메시지·재인증·토큰 만료 타이머. 세션 재개는 resume 모듈을 부른다(허용 간선 auth → resume, S4-1).
 */

/** JWT의 exp(초)를 ms로. 검증은 하지 않는다(verifyToken 통과 후에만 사용) */
export function jwtExpiryMs(token: string): number | null {
  const parts = token.split('.')
  if (parts.length !== 3) return null
  try {
    const payload = JSON.parse(Buffer.from(parts[1].replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8'))
    return typeof payload.exp === 'number' ? payload.exp * 1000 : null
  } catch {
    return null
  }
}

/** auth 핸들러가 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface AuthSvc extends ResumeSvc {
  deps: WsDeps
  insecureDevMode: boolean
}

/**
 * 세션 재개: 이 연결(ctx)을 유예 중이던 이전 Client에 붙인다.
 * 실행 순서는 S3까지 조립부에 있던 resumeSession 한 함수와 같다: 유예 해제·연결 상태 이식·임시 Client 제거·ctx 교체(resume)
 * → 인증 필드·임시 Client의 만료 타이머 정리·만료 타이머 예약(auth 소유 필드) → 재개 알림(resume).
 */
function resumeSession(ctx: Ctx<AuthSvc>, old: Client, token: string) {
  const temp = attachSession(ctx, old)
  old.authState = 'authenticated'
  old.tokenExp = jwtExpiryMs(token)
  if (temp.expiryTimer) clearTimeout(temp.expiryTimer)
  scheduleExpiryCheck(old)
  announceResumed(ctx)
}

/** S3: 연결 클로저의 함수를 그대로 옮겼다(캡처하던 값 없음) */
export function scheduleExpiryCheck(client: Client) {
  if (client.expiryTimer) clearTimeout(client.expiryTimer)
  client.expiryTimer = undefined
  if (client.tokenExp === null) return
  const delay = Math.min(Math.max(client.tokenExp + TOKEN_EXPIRY_GRACE_MS - Date.now(), 0), 2 ** 31 - 1)
  client.expiryTimer = setTimeout(() => {
    if (client.tokenExp !== null && client.tokenExp + TOKEN_EXPIRY_GRACE_MS <= Date.now()) {
      console.log(`[AUTH] ${client.id.slice(0, 8)} token expired - closing`)
      client.ws.close(4001, 'Token expired')
    }
  }, delay)
  client.expiryTimer.unref?.()
}

/** S3: 연결 클로저의 sendAuthResult를 그대로 옮겼다(ws·client는 ctx에서) */
export function sendAuthResult(ctx: Ctx<{ insecureDevMode: boolean }>) {
  const { ws, client } = ctx
  const { insecureDevMode } = ctx.svc
  send(ws, { type: 'auth-result', authenticated: isAuthenticated(client) || (insecureDevMode && client.authState === 'authenticated'), userId: client.userId ?? null })
}

/** `auth` — S3: 연결 클로저의 handleAuth 본문을 그대로 옮겼다 */
export async function handleAuth(ctx: Ctx<AuthSvc>, msg: Msg) {
  const { client, id } = ctx
  const { deps, insecureDevMode, resume } = ctx.svc

  const token = msg.accessToken

  // 재인증 (L1): 같은 사용자의 갱신 토큰이면 만료 연장, 방 밖의 익명 연결은 로그인으로 승격
  if (client.authState === 'authenticated' || client.authState === 'anonymous') {
    if (insecureDevMode || !isNonEmptyString(token, 8192)) return
    const uid = await deps.verifyToken(token)
    if (!uid) return
    if (client.authState === 'authenticated' && uid === client.userId) {
      client.tokenExp = jwtExpiryMs(token)
      scheduleExpiryCheck(client)
    } else if (client.authState === 'anonymous' && !client.roomId) {
      client.userId = uid
      client.authState = 'authenticated'
      client.tokenExp = jwtExpiryMs(token)
      scheduleExpiryCheck(client)
      console.log(`[AUTH] ${id.slice(0, 8)} upgraded to user ${uid.slice(0, 8)}`)
    }
    sendAuthResult(ctx)
    return
  }

  client.authState = 'verifying'
  if (isNonEmptyString(token, 8192) && deps.hasBackend) {
    const uid = await deps.verifyToken(token)
    // 세션 재개: 이전 clientId의 주인이 같은 userId이고 유예시간 안(suspended에 남아 있음)일 때만
    const resumeId = msg.resumeClientId
    if (uid && isNonEmptyString(resumeId, 64)) {
      const old = resume.find(resumeId, uid)
      if (old) {
        resumeSession(ctx, old, token)
        return
      }
    }
    if (uid) {
      client.userId = uid
      client.authState = 'authenticated'
      client.tokenExp = jwtExpiryMs(token)
      scheduleExpiryCheck(client)
    }
  } else if (insecureDevMode && isNonEmptyString(token, 8192)) {
    // 개발 모드: 검증 불가. userId는 join 시 클라이언트 값 사용
    client.authState = 'authenticated'
  }

  if (client.authState !== 'authenticated') client.authState = 'anonymous'
  console.log(`[AUTH] ${id.slice(0, 8)} ${client.authState}${client.userId ? ` user ${client.userId.slice(0, 8)}` : ''}`)
  sendAuthResult(ctx)
}
