import type { Ctx } from './ctx'
import type { Performers } from './performers'
import { isNonEmptyString, type InstrumentChangeRequest, type Msg, type PerformRequest } from './protocol'
import { isAuthenticated, send, type Registry } from './registry'
import type { Client } from './types'
import type { RoomMetaStore } from './roomMeta'

/**
 * 참여 요청·악기 변경 요청 (설계 docs/server-architecture.md 2.1절 I, 계층 2).
 * 요청 맵의 소유자. 요청 메시지 8종은 아래 handle*(S3). handleJoin(room-state)·handleLeave가 맵을 직접 보므로 맵도 내보낸다(S4에서 감춤).
 */
export function createRequests() {
  const performRequests = new Map<string, PerformRequest[]>()
  const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()

  /**
   * 퇴장 정리 단계 (membership.leave, 설계 2.2절): 나간 사람의 대기 중 요청을 지우고 방장에게 알린다.
   * 순서: perform-request-cancelled → instrument-change-request-cancelled. host는 퇴장 처리한 뒤의 방장(없으면 null).
   */
  function onLeave(roomId: string, clientId: string, host: Client | null) {
    const requests = performRequests.get(roomId)
    if (requests) {
      const idx = requests.findIndex(r => r.oderId === clientId)
      if (idx !== -1) {
        requests.splice(idx, 1)
        if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: clientId })
      }
    }

    const changeRequests = instrumentChangeRequests.get(roomId)
    if (changeRequests) {
      const idx = changeRequests.findIndex(r => r.oderId === clientId)
      if (idx !== -1) {
        changeRequests.splice(idx, 1)
        if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: clientId })
      }
    }
  }

  /** 방이 비었을 때: 그 방의 요청 목록을 지운다 */
  function onRoomEmpty(roomId: string) {
    performRequests.delete(roomId)
    instrumentChangeRequests.delete(roomId)
  }

  return { performRequests, instrumentChangeRequests, onLeave, onRoomEmpty }
}

export type Requests = ReturnType<typeof createRequests>

/** 요청 핸들러가 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface RequestsSvc {
  registry: Registry
  roomMeta: RoomMetaStore
  requests: Requests
  performers: Performers
}

/**
 * 승인 계열 네 개(approve/reject-perform, approve/reject-instrument-change)의 공통 전처리: 방장 여부를 다시 확인 (L2).
 * S3: handleMessage의 `isHostAction` 블록을 그대로 옮겼다. 통과하면 검사한 roomId·targetId를 좁힌 타입으로 돌려주고 아니면 null.
 * (S4-1: 호출부에 있던 "통과하면 늘 참인 원래 분기 조건 사본"을 이 반환값으로 대신했다 — #93 리뷰 Low-3)
 * roomId는 호출부가 처리 시작 때 읽은 값이다(원래 handleMessage의 `const roomId = client.roomId`).
 */
async function requireHost(ctx: Ctx<RequestsSvc>, roomId: string | undefined, msg: Msg): Promise<{ roomId: string; targetId: string } | null> {
  const { client } = ctx
  const { refreshHost } = ctx.svc.roomMeta
  const targetId = msg.targetId
  if (!roomId || typeof targetId !== 'string') return null
  if (!(await refreshHost(client)) || client.roomId !== roomId) return null
  return { roomId, targetId }
}

/** `request-perform` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건은 첫 줄로) */
export function handleRequestPerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.roomId
  const { findRoomHost } = ctx.svc.registry
  const { performRequests } = ctx.svc.requests
  if (!roomId || !isNonEmptyString(msg.instrument, 50)) return

  if (!isAuthenticated(client)) {
    send(ws, { type: 'error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
    send(ws, { type: 'request-perform-error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
    return
  }
  if (!performRequests.has(roomId)) performRequests.set(roomId, [])
  const requests = performRequests.get(roomId)!
  const host = findRoomHost(roomId)
  const existing = requests.find(r => r.oderId === id)
  if (existing) {
    // 재전송(화면 새로고침 등): 대기 상태만 다시 알려준다
    send(ws, { type: 'perform-request-sent', instrument: existing.instrument, hostOnline: !!host })
    return
  }
  const request: PerformRequest = { oderId: id, nickname: client.nickname, instrument: msg.instrument, timestamp: Date.now() }
  requests.push(request)
  console.log(`[REQUEST-PERFORM] ${id.slice(0, 8)} (${client.nickname}) requested to perform: ${msg.instrument}`)
  if (host) send(host.ws, { type: 'perform-request-received', request })
  // hostOnline=false면 방장이 입장할 때 room-state와 함께 요청이 전달된다
  send(ws, { type: 'perform-request-sent', instrument: msg.instrument, hostOnline: !!host })
  return
}

/** `approve-perform` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleApprovePerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { performRequests } = ctx.svc.requests
  const { setApproval } = ctx.svc.performers
  const host = await requireHost(ctx, client.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = performRequests.get(roomId)
  const target = getRoomPeer(client, targetId)
  if (!requests || !target) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  if (target.userId) setApproval(roomId, target.userId, null)
  console.log(`[APPROVE-PERFORM] Host approved ${target.id.slice(0, 8)} (${request.nickname}) for ${request.instrument}`)
  send(target.ws, { type: 'perform-request-approved', instrument: request.instrument })
  return
}

/** `reject-perform` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleRejectPerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { performRequests } = ctx.svc.requests
  const host = await requireHost(ctx, client.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = performRequests.get(roomId)
  if (!requests) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  console.log(`[REJECT-PERFORM] Host rejected ${targetId.slice(0, 8)} (${request.nickname})`)
  const target = getRoomPeer(client, targetId)
  const reason = (typeof msg.reason === 'string' ? msg.reason.trim().slice(0, 200) : '') || '방장이 요청을 거절했습니다.'
  if (target) send(target.ws, { type: 'perform-request-rejected', reason, instrument: request.instrument })
  return
}

/** `cancel-perform-request` — S3: 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleCancelPerformRequest(ctx: Ctx<RequestsSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.roomId
  const { findRoomHost } = ctx.svc.registry
  const { performRequests } = ctx.svc.requests
  if (!roomId) return

  const requests = performRequests.get(roomId)
  const host = findRoomHost(roomId)
  if (requests) {
    const requestIndex = requests.findIndex(r => r.oderId === id)
    if (requestIndex !== -1) {
      requests.splice(requestIndex, 1)
      if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
    }
  }
  return
}

/** `request-instrument-change` — S3: 분기 본문을 그대로 옮겼다 (분기 조건은 첫 줄로) */
export function handleRequestInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.roomId
  const { findRoomHost } = ctx.svc.registry
  const { instrumentChangeRequests } = ctx.svc.requests
  if (!roomId || !client.isPerforming || !isNonEmptyString(msg.newInstrument, 50)) return

  if (!instrumentChangeRequests.has(roomId)) instrumentChangeRequests.set(roomId, [])
  const requests = instrumentChangeRequests.get(roomId)!
  if (requests.find(r => r.oderId === id)) return
  const request: InstrumentChangeRequest = { oderId: id, nickname: client.nickname, currentInstrument: client.instrument || '', newInstrument: msg.newInstrument, timestamp: Date.now() }
  requests.push(request)
  const host = findRoomHost(roomId)
  if (host) send(host.ws, { type: 'instrument-change-request-received', request })
  send(ws, { type: 'instrument-change-request-sent', newInstrument: msg.newInstrument })
  return
}

/** `approve-instrument-change` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleApproveInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { rooms, clients, getRoomPeer } = ctx.svc.registry
  const { instrumentChangeRequests } = ctx.svc.requests
  const host = await requireHost(ctx, client.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = instrumentChangeRequests.get(roomId)
  const target = getRoomPeer(client, targetId)
  if (!requests || !target) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  target.instrument = request.newInstrument
  send(target.ws, { type: 'instrument-change-approved', newInstrument: request.newInstrument })
  for (const oderId of rooms.get(roomId) || []) {
    const peer = clients.get(oderId)
    if (peer && peer.id !== target.id) send(peer.ws, { type: 'participant-instrument-changed', oderId: target.id, instrument: request.newInstrument })
  }
  return
}

/** `reject-instrument-change` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleRejectInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { instrumentChangeRequests } = ctx.svc.requests
  const host = await requireHost(ctx, client.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = instrumentChangeRequests.get(roomId)
  if (!requests) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  requests.splice(requestIndex, 1)
  const target = getRoomPeer(client, targetId)
  const reason = typeof msg.reason === 'string' ? msg.reason.slice(0, 200) : '방장이 악기 변경을 거절했습니다.'
  if (target) send(target.ws, { type: 'instrument-change-rejected', reason })
  return
}

/** `cancel-instrument-change-request` — S3: 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleCancelInstrumentChangeRequest(ctx: Ctx<RequestsSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.roomId
  const { findRoomHost } = ctx.svc.registry
  const { instrumentChangeRequests } = ctx.svc.requests
  if (!roomId) return

  const requests = instrumentChangeRequests.get(roomId)
  const host = findRoomHost(roomId)
  if (requests) {
    const requestIndex = requests.findIndex(r => r.oderId === id)
    if (requestIndex !== -1) {
      requests.splice(requestIndex, 1)
      if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
    }
  }
  return
}
