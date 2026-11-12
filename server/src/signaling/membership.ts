import type { Capacity } from './capacity'
import type { Ctx } from './ctx'
import type { Metronome } from './metronome'
import type { NativeSession } from './nativeSession'
import type { Password } from './password'
import type { Performers } from './performers'
import {
  APPROVAL_GRACE_MS, MAX_ROOM_ID_LENGTH, PARTICIPANT_UPDATE_DEBOUNCE_MS, REJOIN_WINDOW_MS,
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
 * handleJoin의 마지막 await(비밀번호 검증) 뒤 ~ addToRoom 구간은 동기다(1.2-2: 동시 입장이 정원을 넘지 못함).
 * 이 구간에 await를 넣지 않는다 — room-capacity.test.ts "concurrent joins never exceed the capacity".
 * 단계 함수 분해와 leave() 호출 순서 정리는 S4.
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
  const { rooms } = svc.registry

  if (!deps.hasBackend || participantTimers.has(roomId)) return
  const t = setTimeout(() => {
    participantTimers.delete(roomId)
    void deps.updateParticipants(roomId, rooms.get(roomId)?.size ?? 0)
  }, PARTICIPANT_UPDATE_DEBOUNCE_MS)
  t.unref?.()
  participantTimers.set(roomId, t)
}

/** 퇴장 처리. 실행 순서가 클라이언트가 받는 메시지 순서다(1.2-1) */
export function handleLeave(svc: MembershipSvc, client: Client, roomId: string, reason: LeaveReason) {
  const { rooms, clients, removeFromRoom, findRoomHost } = svc.registry
  const { approvals, setApproval, clearApproval } = svc.performers
  const { seatHolds, recentlyLeft, clearSeatHold } = svc.capacity
  const { performRequests, instrumentChangeRequests } = svc.requests
  const { metronomes } = svc.metronome
  const roomMeta = svc.roomMeta

  const id = client.id
  const set = rooms.get(roomId)
  if (!set) return

  removeFromRoom(roomId, id)
  console.log(`[LEAVE] Client ${id.slice(0, 8)} left room (${reason}). Room size: ${set.size}`)
  scheduleParticipantUpdate(svc, roomId)

  // 승인 기록: 명시적 퇴장/방 이동은 즉시 삭제, 연결 끊김은 재접속 유예 후 만료, 중복 연결 정리는 유지
  if (client.userId) {
    if (reason === 'leave' || reason === 'move') {
      clearApproval(roomId, client.userId)
      clearSeatHold(roomId, client.userId)
    } else if (reason === 'close' && approvals.get(roomId)?.has(client.userId)) {
      setApproval(roomId, client.userId, Date.now() + APPROVAL_GRACE_MS)
    }
    // 연주 중이었거나 승인을 가진 사용자가 끊기면 승인 유예와 같은 시간 동안 자리를 예약한다 (PR #23 M2)
    if (reason === 'close' && (client.isPerforming || approvals.get(roomId)?.has(client.userId))) {
      if (!seatHolds.has(roomId)) seatHolds.set(roomId, new Map())
      seatHolds.get(roomId)!.set(client.userId, Date.now() + APPROVAL_GRACE_MS)
    }
    if (reason === 'close') {
      if (!recentlyLeft.has(roomId)) recentlyLeft.set(roomId, new Map())
      recentlyLeft.get(roomId)!.set(client.userId, Date.now() + REJOIN_WINDOW_MS)
    }
  }

  const host = findRoomHost(roomId)

  const requests = performRequests.get(roomId)
  if (requests) {
    const idx = requests.findIndex(r => r.oderId === id)
    if (idx !== -1) {
      requests.splice(idx, 1)
      if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
    }
  }

  const changeRequests = instrumentChangeRequests.get(roomId)
  if (changeRequests) {
    const idx = changeRequests.findIndex(r => r.oderId === id)
    if (idx !== -1) {
      changeRequests.splice(idx, 1)
      if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
    }
  }

  for (const oderId of set) {
    const peer = clients.get(oderId)
    if (peer) send(peer.ws, { type: 'participant-left', oderId: id })
  }
  // 방장이 나가면 메트로놈 시각 기준(방장 엔진 시계)이 사라진다. 따르는 쪽은 participant-left로 스스로 멈춘다
  if (metronomes.get(roomId)?.hostId === id) metronomes.delete(roomId)

  if (set.size === 0) {
    rooms.delete(roomId)
    performRequests.delete(roomId)
    metronomes.delete(roomId)
    instrumentChangeRequests.delete(roomId)
    roomMeta.forget(roomId)
  }
}

/** 현재 방 상태를 이 연결에 전송 (방장이면 대기 중인 요청도) */
export function sendRoomState(ctx: Ctx<MembershipSvc>, roomId: string) {
  const { ws, client, id } = ctx
  const { getRoomParticipants } = ctx.svc.registry
  const { performRequests, instrumentChangeRequests } = ctx.svc.requests

  const others = getRoomParticipants(roomId).filter(p => p.oderId !== id)
  send(ws, {
    type: 'room-state',
    participants: others,
    peerIds: others.map(p => p.oderId),
    isHost: client.isHost,
  })
  if (client.isHost) {
    for (const request of performRequests.get(roomId) || []) send(ws, { type: 'perform-request-received', request })
    for (const request of instrumentChangeRequests.get(roomId) || []) send(ws, { type: 'instrument-change-request-received', request })
  }
}

/** `join` — S3: 연결 클로저의 handleJoin 본문을 순서 그대로 옮겼다 */
export async function handleJoin(ctx: Ctx<MembershipSvc>, msg: Msg) {
  const { ws, client, id, svc } = ctx
  const { rooms, clients, removeClient, addToRoom, getRoomParticipants, broadcastToRoom } = svc.registry
  const { getRoomMetaOrNull } = svc.roomMeta
  const { checkCapacity, hasSeatHold, clearSeatHold, consumeRejoin } = svc.capacity
  const { checkRoomPassword } = svc.password
  const { approvals, hasApproval, setApproval } = svc.performers
  const { dropMetronomeIfHostChanged } = svc.metronome
  const { clearNative } = svc.nativeSession
  const { deps, limits, insecureDevMode, resume } = svc

  const roomId = msg.roomId
  if (!isNonEmptyString(roomId, MAX_ROOM_ID_LENGTH)) return

  // 개발 모드에서만 클라이언트 userId 사용 (그 외에는 auth로 확정된 값만)
  if (insecureDevMode && client.authState === 'authenticated' && !client.userId && isNonEmptyString(msg.userId, 128)) {
    client.userId = msg.userId
  }

  // 방 존재 확인 + 방장 판정 (백엔드가 있으면 DB 기준)
  let isHost = false
  let passwordHash: string | null = null
  let capacity = limits.maxRoomParticipants
  if (deps.hasBackend) {
    const meta = await getRoomMetaOrNull(roomId, 0)
    if (!meta) {
      send(ws, { type: 'error', code: 'room-not-found', message: '방을 찾을 수 없습니다.' })
      return
    }
    isHost = !!client.userId && meta.hostId === client.userId
    // 방금 읽은 DB 방장이 메트로놈을 켠 사람과 다르면(이양) 진행 중인 메트로놈을 멈춘다(PR #36 L1)
    dropMetronomeIfHostChanged(roomId, meta.hostId ?? null)
    passwordHash = meta.passwordHash ?? null
    if (meta.maxParticipants !== undefined) capacity = Math.min(capacity, meta.maxParticipants)
    // 비밀번호 방으로 표시됐는데 해시가 아직 없음(생성·변경 중): 방장 외 입장 보류 (fail-closed, 리뷰 L1)
    if (meta.hasPassword && !passwordHash && !isHost) {
      send(ws, { type: 'error', code: 'password-busy', roomId, retryAfterMs: 1000, message: '합주실을 준비 중입니다. 잠시 후 다시 시도하세요.' })
      return
    }
  } else if (insecureDevMode && client.authState === 'authenticated') {
    isHost = msg.isHost === true
  }
  // DB 조회 중 연결이 끊겼으면 중단
  if (clients.get(id) !== client) return

  // 이미 이 방에 있음 (세션 재개 후 클라이언트의 rejoin 등): 상태만 다시 보내고 브로드캐스트 없음
  if (client.roomId === roomId && rooms.get(roomId)?.has(id)) {
    client.isHost = isHost
    const nickname = sanitizeNickname(msg.nickname)
    if (nickname) client.nickname = nickname
    sendRoomState(ctx, roomId)
    return
  }

  // 비밀번호 방: 방장이 아니면 검증 (실패하면 지금 있는 방은 그대로 둔다)
  if (passwordHash && !isHost) {
    const ok = await checkRoomPassword(ws, client, roomId, passwordHash, msg.password)
    if (!ok || clients.get(id) !== client) return
  }

  // 정원(관람 포함, P3-12b): 방장은 항상 입장. 마지막 await 뒤부터 방에 추가할 때까지 동기 구간이라 동시 입장 경합이 없다.
  // 유예 중인 세션은 rooms에 남아 자리를 유지하고, 같은 사용자의 기존 연결(아래에서 대체됨)은 새 자리로 세지 않는다.
  // 승인 보유자·자리 예약(유예 밖으로 끊긴 연주자, M2)은 정원 검사를 건너뛴다. 로그인 사용자가 익명보다 우선이다(H1).
  // 실패해도 지금 있는 방은 그대로 둔다.
  const reserved = hasApproval(roomId, client.userId) || hasSeatHold(roomId, client.userId)
  const rejected = isHost || reserved ? null : checkCapacity(roomId, client, capacity)
  if (rejected) {
    console.warn(`[JOIN] ${id.slice(0, 8)} rejected: room ${roomId.slice(0, 8)} ${rejected} (${capacity})`)
    const message = rejected === 'room-full'
      ? `합주실 정원(${capacity}명)이 가득 찼습니다.`
      : '로그인하지 않은 관람자 자리가 가득 찼습니다. 로그인하면 입장할 수 있습니다.'
    send(ws, { type: 'error', code: 'room-full', reason: rejected, roomId, maxParticipants: capacity, message })
    return
  }
  clearSeatHold(roomId, client.userId)

  // 다른 방에 있었다면 먼저 퇴장 처리
  if (client.roomId && client.roomId !== roomId) {
    handleLeave(svc, client, client.roomId, 'move')
    client.roomId = undefined
    client.isPerforming = false
    client.instrument = undefined
    clearNative(client)
  }
  client.isHost = isHost

  const nickname = sanitizeNickname(msg.nickname)
  if (nickname) client.nickname = nickname

  // 같은 userId 중복 연결 제거 (서버 확정 userId 기준) — 상태 정리까지 수행 (L3)
  let replacedDuplicate = false
  if (client.userId) {
    for (const existingId of [...(rooms.get(roomId) || [])]) {
      if (existingId === id) continue
      const existing = clients.get(existingId)
      if (existing && existing.userId === client.userId) {
        console.log(`[JOIN] Removing duplicate connection for userId ${client.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`)
        handleLeave(svc, existing, roomId, 'duplicate')
        existing.roomId = undefined // close 핸들러에서 중복 퇴장 처리 방지
        resume.drop(existing)
        removeClient(existingId)
        existing.ws.close(1000, 'Duplicate connection')
        replacedDuplicate = true
      }
    }
  }

  // rejoin 여부는 서버가 판단: 같은 사용자가 직전에 이 방에서 끊겼거나 중복 연결을 대체한 경우만
  const isRejoin = msg.isRejoin === true && (consumeRejoin(roomId, client.userId) || replacedDuplicate)
  if (client.userId && approvals.get(roomId)?.has(client.userId) && hasApproval(roomId, client.userId)) {
    setApproval(roomId, client.userId, null) // 재접속: 승인 유예 해제
  }

  // 이 clientId가 이미 방에 있었는지 (세션 재개 후 클라이언트의 rejoin은 같은 id라서 true)
  const alreadyInRoom = rooms.get(roomId)?.has(id) ?? false
  client.roomId = roomId
  addToRoom(roomId, id)

  const participants = getRoomParticipants(roomId)
  console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.nickname}) joined room ${roomId.slice(0, 8)}. Room size: ${participants.length}${client.isHost ? ' [HOST]' : ''}${client.authState === 'anonymous' ? ' [ANON]' : ''}`)

  scheduleParticipantUpdate(svc, roomId)

  // 새 clientId가 방에 들어오면 rejoin이어도 항상 알린다 (H1: 다른 클라이언트가 새 id를 peers/관람자 목록에 넣어야
  // 방송 담당이 offer를 보낼 수 있다). rejoin 플래그는 UI 입장 알림 억제용으로만 함께 보낸다.
  if (!alreadyInRoom) {
    broadcastToRoom(roomId, id, {
      type: 'participant-joined',
      participant: { oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: client.isPerforming, isHost: client.isHost },
      rejoin: isRejoin,
    })
  }

  sendRoomState(ctx, roomId)
}

/** `leave` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 (분기 조건 `&& roomId`는 첫 줄로) */
export function handleLeaveMessage(ctx: Ctx<MembershipSvc>, _msg: Msg) {
  const { client, svc } = ctx
  const roomId = client.roomId
  const { clearNative } = svc.nativeSession
  if (!roomId) return

  handleLeave(svc, client, roomId, 'leave')
  client.roomId = undefined
  client.isPerforming = false
  client.instrument = undefined
  client.isHost = false
  clearNative(client)
  return
}
