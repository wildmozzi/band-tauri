import type { Ctx } from './ctx'
import { handleLeave, sendRoomState, type MembershipSvc } from './membership'
import { metronomeHostChanged } from './metronome'
import { isAuthenticated, send, type Registry } from './registry'
import type { Client, WsLimits } from './types'

/**
 * 세션 재개 유예 (설계 docs/server-architecture.md 2.1절 C, 계층 3 — 계층 안 순서 membership < resume < auth).
 * `suspended`의 소유자. 소켓이 닫힌 인증 사용자를 유예시간 동안 clients·rooms에 남겨 두고(1.2-7),
 * 같은 userId가 resumeClientId로 다시 auth하면 새 연결을 옛 Client에 붙인다(1.2-6).
 *
 * S4-1: wsService 조립부의 close suspend 분기·finalizeSuspended·resumeSession을 옮겼다.
 * 재개는 auth가 attachSession → (auth 필드·만료 타이머) → announceResumed 순으로 부른다(원래 resumeSession 한 함수의 순서).
 * membership은 이 모듈을 import하지 않는다(순환). 중복 연결 정리는 svc.resume.drop으로 부른다(설계 2.2).
 */
export function createResume(registry: Registry) {
  const { clients } = registry
  /** clientId → 연결이 끊겨 재개를 기다리는 클라이언트 (방/연주 상태 유지) */
  const suspended = new Map<string, Client>()

  /** 유예 해제: 타이머를 끄고 목록에서 뺀다 (재개·유예 만료·중복 연결 정리) */
  function drop(c: Client) {
    if (c.suspendTimer) clearTimeout(c.suspendTimer)
    c.suspendTimer = undefined
    suspended.delete(c.id)
  }

  /** 재개 대상: 이전 clientId의 주인이 같은 userId이고 유예시간 안(suspended에 남아 있음)일 때만 */
  function find(clientId: string, userId: string): Client | null {
    const old = suspended.get(clientId)
    return old && old.userId === userId && clients.get(old.id) === old ? old : null
  }

  function isSuspended(clientId: string): boolean {
    return suspended.has(clientId)
  }

  function add(c: Client, timer: ReturnType<typeof setTimeout>) {
    suspended.set(c.id, c)
    c.suspendTimer = timer
  }

  /** 서버 종료: 유예 타이머 정리 */
  function clearTimers() {
    for (const c of suspended.values()) if (c.suspendTimer) clearTimeout(c.suspendTimer)
  }

  return { drop, find, isSuspended, add, clearTimers }
}

export type Resume = ReturnType<typeof createResume>

/** resume이 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface ResumeSvc extends MembershipSvc {
  resume: Resume
  limits: WsLimits
}

/** 유예 중인 클라이언트를 최종 제거 (방 참가자에게 leave 전송) */
function finalizeSuspended(svc: ResumeSvc, c: Client) {
  const { rooms, clients, removeClient } = svc.registry
  svc.resume.drop(c)
  if (clients.get(c.id) !== c) return
  console.log(`[RESUME] ${c.id.slice(0, 8)} grace period over - leaving`)
  if (c.roomId && rooms.has(c.roomId)) handleLeave(svc, c, c.roomId, 'close')
  c.roomId = undefined
  removeClient(c.id)
}

/**
 * 소켓 닫힘(close 핸들러): 인증된 사용자가 방 안에서 끊겼으면 유예시간 동안 상태를 유지한다(참가자에게 leave 안 보냄).
 * 유예에 들어갔으면 true — 호출부는 퇴장 처리를 하지 않는다.
 */
export function suspendOnClose(svc: ResumeSvc, client: Client): boolean {
  const { rooms } = svc.registry
  const { limits } = svc
  const roomId = client.roomId
  if (!(roomId && rooms.has(roomId) && isAuthenticated(client) && limits.resumeGraceMs > 0)) return false
  const c = client
  const timer = setTimeout(() => finalizeSuspended(svc, c), limits.resumeGraceMs)
  timer.unref?.()
  svc.resume.add(c, timer)
  console.log(`[RESUME] ${c.id.slice(0, 8)} suspended for ${limits.resumeGraceMs}ms`)
  return true
}

/**
 * 세션 재개 1단계: 이 연결(ctx)을 유예 중이던 이전 Client(clientId/방/연주 상태)에 붙인다.
 * 연결 상태(소켓·체인·대기 수·버킷)를 옛 Client로 옮기고, 임시 Client를 지우고, ctx.client·ctx.id를 바꿔 끼운다(1.2-6).
 * 그 뒤로 이 연결의 메시지 체인·close·auth 타이머는 모두 ctx를 거쳐 옛 Client를 본다.
 * 인증 필드(authState·tokenExp·만료 타이머)는 auth가 이어서 정한다. 임시 Client를 돌려준다(그 만료 타이머 정리용).
 */
export function attachSession(ctx: Ctx<ResumeSvc>, old: Client): Client {
  const { ws } = ctx
  const { removeClient } = ctx.svc.registry
  const temp = ctx.client
  ctx.svc.resume.drop(old)

  old.ws = ws
  old.ip = temp.ip
  old.isAlive = true
  // 이 연결에서 이미 큐에 쌓인 메시지 순서를 유지하기 위해 체인/카운터를 넘겨받는다
  old.chain = temp.chain
  old.pending = temp.pending
  old.msgBucket = temp.msgBucket
  old.chatBucket = temp.chatBucket
  old.joinBucket = temp.joinBucket
  old.dropped = 0

  removeClient(temp.id)
  ctx.client = old
  ctx.id = old.id
  return temp
}

/**
 * 세션 재개 2단계: 재개 알림. 방 참가자에게는 leave/joined를 보내지 않는다(그들 입장에서는 끊긴 적이 없음).
 * 순서: welcome(resumed) → auth-result(resumed) → room-state → (유예 중 이양됐으면) metronome-host-only.
 */
export function announceResumed(ctx: Ctx<ResumeSvc>) {
  const { ws, client, id } = ctx
  const { rooms } = ctx.svc.registry
  console.log(`[RESUME] ${id.slice(0, 8)} resumed session (user ${client.userId?.slice(0, 8)}, room ${client.roomId?.slice(0, 8) || 'none'}, performing: ${client.isPerforming})`)
  send(ws, { type: 'welcome', clientId: id, resumed: true })
  send(ws, { type: 'auth-result', authenticated: true, userId: client.userId ?? null, resumed: true })
  if (client.roomId && rooms.has(client.roomId)) sendRoomState(ctx, client.roomId)
  // 유예 중에 이양으로 메트로놈이 꺼졌다: 그때 못 받은 알림을 지금 보내 내 기준 클릭도 끄게 한다(PR #53 L2)
  if (client.pendingMetronomeStop) {
    client.pendingMetronomeStop = false
    send(ws, metronomeHostChanged())
  }
}
