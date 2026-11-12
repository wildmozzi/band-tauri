import type { Ctx } from './ctx'
import { APPROVAL_GRACE_MS, sanitizeNickname, type Msg } from './protocol'
import { isAuthenticated, isPerformer, send, type Registry } from './registry'
import type { RoomMetaStore } from './roomMeta'
import type { Client, LeaveReason, WsLimits } from './types'

/**
 * 연주 권한·승인 기록 (설계 docs/server-architecture.md 2.1절 F, 계층 2).
 * approvals의 소유자. start/stop-performing 메시지는 아래 handle*(S3). 맵은 내보내지 않는다(S4-3).
 * 승인 유예(끊김 → APPROVAL_GRACE_MS 뒤 만료)는 퇴장 정리(onLeave)가 걸고, 재입장(releaseGraceOnRejoin)이 푼다.
 */
export function createPerformers(roomMeta: RoomMetaStore, insecureDevMode: boolean) {
  const { getRoomMetaOrNull } = roomMeta
  /** roomId → (userId → 만료 시각 | null=무기한). 방장 승인 기록 (H1) */
  const approvals = new Map<string, Map<string, number | null>>()

  function hasApproval(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const exp = approvals.get(roomId)?.get(userId)
    if (exp === undefined) return false
    if (exp === null || exp > Date.now()) return true
    approvals.get(roomId)?.delete(userId)
    return false
  }

  function setApproval(roomId: string, userId: string, expiresAt: number | null) {
    if (!approvals.has(roomId)) approvals.set(roomId, new Map())
    approvals.get(roomId)!.set(userId, expiresAt)
  }

  function clearApproval(roomId: string, userId: string | undefined) {
    if (!userId) return
    const m = approvals.get(roomId)
    m?.delete(userId)
    if (m && m.size === 0) approvals.delete(roomId)
  }

  /** 연주 시작 허용 여부 (H1): 방장, free_join 방, 또는 방장이 승인한 사용자.
   *  client는 연결 클로저의 값을 호출부에서 넘긴다(팩토리는 연결 상태를 캡처하지 않는다 — password의 ws와 같은 원칙). */
  const canStartPerforming = async (client: Client, roomId: string): Promise<boolean> => {
    if (!isAuthenticated(client) && !(insecureDevMode && client.auth.state === 'authenticated')) return false
    if (insecureDevMode) return true
    if (client.member.isHost) return true
    if (hasApproval(roomId, client.auth.userId)) return true
    const meta = await getRoomMetaOrNull(roomId)
    return !!meta && meta.freeJoin
  }

  /**
   * 퇴장 정리 단계 (membership.leave, 설계 2.2절): 명시적 퇴장/방 이동은 승인 즉시 삭제, 연결 끊김은 재접속 유예 후 만료,
   * 중복 연결 정리는 유지. 정리 뒤에도 승인 기록이 남아 있으면 true (자리 예약 판정에 쓴다 — 만료 여부는 보지 않는 원래 판정).
   */
  function onLeave(client: Client, roomId: string, reason: LeaveReason): boolean {
    if (!client.auth.userId) return false
    if (reason === 'leave' || reason === 'move') {
      clearApproval(roomId, client.auth.userId)
    } else if (reason === 'close' && approvals.get(roomId)?.has(client.auth.userId)) {
      setApproval(roomId, client.auth.userId, Date.now() + APPROVAL_GRACE_MS)
    }
    return approvals.get(roomId)?.has(client.auth.userId) ?? false
  }

  /** 재입장: 유예 중인(아직 만료 전) 승인이 있으면 무기한으로 되돌린다 (membership 입장 6단계) */
  function releaseGraceOnRejoin(roomId: string, userId: string | undefined) {
    if (userId && approvals.get(roomId)?.has(userId) && hasApproval(roomId, userId)) {
      setApproval(roomId, userId, null)
    }
  }

  return { hasApproval, setApproval, clearApproval, canStartPerforming, onLeave, releaseGraceOnRejoin }
}

export type Performers = ReturnType<typeof createPerformers>

/** 연주 시작·중단 핸들러가 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface PerformersSvc {
  registry: Registry
  performers: Performers
  limits: WsLimits
  insecureDevMode: boolean
}

/** `start-performing` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export async function handleStartPerforming(ctx: Ctx<PerformersSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.member.roomId
  const { roomMemberIds, getClient, broadcastToRoom } = ctx.svc.registry
  const { canStartPerforming } = ctx.svc.performers
  const { limits, insecureDevMode } = ctx.svc
  if (!roomId) return

  if (!isAuthenticated(client) && !(insecureDevMode && client.auth.state === 'authenticated')) {
    send(ws, { type: 'error', code: 'login-required', message: '연주하려면 로그인이 필요합니다.' })
    return
  }
  if (!(await canStartPerforming(client, roomId)) || client.member.roomId !== roomId) {
    send(ws, { type: 'error', code: 'perform-not-approved', message: '방장의 승인이 필요합니다.' })
    return
  }
  // 연주자 수 제한 (이미 연주 중인 본인의 재전송은 허용)
  if (!client.perform.isPerforming) {
    let performers = 0
    for (const pid of roomMemberIds(roomId)) {
      if (pid !== id && getClient(pid)?.perform.isPerforming) performers++
    }
    if (performers >= limits.maxPerformers) {
      send(ws, { type: 'error', code: 'room-full-performers', message: `연주자는 최대 ${limits.maxPerformers}명입니다.` })
      return
    }
  }
  client.perform.isPerforming = true
  client.perform.instrument = typeof msg.instrument === 'string' ? msg.instrument.slice(0, 50) : undefined
  const nickname = sanitizeNickname(msg.nickname)
  if (nickname) client.member.nickname = nickname
  // 클라이언트 isHost는 무시 (서버 판정)
  console.log(`[START-PERFORMING] ${id.slice(0, 8)} (${client.member.nickname}) started performing: ${client.perform.instrument}, isHost: ${client.member.isHost}`)
  broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.member.nickname, instrument: client.perform.instrument, isPerforming: true, isHost: client.member.isHost })
  return
}

/** `stop-performing` — S3: 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleStopPerforming(ctx: Ctx<PerformersSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { broadcastToRoom } = ctx.svc.registry
  if (!roomId) return

  client.perform.isPerforming = false
  client.perform.instrument = undefined
  // 연주자가 아니게 되면 ssrc를 잊는다(토큰 갱신 불가 → 기존 토큰 만료와 함께 릴레이 슬롯 해제)
  if (!isPerformer(client)) client.native.ssrc = undefined
  console.log(`[STOP-PERFORMING] ${id.slice(0, 8)} (${client.member.nickname}) stopped performing`)
  broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.member.nickname, instrument: undefined, isPerforming: false })
  return
}
