import type { Capacity } from './capacity'
import type { Ctx } from './ctx'
import type { Metronome } from './metronome'
import type { NativeSession } from './nativeSession'
import type { Password } from './password'
import type { Performers } from './performers'
import {
  MAX_ROOM_ID_LENGTH, PARTICIPANT_UPDATE_DEBOUNCE_MS,
  isNonEmptyString, sanitizeNickname, type Msg,
} from './protocol'
import { send, type Registry } from './registry'
import type { Requests } from './requests'
import type { RoomMetaStore } from './roomMeta'
import type { Client, LeaveReason, WsDeps, WsLimits } from './types'

/**
 * 방 멤버십 (설계 docs/server-architecture.md 2.1절 E, 계층 3): join/leave, room-state, 중복 연결 대체, 참가자 수 DB 디바운스.
 *
 * S3: handleJoin·handleLeave·sendRoomState·scheduleParticipantUpdate를 wsService 클로저에서 **본문 순서 그대로** 옮겼다.
 * 바뀐 것은 캡처하던 값을 ctx·svc에서 꺼내는 머리 부분과, 같은 모듈 함수 호출에 svc/ctx 인자를 붙인 것뿐이다.
 * S4-2: handleLeave는 설계 2.2절의 이름 붙인 단계(소유 모듈의 onLeave·onRoomEmpty 호출)로, handleJoin은 단계 함수
 * (방 확인 → 비밀번호 → 정원 → 이동 → 중복 정리 → 추가·알림)로 나눴다. 실행 순서는 그대로다.
 * handleJoin의 마지막 await(비밀번호 검증) 뒤 ~ rooms 추가 구간은 동기 함수 admitToRoom 하나다(1.2-2: 동시 입장이 정원을 넘지 못함).
 */

/** membership이 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface MembershipSvc {
  registry: Registry
  roomMeta: RoomMetaStore
  capacity: Capacity
  password: Password
  performers: Performers
  metronome: Metronome
  nativeSession: NativeSession
  requests: Requests
  deps: WsDeps
  limits: WsLimits
  insecureDevMode: boolean
  /**
   * 세션 재개 유예(resume 모듈)의 해제 함수 하나. 순환 방지용 주입, 설계 2.2: resume이 membership을 import하므로
   * (계층 3 안 순서 membership < resume < auth) membership은 resume을 import하지 않고 이 구조 타입으로만 부른다.
   */
  resume: { drop(c: Client): void }
  /** roomId → 참가자 수 DB 갱신 디바운스 타이머 */
  participantTimers: Map<string, ReturnType<typeof setTimeout>>
}

/** 참가자 수 DB 갱신 디바운스 (M3) */
export function scheduleParticipantUpdate(svc: MembershipSvc, roomId: string) {
  const { deps, participantTimers } = svc
  const { roomSize } = svc.registry

  if (!deps.hasBackend || participantTimers.has(roomId)) return
  const t = setTimeout(() => {
    participantTimers.delete(roomId)
    void deps.updateParticipants(roomId, roomSize(roomId))
  }, PARTICIPANT_UPDATE_DEBOUNCE_MS)
  t.unref?.()
  participantTimers.set(roomId, t)
}

/**
 * 퇴장 처리 — 설계 2.2절의 고정된 호출 순서. **실행 순서가 클라이언트가 받는 메시지 순서다**(1.2-1).
 * 이벤트 버스가 아니라 명시적 호출이다. 단계를 더하거나 순서를 바꾸면 ws-transcript 기준 파일이 깨진다.
 *   1. 방 멤버에서 제거 + 참가자 수 DB 디바운스
 *   2. 승인 기록 (performers) — 정리 뒤에도 승인이 남았는지를 3에 넘긴다(capacity가 performers를 부르지 않게)
 *   3. 자리 예약·재입장 기록 (capacity)
 *   4. 대기 중 요청 취소 → 방장에게 perform-request-cancelled, instrument-change-request-cancelled (requests)
 *   5. 남은 사람에게 participant-left (registry)
 *   6. 나간 사람이 켠 메트로놈 삭제 (metronome)
 *   7. 방이 비면 방·요청·메트로놈·방 정보 캐시 삭제
 */
export function handleLeave(svc: MembershipSvc, client: Client, roomId: string, reason: LeaveReason) {
  const { registry, performers, capacity, requests, metronome, roomMeta } = svc
  const id = client.id
  if (!registry.hasRoom(roomId)) return

  // 1
  registry.removeFromRoom(roomId, id)
  console.log(`[LEAVE] Client ${id.slice(0, 8)} left room (${reason}). Room size: ${registry.roomSize(roomId)}`)
  scheduleParticipantUpdate(svc, roomId)
  // 2·3
  const approved = performers.onLeave(client, roomId, reason)
  capacity.onLeave(client, roomId, reason, approved)
  // 4
  requests.onLeave(roomId, id, registry.findRoomHost(roomId))
  // 5
  registry.broadcastLeft(roomId, id)
  // 6
  metronome.onMemberLeft(roomId, id)
  // 7
  if (registry.roomSize(roomId) === 0) {
    registry.deleteRoom(roomId)
    requests.onRoomEmpty(roomId)
    metronome.onRoomEmpty(roomId)
    roomMeta.forget(roomId)
  }
}

/** 현재 방 상태를 이 연결에 전송 (방장이면 대기 중인 요청도) */
export function sendRoomState(ctx: Ctx<MembershipSvc>, roomId: string) {
  const { ws, client, id } = ctx
  const { getRoomParticipants } = ctx.svc.registry
  const { pendingFor } = ctx.svc.requests

  const others = getRoomParticipants(roomId).filter(p => p.oderId !== id)
  send(ws, {
    type: 'room-state',
    participants: others,
    peerIds: others.map(p => p.oderId),
    isHost: client.member.isHost,
  })
  if (client.member.isHost) {
    const pending = pendingFor(roomId)
    for (const request of pending.perform) send(ws, { type: 'perform-request-received', request })
    for (const request of pending.instrument) send(ws, { type: 'instrument-change-request-received', request })
  }
}

/** join 1단계(방 확인)의 결과: 이 방에 들어올 조건 */
export interface JoinPlan {
  /** DB 기준 방장 (개발 모드는 클라이언트 값) */
  isHost: boolean
  /** 비밀번호 해시 (없으면 비밀번호 없는 방) */
  passwordHash: string | null
  /** 정원: 서버 상한과 rooms.max_participants 중 작은 값 */
  capacity: number
}

/**
 * join 1단계 — 방 확인 (백엔드 있음): 캐시 없이 DB를 새로 읽어 방 존재·방장·비밀번호·정원을 정한다.
 * 방금 읽은 DB 방장이 메트로놈을 켠 사람과 다르면(이양) 여기서 메트로놈을 멈춘다(join이 이양 감지 시점, 1.2-2).
 * 거부하면 에러를 보내고 null.
 */
async function checkRoom(ctx: Ctx<MembershipSvc>, roomId: string): Promise<JoinPlan | null> {
  const { ws, client } = ctx
  const { getRoomMetaOrNull } = ctx.svc.roomMeta
  const { dropMetronomeIfHostChanged } = ctx.svc.metronome
  const { limits } = ctx.svc

  let capacity = limits.maxRoomParticipants
  const meta = await getRoomMetaOrNull(roomId, 0)
  if (!meta) {
    send(ws, { type: 'error', code: 'room-not-found', message: '방을 찾을 수 없습니다.' })
    return null
  }
  const isHost = !!client.auth.userId && meta.hostId === client.auth.userId
  // 방금 읽은 DB 방장이 메트로놈을 켠 사람과 다르면(이양) 진행 중인 메트로놈을 멈춘다(PR #36 L1)
  dropMetronomeIfHostChanged(roomId, meta.hostId ?? null)
  const passwordHash = meta.passwordHash ?? null
  if (meta.maxParticipants !== undefined) capacity = Math.min(capacity, meta.maxParticipants)
  // 비밀번호 방으로 표시됐는데 해시가 아직 없음(생성·변경 중): 방장 외 입장 보류 (fail-closed, 리뷰 L1)
  if (meta.hasPassword && !passwordHash && !isHost) {
    send(ws, { type: 'error', code: 'password-busy', roomId, retryAfterMs: 1000, message: '합주실을 준비 중입니다. 잠시 후 다시 시도하세요.' })
    return null
  }
  return { isHost, passwordHash, capacity }
}

/** join 1단계 — 방 확인 (백엔드 없음): DB가 없어 방장은 개발 모드에서만 클라이언트 값. 동기(await 없음 — 원래도 없었다) */
function localRoomPlan(ctx: Ctx<MembershipSvc>, msg: Msg): JoinPlan {
  const { client } = ctx
  const { limits, insecureDevMode } = ctx.svc
  const isHost = insecureDevMode && client.auth.state === 'authenticated' ? msg.isHost === true : false
  return { isHost, passwordHash: null, capacity: limits.maxRoomParticipants }
}

/**
 * `join` — 단계: 방 확인 → 비밀번호 → (여기서부터 동기) 정원 → 이동 → 중복 정리 → 추가·알림.
 * await는 방 확인(DB)과 비밀번호(scrypt) 두 곳뿐이고, 각 await 뒤에 연결이 그대로인지 다시 확인한다(1.2-9).
 */
export async function handleJoin(ctx: Ctx<MembershipSvc>, msg: Msg) {
  const { ws, client, id, svc } = ctx
  const { getClient, isInRoom } = svc.registry
  const { checkRoomPassword } = svc.password
  const { deps, insecureDevMode } = svc

  const roomId = msg.roomId
  if (!isNonEmptyString(roomId, MAX_ROOM_ID_LENGTH)) return

  // 개발 모드에서만 클라이언트 userId 사용 (그 외에는 auth로 확정된 값만)
  if (insecureDevMode && client.auth.state === 'authenticated' && !client.auth.userId && isNonEmptyString(msg.userId, 128)) {
    client.auth.userId = msg.userId
  }

  // 1. 방 확인 + 방장 판정 (백엔드가 있으면 DB 기준). 백엔드가 없으면 await하지 않는다(원래와 같음)
  const plan = deps.hasBackend ? await checkRoom(ctx, roomId) : localRoomPlan(ctx, msg)
  if (!plan) return
  // DB 조회 중 연결이 끊겼으면 중단
  if (getClient(id) !== client) return

  // 이미 이 방에 있음 (세션 재개 후 클라이언트의 rejoin 등): 상태만 다시 보내고 브로드캐스트 없음
  if (client.member.roomId === roomId && isInRoom(roomId, id)) {
    client.member.isHost = plan.isHost
    const nickname = sanitizeNickname(msg.nickname)
    if (nickname) client.member.nickname = nickname
    sendRoomState(ctx, roomId)
    return
  }

  // 2. 비밀번호 방: 방장이 아니면 검증 (실패하면 지금 있는 방은 그대로 둔다)
  if (plan.passwordHash && !plan.isHost) {
    const ok = await checkRoomPassword(ws, client, roomId, plan.passwordHash, msg.password)
    if (!ok || getClient(id) !== client) return
  }

  // ── 마지막 await. 여기부터 방에 추가할 때까지 동기 구간(1.2-2) — admitToRoom 하나로 묶었다 ──
  admitToRoom(ctx, msg, roomId, plan)
}

/**
 * join 동기 구간: 정원 → 이동 → 중복 정리 → 추가·알림.
 *
 * **이 함수와 여기서 부르는 단계 함수에 await를 넣지 않는다.** 정원 검사부터 rooms 추가까지 사이에 다른 연결의 메시지가
 * 끼어들 수 없어야 동시 입장이 정원을 넘지 못한다(1.2-2, room-capacity.test.ts "concurrent joins never exceed the capacity").
 * 반환 타입을 `void`로 적어 고정한다: async로 바꾸면 tsc가 막고(async 함수의 반환 타입은 Promise여야 함),
 * async가 아닌 함수 안의 await는 문법 오류다. signaling-units.test.ts가 실제로 불러 Promise가 아님을 확인한다.
 */
export function admitToRoom(ctx: Ctx<MembershipSvc>, msg: Msg, roomId: string, plan: JoinPlan): void {
  const { client } = ctx

  // 3. 정원
  if (!passCapacity(ctx, roomId, plan)) return

  // 4. 이동: 다른 방에 있었다면 먼저 퇴장 처리
  moveOutOfPreviousRoom(ctx, roomId)
  client.member.isHost = plan.isHost

  const nickname = sanitizeNickname(msg.nickname)
  if (nickname) client.member.nickname = nickname

  // 5. 중복 정리
  const replacedDuplicate = replaceDuplicateConnections(ctx, roomId)

  // 6. 추가·알림
  addAndAnnounce(ctx, msg, roomId, replacedDuplicate)
}

/**
 * 3단계 정원(관람 포함, P3-12b): 방장은 항상 입장. 유예 중인 세션은 rooms에 남아 자리를 유지하고,
 * 같은 사용자의 기존 연결(5단계에서 대체됨)은 새 자리로 세지 않는다. 승인 보유자·자리 예약(유예 밖으로 끊긴 연주자, M2)은
 * 정원 검사를 건너뛴다. 로그인 사용자가 익명보다 우선이다(H1). 거부하면 에러를 보내고 false — 지금 있는 방은 그대로 둔다.
 */
function passCapacity(ctx: Ctx<MembershipSvc>, roomId: string, plan: JoinPlan): boolean {
  const { ws, client, id } = ctx
  const { checkCapacity, hasSeatHold, clearSeatHold } = ctx.svc.capacity
  const { hasApproval } = ctx.svc.performers
  const { capacity } = plan

  const reserved = hasApproval(roomId, client.auth.userId) || hasSeatHold(roomId, client.auth.userId)
  const rejected = plan.isHost || reserved ? null : checkCapacity(roomId, client, capacity)
  if (rejected) {
    console.warn(`[JOIN] ${id.slice(0, 8)} rejected: room ${roomId.slice(0, 8)} ${rejected} (${capacity})`)
    const message = rejected === 'room-full'
      ? `합주실 정원(${capacity}명)이 가득 찼습니다.`
      : '로그인하지 않은 관람자 자리가 가득 찼습니다. 로그인하면 입장할 수 있습니다.'
    send(ws, { type: 'error', code: 'room-full', reason: rejected, roomId, maxParticipants: capacity, message })
    return false
  }
  clearSeatHold(roomId, client.auth.userId)
  return true
}

/** 4단계 이동: 다른 방에 있었다면 'move'로 퇴장 처리하고 그 방의 연주·엔진 상태를 지운다 */
function moveOutOfPreviousRoom(ctx: Ctx<MembershipSvc>, roomId: string): void {
  const { client, svc } = ctx
  if (!client.member.roomId || client.member.roomId === roomId) return
  handleLeave(svc, client, client.member.roomId, 'move')
  client.member.roomId = undefined
  client.perform.isPerforming = false
  client.perform.instrument = undefined
  svc.nativeSession.clearNative(client)
}

/**
 * 5단계 중복 정리: 같은 userId(서버 확정 값)의 이 방 기존 연결을 'duplicate'로 내보내고 상태까지 정리한다(L3).
 * 유예 중이던 연결이면 유예도 푼다(svc.resume.drop). 하나라도 대체했으면 true(서버 판정 rejoin 근거).
 */
function replaceDuplicateConnections(ctx: Ctx<MembershipSvc>, roomId: string): boolean {
  const { client, id, svc } = ctx
  const { getClient, roomMemberIds, removeClient } = svc.registry
  let replacedDuplicate = false
  if (!client.auth.userId) return replacedDuplicate
  for (const existingId of roomMemberIds(roomId)) {
    if (existingId === id) continue
    const existing = getClient(existingId)
    if (existing && existing.auth.userId === client.auth.userId) {
      console.log(`[JOIN] Removing duplicate connection for userId ${client.auth.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`)
      handleLeave(svc, existing, roomId, 'duplicate')
      existing.member.roomId = undefined // close 핸들러에서 중복 퇴장 처리 방지
      svc.resume.drop(existing)
      removeClient(existingId)
      // 사유 문자열은 클라이언트와의 계약: client/src/features/room/state/reconnectPolicy.ts DUPLICATE_CONNECTION_CLOSE_REASON
      // (클라이언트는 이 사유를 보고 "다른 창·기기에서 입장" 안내를 띄우며 자동 재연결하지 않는다). 바꿀 때는 양쪽을 함께 바꾼다
      existing.conn.ws.close(1000, 'Duplicate connection')
      replacedDuplicate = true
    }
  }
  return replacedDuplicate
}

/** 6단계 추가·알림: rejoin 판정·승인 유예 해제 → rooms 추가 → 참가자 수 갱신 → participant-joined → room-state */
function addAndAnnounce(ctx: Ctx<MembershipSvc>, msg: Msg, roomId: string, replacedDuplicate: boolean): void {
  const { client, id, svc } = ctx
  const { isInRoom, addToRoom, getRoomParticipants, broadcastToRoom } = svc.registry
  const { consumeRejoin } = svc.capacity
  const { releaseGraceOnRejoin } = svc.performers

  // rejoin 여부는 서버가 판단: 같은 사용자가 직전에 이 방에서 끊겼거나 중복 연결을 대체한 경우만
  const isRejoin = msg.isRejoin === true && (consumeRejoin(roomId, client.auth.userId) || replacedDuplicate)
  releaseGraceOnRejoin(roomId, client.auth.userId) // 재접속: 승인 유예 해제

  // 이 clientId가 이미 방에 있었는지 (세션 재개 후 클라이언트의 rejoin은 같은 id라서 true)
  const alreadyInRoom = isInRoom(roomId, id)
  client.member.roomId = roomId
  addToRoom(roomId, id)

  const participants = getRoomParticipants(roomId)
  console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.member.nickname}) joined room ${roomId.slice(0, 8)}. Room size: ${participants.length}${client.member.isHost ? ' [HOST]' : ''}${client.auth.state === 'anonymous' ? ' [ANON]' : ''}`)

  scheduleParticipantUpdate(svc, roomId)

  // 새 clientId가 방에 들어오면 rejoin이어도 항상 알린다 (H1: 다른 클라이언트가 새 id를 peers/관람자 목록에 넣어야
  // 방송 담당이 offer를 보낼 수 있다). rejoin 플래그는 UI 입장 알림 억제용으로만 함께 보낸다.
  if (!alreadyInRoom) {
    broadcastToRoom(roomId, id, {
      type: 'participant-joined',
      participant: { oderId: id, nickname: client.member.nickname, instrument: client.perform.instrument, isPerforming: client.perform.isPerforming, isHost: client.member.isHost },
      rejoin: isRejoin,
    })
  }

  sendRoomState(ctx, roomId)
}

/** `leave` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleLeaveMessage(ctx: Ctx<MembershipSvc>, _msg: Msg) {
  const { client, svc } = ctx
  const roomId = client.member.roomId
  const { clearNative } = svc.nativeSession
  if (!roomId) return

  handleLeave(svc, client, roomId, 'leave')
  client.member.roomId = undefined
  client.perform.isPerforming = false
  client.perform.instrument = undefined
  client.member.isHost = false
  clearNative(client)
  return
}
