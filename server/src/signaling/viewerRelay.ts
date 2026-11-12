import type { Ctx } from './ctx'
import type { Msg } from './protocol'
import { isPerformer, send, type Registry } from './registry'

/** 관람 WebRTC 중계 (설계 docs/server-architecture.md 2.1절 K, 계층 2). 상태 없음(중계만) */

export interface ViewerRelaySvc {
  registry: Registry
}

/** `offer`·`answer`·`ice-candidate` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 */
export function handleViewerSignal(ctx: Ctx<ViewerRelaySvc>, msg: Msg) {
  const { client, id } = ctx
  const { getRoomPeer } = ctx.svc.registry

  // WebRTC 시그널링: 같은 방 안에서, 양쪽 중 한쪽 이상이 연주자일 때만 (M1)
  const target = getRoomPeer(client, msg.to)
  if (target && (isPerformer(client) || isPerformer(target))) {
    console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${target.id.slice(0, 8)}`)
    send(target.ws, { ...msg, from: id })
  }
  return
}
