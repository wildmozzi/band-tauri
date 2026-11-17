import type { Ctx } from './ctx'
import type { Performers } from './performers'
import { isNonEmptyString, rejectReason, type InstrumentChangeRequest, type Msg, type PerformRequest } from './protocol'
import { isAuthenticated, isPerformer, send, type Registry } from './registry'
import type { Client } from './types'
import type { RoomMetaStore } from './roomMeta'

/**
 * 참여 요청·악기 변경 요청 (설계 docs/server-architecture.md 2.1절 I, 계층 2).
 * 요청 맵의 소유자. 요청 메시지 8종은 아래 handle*(S3). 맵은 내보내지 않는다(S4-3): 방별 목록은 이 모듈의 핸들러만
 * performList/instrumentList로 꺼내 쓰고, room-state는 pendingFor, 퇴장 정리는 onLeave·onRoomEmpty.
 */
export function createRequests() {
  const performRequests = new Map<string, PerformRequest[]>()
  const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()

  /**
   * 퇴장 정리 단계 (membership.leave, 설계 2.2절): 나간 사람의 대기 중 요청을 지우고 방장에게 알린다.
   * 순서: perform-request-cancelled → instrument-change-request-cancelled. host는 퇴장 처리한 뒤의 방장(없으면 null).
   */
  function onLeave(roomId: string, clientId: string, host: Client | null) {
    dropPerformRequest(roomId, clientId, host)
    dropInstrumentChange(roomId, clientId, host)
  }

  /** 대기 중인 참여 요청을 지우고 방장에게 perform-request-cancelled (onLeave 첫째 단계, free_join에서 바로 연주를 시작했을 때) */
  function dropPerformRequest(roomId: string, clientId: string, host: Client | null) {
    const requests = performRequests.get(roomId)
    if (requests) {
      const idx = requests.findIndex(r => r.oderId === clientId)
      if (idx !== -1) {
        requests.splice(idx, 1)
        if (host) send(host.conn.ws, { type: 'perform-request-cancelled', oderId: clientId })
      }
    }
  }

  /** 대기 중인 악기 변경 요청을 지우고 방장에게 instrument-change-request-cancelled (onLeave 둘째 단계) */
  function dropInstrumentChange(roomId: string, clientId: string, host: Client | null) {
    const changeRequests = instrumentChangeRequests.get(roomId)
    if (changeRequests) {
      const idx = changeRequests.findIndex(r => r.oderId === clientId)
      if (idx !== -1) {
        changeRequests.splice(idx, 1)
        if (host) send(host.conn.ws, { type: 'instrument-change-request-cancelled', oderId: clientId })
      }
    }
  }

  /** 방이 비었을 때: 그 방의 요청 목록을 지운다 */
  function onRoomEmpty(roomId: string) {
    performRequests.delete(roomId)
    instrumentChangeRequests.delete(roomId)
  }

  /** 방의 참여 요청 목록. create면 없을 때 만든다 (이 모듈의 핸들러 전용) */
  function performList(roomId: string, create: true): PerformRequest[]
  function performList(roomId: string, create?: false): PerformRequest[] | undefined
  function performList(roomId: string, create = false): PerformRequest[] | undefined {
    if (create && !performRequests.has(roomId)) performRequests.set(roomId, [])
    return performRequests.get(roomId)
  }

  /** 방의 악기 변경 요청 목록. create면 없을 때 만든다 (이 모듈의 핸들러 전용) */
  function instrumentList(roomId: string, create: true): InstrumentChangeRequest[]
  function instrumentList(roomId: string, create?: false): InstrumentChangeRequest[] | undefined
  function instrumentList(roomId: string, create = false): InstrumentChangeRequest[] | undefined {
    if (create && !instrumentChangeRequests.has(roomId)) instrumentChangeRequests.set(roomId, [])
    return instrumentChangeRequests.get(roomId)
  }

  /** room-state와 함께 방장에게 다시 보낼 대기 중 요청 (읽기 전용) */
  function pendingFor(roomId: string): { perform: readonly PerformRequest[]; instrument: readonly InstrumentChangeRequest[] } {
    return { perform: performRequests.get(roomId) || [], instrument: instrumentChangeRequests.get(roomId) || [] }
  }

  return { performList, instrumentList, pendingFor, onLeave, onRoomEmpty, dropInstrumentChange, dropPerformRequest }
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
  if (!(await refreshHost(client)) || client.member.roomId !== roomId) return null
  return { roomId, targetId }
}

/** `request-perform` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건은 첫 줄로) */
export function handleRequestPerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.member.roomId
  const { findRoomHost } = ctx.svc.registry
  const { performList } = ctx.svc.requests
  if (!roomId || !isNonEmptyString(msg.instrument, 50)) return

  if (!isAuthenticated(client)) {
    send(ws, { type: 'error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
    send(ws, { type: 'request-perform-error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
    return
  }
  const requests = performList(roomId, true)
  const host = findRoomHost(roomId)
  const existing = requests.find(r => r.oderId === id)
  if (existing) {
    // 재전송(화면 새로고침 등): 대기 상태만 다시 알려준다
    send(ws, { type: 'perform-request-sent', instrument: existing.instrument, hostOnline: !!host })
    return
  }
  const request: PerformRequest = { oderId: id, nickname: client.member.nickname, instrument: msg.instrument, timestamp: Date.now() }
  requests.push(request)
  console.log(`[REQUEST-PERFORM] ${id.slice(0, 8)} (${client.member.nickname}) requested to perform: ${msg.instrument}`)
  if (host) send(host.conn.ws, { type: 'perform-request-received', request })
  // hostOnline=false면 방장이 입장할 때 room-state와 함께 요청이 전달된다
  send(ws, { type: 'perform-request-sent', instrument: msg.instrument, hostOnline: !!host })
  return
}

/** `approve-perform` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleApprovePerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { performList } = ctx.svc.requests
  const { setApproval } = ctx.svc.performers
  const host = await requireHost(ctx, client.member.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = performList(roomId)
  const target = getRoomPeer(client, targetId)
  if (!requests || !target) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  if (target.auth.userId) setApproval(roomId, target.auth.userId, null, request.instrument)
  console.log(`[APPROVE-PERFORM] Host approved ${target.id.slice(0, 8)} (${request.nickname}) for ${request.instrument}`)
  send(target.conn.ws, { type: 'perform-request-approved', instrument: request.instrument })
  return
}

/** `reject-perform` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleRejectPerform(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { performList } = ctx.svc.requests
  const host = await requireHost(ctx, client.member.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = performList(roomId)
  if (!requests) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  console.log(`[REJECT-PERFORM] Host rejected ${targetId.slice(0, 8)} (${request.nickname})`)
  const target = getRoomPeer(client, targetId)
  const reason = rejectReason(msg.reason, '방장이 요청을 거절했습니다.')
  if (target) send(target.conn.ws, { type: 'perform-request-rejected', reason, instrument: request.instrument })
  return
}

/** `cancel-perform-request` — S3: 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleCancelPerformRequest(ctx: Ctx<RequestsSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { findRoomHost } = ctx.svc.registry
  const { performList } = ctx.svc.requests
  if (!roomId) return

  const requests = performList(roomId)
  const host = findRoomHost(roomId)
  if (requests) {
    const requestIndex = requests.findIndex(r => r.oderId === id)
    if (requestIndex !== -1) {
      requests.splice(requestIndex, 1)
      if (host) send(host.conn.ws, { type: 'perform-request-cancelled', oderId: id })
    }
  }
  return
}

/** `request-instrument-change` — S3: 분기 본문을 그대로 옮겼다 (분기 조건은 첫 줄로) */
export function handleRequestInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.member.roomId
  const { findRoomHost } = ctx.svc.registry
  const { instrumentList } = ctx.svc.requests
  if (!roomId || !client.perform.isPerforming || !isNonEmptyString(msg.newInstrument, 50)) return

  const requests = instrumentList(roomId, true)
  if (requests.find(r => r.oderId === id)) return
  const request: InstrumentChangeRequest = { oderId: id, nickname: client.member.nickname, currentInstrument: client.perform.instrument || '', newInstrument: msg.newInstrument, timestamp: Date.now() }
  requests.push(request)
  const host = findRoomHost(roomId)
  if (host) send(host.conn.ws, { type: 'instrument-change-request-received', request })
  send(ws, { type: 'instrument-change-request-sent', newInstrument: msg.newInstrument })
  return
}

/** `approve-instrument-change` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleApproveInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { roomMembers, getRoomPeer } = ctx.svc.registry
  const { instrumentList } = ctx.svc.requests
  const host = await requireHost(ctx, client.member.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = instrumentList(roomId)
  const target = getRoomPeer(client, targetId)
  if (!requests || !target) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  const request = requests[requestIndex]
  requests.splice(requestIndex, 1)
  target.perform.instrument = request.newInstrument
  // 승인 악기도 바꾼다: 이후 start-performing 재전송(엔진 재시작 등)이 새 악기로 유지된다 (P2-21b)
  ctx.svc.performers.updateApprovedInstrument(roomId, target.auth.userId, request.newInstrument)
  send(target.conn.ws, { type: 'instrument-change-approved', newInstrument: request.newInstrument })
  for (const peer of roomMembers(roomId)) {
    if (peer.id !== target.id) send(peer.conn.ws, { type: 'participant-instrument-changed', oderId: target.id, instrument: request.newInstrument })
  }
  return
}

/** `reject-instrument-change` — S3: 방장 재확인(requireHost) 뒤 분기 본문을 그대로 옮겼다 */
export async function handleRejectInstrumentChange(ctx: Ctx<RequestsSvc>, msg: Msg) {
  const { client } = ctx
  const { getRoomPeer } = ctx.svc.registry
  const { instrumentList } = ctx.svc.requests
  const host = await requireHost(ctx, client.member.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const requests = instrumentList(roomId)
  if (!requests) return
  const requestIndex = requests.findIndex(r => r.oderId === targetId)
  if (requestIndex === -1) return
  requests.splice(requestIndex, 1)
  const target = getRoomPeer(client, targetId)
  // P2-11: reject-perform과 같은 규칙(trim·200자·빈 사유는 기본 문구). 전에는 trim 없이 잘라 빈 문자열·공백도 그대로 보냈다.
  const reason = rejectReason(msg.reason, '방장이 악기 변경을 거절했습니다.')
  if (target) send(target.conn.ws, { type: 'instrument-change-rejected', reason })
  return
}

/** `cancel-instrument-change-request` — S3: 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleCancelInstrumentChangeRequest(ctx: Ctx<RequestsSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { findRoomHost } = ctx.svc.registry
  const { instrumentList } = ctx.svc.requests
  if (!roomId) return

  const requests = instrumentList(roomId)
  const host = findRoomHost(roomId)
  if (requests) {
    const requestIndex = requests.findIndex(r => r.oderId === id)
    if (requestIndex !== -1) {
      requests.splice(requestIndex, 1)
      if (host) send(host.conn.ws, { type: 'instrument-change-request-cancelled', oderId: id })
    }
  }
  return
}

/**
 * 관람자로 내리기(P2-9)가 쓰는 공용 서비스 조각. 엔진·메트로놈은 같은 계층 모듈이라 import하지 않고 쓰는 함수만 구조 타입으로 적는다
 * (requests → nativeSession·metronome 간선 없이, 설계 docs/server-architecture.md 2.1절).
 */
export interface DemoteSvc extends RequestsSvc {
  nativeSession: { clearNative(c: Client): void }
  metronome: { onEngineDown(roomId: string, clientId: string): void }
}

/**
 * `demote-performer {targetId}` — 방장이 연주 중인 참가자를 관람자로 내린다 (P2-9, 계약: native-engine-design.md 5절).
 * 권한은 DB 방장(requireHost, 아니면 무시 — 다른 방장 조작과 같음). 대상이 같은 방에서 연주 중인, 방장 아닌 참가자가 아니면
 * `error demote-not-performer`. 처리 순서(= 받는 메시지 순서):
 *   1. 승인 기록 삭제 (승인제 방이면 다시 연주하려면 새 참여 요청이 필요하다. free_join 방은 바로 다시 시작할 수 있다)
 *   2. 대기 중 요청 취소(퇴장 정리와 같은 onLeave) → 방장에게 perform-request-cancelled, instrument-change-request-cancelled
 *      (연주 중에도 참여 요청을 보낼 수 있어 남은 요청을 방장이 승인하면 곧바로 다시 승인되는 것을 막는다 — #180 리뷰 L-5)
 *   3. 연주 상태 해제(stop-performing과 같음) + 엔진 바인딩 해제(ssrc·키 — 릴레이 토큰 갱신 불가) + 대상이 켠 메트로놈 삭제
 *   4. 대상에게 demoted-to-viewer (재개 유예 중이면 재개 때 보낸다). 열린 소켓으로 보냈는데 유실된 경우는 재개 때 room-state의
 *      isPerforming=false를 보고 대상 앱이 스스로 맞춘다(#180 리뷰 M-1)
 *   5. 대상 외 전원에게 performer-updated(isPerforming=false) — stop-performing과 같은 모양
 *   6. 연주자·엔진 켠 참가자에게 native-leave(from=대상) — 각 엔진이 곧바로 대상 피어를 뺀다(대상 클라이언트의 native-leave를 기다리지 않음)
 */
export async function handleDemotePerformer(ctx: Ctx<DemoteSvc>, msg: Msg) {
  const { ws, client } = ctx
  const { getRoomPeer, roomMembers, broadcastToRoom } = ctx.svc.registry
  const { clearApproval } = ctx.svc.performers
  const { onLeave } = ctx.svc.requests
  // 방장 확인은 승인·거절과 같은 방 정보 캐시(최대 30초)다. 지금은 앱에 방장 이양 경로가 없어 충분하다.
  // 이양 기능을 만들면 이 파괴적 조작은 메트로놈처럼 짧은 캐시(checkHost(client, limits.metronomeHostCheckMs))로 바꾼다 (#180 리뷰 L-3)
  const host = await requireHost(ctx, client.member.roomId, msg)
  if (!host) return
  const { roomId, targetId } = host

  const target = getRoomPeer(client, targetId)
  if (target && !target.member.isHost && !target.perform.isPerforming && target.perform.starting) {
    // 대상이 연주를 시작하는 중(권한 확인 await): 그 시작을 취소하고 내린 것으로 처리한다(시그널링 상태 점검).
    // 아직 연주자가 아니라 엔진 바인딩·performer-updated·native-leave는 없다. 승인·요청은 아래와 같이 지운다
    console.log(`[DEMOTE-PERFORMER] Host demoted ${target.id.slice(0, 8)} while starting - start cancelled`)
    target.perform.cancelledStart = true
    clearApproval(roomId, target.auth.userId)
    onLeave(roomId, target.id, client)
    if (target.conn.ws.readyState === target.conn.ws.OPEN) send(target.conn.ws, { type: 'demoted-to-viewer' })
    else target.resume.pendingDemoted = true
    return
  }
  if (!target || !target.perform.isPerforming || target.member.isHost) {
    send(ws, { type: 'error', code: 'demote-not-performer', message: '연주 중인 참가자만 관람자로 내릴 수 있습니다.' })
    return
  }
  console.log(`[DEMOTE-PERFORMER] Host demoted ${target.id.slice(0, 8)} (${target.member.nickname}) to viewer`)
  // 1·2
  clearApproval(roomId, target.auth.userId)
  onLeave(roomId, target.id, client)
  // 3
  target.perform.isPerforming = false
  target.perform.instrument = undefined
  ctx.svc.nativeSession.clearNative(target)
  ctx.svc.metronome.onEngineDown(roomId, target.id)
  // 4
  if (target.conn.ws.readyState === target.conn.ws.OPEN) send(target.conn.ws, { type: 'demoted-to-viewer' })
  else target.resume.pendingDemoted = true
  // 5
  broadcastToRoom(roomId, target.id, { type: 'performer-updated', nickname: target.member.nickname, instrument: undefined, isPerforming: false })
  // 6
  for (const peer of roomMembers(roomId)) {
    if (peer.id === target.id) continue
    if (isPerformer(peer) || peer.native.active) send(peer.conn.ws, { type: 'native-leave', from: target.id })
  }
}
