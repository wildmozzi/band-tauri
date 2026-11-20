import type { Ctx } from './ctx'
import { MAX_CHAT_LENGTH, type Msg } from './protocol'
import { isAuthenticated, send, type Registry } from './registry'

/** 채팅 (설계 docs/server-architecture.md 2.1절 J, 계층 2). 상태 없음(중계만) */

export interface ChatSvc {
  registry: Registry
  insecureDevMode: boolean
}

/** `chat` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleChat(ctx: Ctx<ChatSvc>, msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { broadcastToRoom } = ctx.svc.registry
  if (!roomId) return

  // 익명 관람자는 채팅할 수 없다(P2-14c). 신원은 JWT(auth 단계에서 검증한 userId)로만 판단한다 — 클라이언트가 보낸 필드는 보지 않는다.
  // insecureDevMode(로컬 개발, Supabase 없음)는 연주 시작처럼 auth를 보낸 연결을 로그인으로 본다
  if (!isAuthenticated(client) && !(ctx.svc.insecureDevMode && client.auth.state === 'authenticated')) {
    send(ctx.ws, { type: 'error', code: 'chat-login-required', message: '로그인하면 채팅할 수 있어요.' })
    return
  }
  // 채팅: 서버가 필드를 새로 구성 (M4)
  if (typeof msg.message !== 'string') return
  const message = msg.message.trim().slice(0, MAX_CHAT_LENGTH)
  if (!message) return
  broadcastToRoom(roomId, id, { type: 'chat', message, nickname: client.member.nickname, timestamp: Date.now() })
  return
}
