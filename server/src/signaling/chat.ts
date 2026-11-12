import type { Ctx } from './ctx'
import { MAX_CHAT_LENGTH, type Msg } from './protocol'
import type { Registry } from './registry'

/** 채팅 (설계 docs/server-architecture.md 2.1절 J, 계층 2). 상태 없음(중계만) */

export interface ChatSvc {
  registry: Registry
}

/** `chat` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleChat(ctx: Ctx<ChatSvc>, msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { broadcastToRoom } = ctx.svc.registry
  if (!roomId) return

  // 채팅: 서버가 필드를 새로 구성 (M4)
  if (typeof msg.message !== 'string') return
  const message = msg.message.trim().slice(0, MAX_CHAT_LENGTH)
  if (!message) return
  broadcastToRoom(roomId, id, { type: 'chat', message, nickname: client.member.nickname, timestamp: Date.now() })
  return
}
