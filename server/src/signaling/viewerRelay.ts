import type { Ctx } from './ctx'
import type { Msg } from './protocol'
import { isPerformer, send, type Registry } from './registry'

/** 관람 WebRTC 중계 (설계 docs/server-architecture.md 2.1절 K, 계층 2). 상태 없음(중계만) */

export interface ViewerRelaySvc {
  registry: Registry
}

const DESCRIPTION_TYPES = new Set(['offer', 'answer', 'pranswer', 'rollback'])

const isRecord = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)
const optString = (v: unknown) => v === undefined || v === null || typeof v === 'string'

/** RTCSessionDescriptionInit 중 서버가 아는 필드만: { type?, sdp }. 형식이 틀리면 null */
export function parseSessionDescription(v: unknown): { type?: string; sdp: string } | null {
  if (!isRecord(v) || typeof v.sdp !== 'string') return null
  if (v.type !== undefined && !(typeof v.type === 'string' && DESCRIPTION_TYPES.has(v.type))) return null
  return v.type === undefined ? { sdp: v.sdp } : { type: v.type as string, sdp: v.sdp }
}

/** RTCIceCandidateInit 중 서버가 아는 필드만. candidate는 빈 문자열(후보 끝)도 허용. 형식이 틀리면 null */
export function parseIceCandidate(v: unknown): Record<string, string | number | null> | null {
  if (!isRecord(v) || typeof v.candidate !== 'string') return null
  const { sdpMid, sdpMLineIndex, usernameFragment } = v
  if (!optString(sdpMid) || !optString(usernameFragment)) return null
  if (!(sdpMLineIndex === undefined || sdpMLineIndex === null || (Number.isInteger(sdpMLineIndex) && (sdpMLineIndex as number) >= 0 && (sdpMLineIndex as number) <= 0xffff))) return null
  const out: Record<string, string | number | null> = { candidate: v.candidate }
  if (sdpMid !== undefined) out.sdpMid = sdpMid as string | null
  if (sdpMLineIndex !== undefined) out.sdpMLineIndex = sdpMLineIndex as number | null
  if (usernameFragment !== undefined) out.usernameFragment = usernameFragment as string | null
  return out
}

/**
 * 중계할 메시지를 서버가 다시 구성한다(native-descriptor와 같은 원칙, 서버 보안 점검): type·to·from + 종류별 본문 하나
 * (offer/answer: offer·answer, ice-candidate: candidate). 클라이언트가 넣은 다른 필드(oderId·isHost 등)는 버린다.
 * 본문 형식이 틀리면 null(중계하지 않음).
 */
export function buildViewerSignal(msg: Msg, from: string, to: string): Record<string, unknown> | null {
  if (msg.type === 'offer' || msg.type === 'answer') {
    const desc = parseSessionDescription(msg[msg.type])
    return desc && { type: msg.type, to, from, [msg.type]: desc }
  }
  if (msg.type === 'ice-candidate') {
    const candidate = parseIceCandidate(msg.candidate)
    return candidate && { type: 'ice-candidate', to, from, candidate }
  }
  return null
}

/** `offer`·`answer`·`ice-candidate` */
export function handleViewerSignal(ctx: Ctx<ViewerRelaySvc>, msg: Msg) {
  const { client, id } = ctx
  const { getRoomPeer } = ctx.svc.registry

  // WebRTC 시그널링: 같은 방 안에서, 양쪽 중 한쪽 이상이 연주자일 때만 (M1)
  const target = getRoomPeer(client, msg.to)
  if (target && (isPerformer(client) || isPerformer(target))) {
    const out = buildViewerSignal(msg, id, target.id)
    if (!out) return
    console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${target.id.slice(0, 8)}`)
    send(target.conn.ws, out)
  }
}
