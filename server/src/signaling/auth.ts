import type { Ctx } from './ctx'
import { TOKEN_EXPIRY_GRACE_MS, isNonEmptyString, type Msg } from './protocol'
import { isAuthenticated, send, type Registry } from './registry'
import type { Client, WsDeps } from './types'

/**
 * 인증 (설계 docs/server-architecture.md 2.1절 B, 계층 3).
 * `auth` 메시지·재인증·토큰 만료 타이머. 세션 재개(resumeSession)는 아직 조립부(wsService)에 있어 svc로 받는다
 * (resume.ts로 떼는 일은 S4 — 같은 계층 3끼리 import 간선을 만들지 않으려고 함수로 주입).
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
export interface AuthSvc {
  registry: Registry
  deps: WsDeps
  insecureDevMode: boolean
  /** clientId → 연결이 끊겨 재개를 기다리는 클라이언트 */
  suspended: Map<string, Client>
  /** 세션 재개: 이 연결(ctx)을 유예 중이던 이전 Client에 붙인다 (조립부 wsService가 준다) */
  resumeSession(ctx: Ctx, old: Client, token: string): void
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
  const { clients } = ctx.svc.registry
  const { deps, insecureDevMode, suspended } = ctx.svc
  const resumeSession = (old: Client, token: string) => ctx.svc.resumeSession(ctx, old, token)

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
      const old = suspended.get(resumeId)
      if (old && old.userId === uid && clients.get(old.id) === old) {
        resumeSession(old, token)
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
