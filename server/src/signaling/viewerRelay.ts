import type { Ctx } from './ctx'
import { MAX_ICE_CANDIDATE_LENGTH, MAX_ICE_FIELD_LENGTH, MAX_SDP_LENGTH, type Msg } from './protocol'
import { TokenBucket } from './rateLimit'
import { isPerformer, send, type Registry } from './registry'
import type { Client, WsLimits } from './types'

/**
 * 관람 WebRTC 중계 (설계 docs/server-architecture.md 2.1절 K, 계층 2). 상태는 연결당 속도 제한(Client.relay)뿐.
 * 남용 방지(#184 리뷰 M — 익명 관람자가 큰 offer를 몰아 보내 연주자의 송신 대기열을 채워 끊게 하던 표적 강퇴):
 * 본문 크기 상한(MAX_SDP_LENGTH 등), 보낸 사람 → 대상별 메시지 버킷, 대상이 받는 중계 바이트 버킷(보낸 사람 전체 합).
 * 넘친 메시지는 조용히 버린다(정상 클라이언트는 닿지 않는 한도).
 */

export interface ViewerRelaySvc {
  registry: Registry
  limits: WsLimits
}

/** 보낸 사람 하나가 버킷을 들고 있는 대상 수 상한(방 정원 20보다 크게). 넘으면 비우고 새로 센다(메모리 상한) */
const MAX_RELAY_TARGETS = 64

function relayState(c: Client, limits: WsLimits): NonNullable<Client['relay']> {
  if (!c.relay) c.relay = { out: new Map(), inBytes: new TokenBucket(limits.relayInBytesBurst, limits.relayInBytesPerSec) }
  return c.relay
}

/** 보낸 사람 → 대상 메시지 한 개, 대상이 받는 bytes 바이트를 쓸 수 있으면 꺼내고 true */
function allowRelay(sender: Client, target: Client, bytes: number, limits: WsLimits): boolean {
  const out = relayState(sender, limits).out
  let bucket = out.get(target.id)
  if (!bucket) {
    if (out.size >= MAX_RELAY_TARGETS) out.clear()
    bucket = new TokenBucket(limits.relayPerTargetBurst, limits.relayPerTargetPerSec)
    out.set(target.id, bucket)
  }
  if (!bucket.take()) return false
  return relayState(target, limits).inBytes.take(bytes)
}

const DESCRIPTION_TYPES = new Set(['offer', 'answer', 'pranswer', 'rollback'])

const isRecord = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)
const optString = (v: unknown) => v === undefined || v === null || (typeof v === 'string' && v.length <= MAX_ICE_FIELD_LENGTH)

/** RTCSessionDescriptionInit 중 서버가 아는 필드만: { type?, sdp }. 형식이 틀리면 null */
export function parseSessionDescription(v: unknown): { type?: string; sdp: string } | null {
  if (!isRecord(v) || typeof v.sdp !== 'string' || v.sdp.length > MAX_SDP_LENGTH) return null
  if (v.type !== undefined && !(typeof v.type === 'string' && DESCRIPTION_TYPES.has(v.type))) return null
  return v.type === undefined ? { sdp: v.sdp } : { type: v.type as string, sdp: v.sdp }
}

/** RTCIceCandidateInit 중 서버가 아는 필드만. candidate는 빈 문자열(후보 끝)도 허용. 형식이 틀리면 null */
export function parseIceCandidate(v: unknown): Record<string, string | number | null> | null {
  if (!isRecord(v) || typeof v.candidate !== 'string' || v.candidate.length > MAX_ICE_CANDIDATE_LENGTH) return null
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
    const data = JSON.stringify(out)
    if (!allowRelay(client, target, data.length, ctx.svc.limits)) return
    console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${target.id.slice(0, 8)}`)
    send(target.conn.ws, out)
  }
}
