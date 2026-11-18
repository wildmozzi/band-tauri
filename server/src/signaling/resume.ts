import { createHash, randomBytes, timingSafeEqual } from 'crypto'
import type { WebSocket } from 'ws'
import { ipBucket } from '../lib/roomPassword'
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
 *
 * P1-16: 서버가 옛 소켓의 닫힘을 아직 모르는(반쯤 열린) 연결도 재개 대상이다(findLive). 새 연결을 붙인 뒤
 * 옛 소켓을 끊는다(attachSession). 옛 소켓의 close는 `client.conn.ws`가 이미 새 소켓이라 "대체됨"으로 보고 무시한다(조립부).
 *
 * P2-18c: 익명 관람자도 재개한다. 신원(JWT)이 없으므로 서버가 익명 auth-result에 실어 보낸 1회용 재개 토큰
 * (무작위 256비트, 서버는 SHA-256만 쥔다)과 그 연결이 있던 방이 맞을 때만 같은 clientId로 붙인다(findAnon).
 * 재개 토큰은 익명 auth-result를 보낼 때마다 새로 만들어 이전 것을 무효로 한다(재개 성공 = 회전).
 * 익명 유예는 따로 상한을 둔다(서버 전체 maxSuspendedAnonymous, IP(/64)별 maxSuspendedAnonymousPerIp) — 넘으면 바로 퇴장.
 */
export function createResume(registry: Registry) {
  const { getClient } = registry
  /** clientId → 연결이 끊겨 재개를 기다리는 클라이언트 (방/연주 상태 유지) */
  const suspended = new Map<string, Client>()

  /** 유예 해제: 타이머를 끄고 목록에서 뺀다 (재개·유예 만료·중복 연결 정리) */
  function drop(c: Client) {
    if (c.resume.timer) clearTimeout(c.resume.timer)
    c.resume.timer = undefined
    suspended.delete(c.id)
  }

  /** 재개 대상: 이전 clientId의 주인이 같은 userId이고 유예시간 안(suspended에 남아 있음)일 때만 */
  function find(clientId: string, userId: string): Client | null {
    const old = suspended.get(clientId)
    return old && old.auth.userId === userId && getClient(old.id) === old ? old : null
  }

  /**
   * 재개 대상(P1-16): 유예 중이 아니어도 같은 userId(JWT)의 Client가 살아 있으면 — 서버가 옛 소켓의 닫힘을 아직 모를 때.
   * 클라이언트는 수신 없음(15초 + ping 5초)으로 먼저 끊김을 알고 재연결하지만 서버 하트비트는 최대 60초 뒤에야 안다.
   * 인증된 연결만(userId는 auth가 JWT로만 정한다). 이 연결 자신(self)은 제외.
   */
  function findLive(clientId: string, userId: string, self: WebSocket): Client | null {
    const c = getClient(clientId)
    if (!c || suspended.has(clientId)) return null
    return c.auth.state === 'authenticated' && c.auth.userId === userId && c.conn.ws !== self ? c : null
  }

  /**
   * 익명 재개 대상(P2-18c): 이전 clientId의 Client가 익명(JWT 없음)이고, 재개 토큰과 방이 맞고,
   * 유예 중이거나(P1-16처럼) 서버가 아직 닫힘을 모르는 다른 소켓의 연결일 때만.
   * clientId는 같은 방 참가자에게 공개(oderId)되므로 토큰이 신원 증명이다. 틀린 시도는 토큰을 무효로 하지 않는다
   * (clientId를 아는 사람이 일부러 틀려 남의 재개를 막지 못하게). 256비트라 추측은 불가능하다.
   */
  function findAnon(clientId: string, token: string, roomId: string, self: WebSocket): Client | null {
    const c = getClient(clientId)
    if (!c || c.auth.state !== 'anonymous' || c.auth.userId || !c.resume.anonTokenHash) return null
    if (!c.member.roomId || c.member.roomId !== roomId) return null
    if (!suspended.has(clientId) && c.conn.ws === self) return null
    return timingSafeEqual(hashToken(token), c.resume.anonTokenHash) ? c : null
  }

  /** 익명 재개 토큰을 새로 만든다(이전 것은 무효). 원문은 돌려주기만 하고 Client에는 해시만 둔다 */
  function issueAnonToken(c: Client): string {
    const token = randomBytes(32).toString('base64url')
    c.resume.anonTokenHash = hashToken(token)
    return token
  }

  /** 유예 중인 익명 연결 수 (서버 전체, 이 IP(/64)) — 익명 유예 상한 판정용 */
  function suspendedAnonymous(ip: string): { total: number; fromIp: number } {
    const key = ipBucket(ip)
    let total = 0
    let fromIp = 0
    for (const c of suspended.values()) {
      if (c.auth.userId) continue
      total++
      if (ipBucket(c.conn.ip) === key) fromIp++
    }
    return { total, fromIp }
  }

  function isSuspended(clientId: string): boolean {
    return suspended.has(clientId)
  }

  function add(c: Client, timer: ReturnType<typeof setTimeout>) {
    suspended.set(c.id, c)
    c.resume.timer = timer
  }

  /** 서버 종료: 유예 타이머 정리 */
  function clearTimers() {
    for (const c of suspended.values()) if (c.resume.timer) clearTimeout(c.resume.timer)
  }

  return { drop, find, findLive, findAnon, issueAnonToken, suspendedAnonymous, isSuspended, add, clearTimers }
}

function hashToken(token: string): Buffer {
  return createHash('sha256').update(token).digest()
}

export type Resume = ReturnType<typeof createResume>

/** resume이 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface ResumeSvc extends MembershipSvc {
  resume: Resume
  limits: WsLimits
}

/** 유예 중인 클라이언트를 최종 제거 (방 참가자에게 leave 전송) */
function finalizeSuspended(svc: ResumeSvc, c: Client) {
  const { hasRoom, getClient, removeClient } = svc.registry
  svc.resume.drop(c)
  if (getClient(c.id) !== c) return
  console.log(`[RESUME] ${c.id.slice(0, 8)} grace period over - leaving`)
  if (c.member.roomId && hasRoom(c.member.roomId)) handleLeave(svc, c, c.member.roomId, 'close')
  c.member.roomId = undefined
  removeClient(c.id)
}

/**
 * 끊긴 연결의 유예시간(ms). 0이면 유예하지 않는다.
 * 인증 사용자는 resumeGraceMs. 익명(P2-18c)은 재개 토큰을 받은 연결만 anonResumeGraceMs, 익명 유예 상한(전체·IP별) 안에서.
 */
function graceFor(svc: ResumeSvc, c: Client): number {
  const { limits } = svc
  if (isAuthenticated(c)) return limits.resumeGraceMs
  if (c.auth.state !== 'anonymous' || c.auth.userId || !c.resume.anonTokenHash || limits.anonResumeGraceMs <= 0) return 0
  const n = svc.resume.suspendedAnonymous(c.conn.ip)
  if (n.total >= limits.maxSuspendedAnonymous || n.fromIp >= limits.maxSuspendedAnonymousPerIp) {
    console.warn(`[RESUME] ${c.id.slice(0, 8)} anonymous suspend limit reached (${n.total} total, ${n.fromIp} from ip) - leaving now`)
    return 0
  }
  return limits.anonResumeGraceMs
}

/**
 * 소켓 닫힘(close 핸들러): 인증된 사용자(또는 재개 토큰을 받은 익명 관람자, P2-18c)가 방 안에서 끊겼으면
 * 유예시간 동안 상태를 유지한다(참가자에게 leave 안 보냄). 유예에 들어갔으면 true — 호출부는 퇴장 처리를 하지 않는다.
 */
export function suspendOnClose(svc: ResumeSvc, client: Client): boolean {
  const { hasRoom } = svc.registry
  const roomId = client.member.roomId
  if (!(roomId && hasRoom(roomId))) return false
  const graceMs = graceFor(svc, client)
  if (graceMs <= 0) return false
  const c = client
  const timer = setTimeout(() => finalizeSuspended(svc, c), graceMs)
  timer.unref?.()
  svc.resume.add(c, timer)
  console.log(`[RESUME] ${c.id.slice(0, 8)} ${isAuthenticated(c) ? '' : 'anonymous '}suspended for ${graceMs}ms`)
  return true
}

/**
 * 세션 재개 1단계: 이 연결(ctx)을 유예 중이던(또는 P1-16: 아직 닫힘을 모르는 옛 소켓에 붙어 있던) 이전 Client(clientId/방/연주 상태)에 붙인다.
 * 연결 상태(소켓·체인·대기 수·버킷)를 옛 Client로 옮기고, 임시 Client를 지우고, ctx.client·ctx.id를 바꿔 끼운다(1.2-6).
 * 그 뒤로 이 연결의 메시지 체인·close·auth 타이머는 모두 ctx를 거쳐 옛 Client를 본다.
 * 인증 필드(authState·tokenExp·만료 타이머)는 auth가 이어서 정한다. 임시 Client를 돌려준다(그 만료 타이머 정리용).
 * 옛 소켓이 아직 열려 있으면(P1-16) 새 소켓으로 바꿔 끼운 뒤 terminate한다. 그 close 핸들러는 conn.ws가 자기 소켓이 아니므로
 * 유예·퇴장을 일으키지 않는다(같은 방 참가자에게 participant-left/joined 없음).
 */
export function attachSession(ctx: Ctx<ResumeSvc>, old: Client): Client {
  const { ws } = ctx
  const { removeClient } = ctx.svc.registry
  const temp = ctx.client
  ctx.svc.resume.drop(old)

  const prevWs = old.conn.ws
  old.conn.ws = ws
  old.conn.ip = temp.conn.ip
  old.conn.isAlive = true
  // 이 연결에서 이미 큐에 쌓인 메시지 순서를 유지하기 위해 체인/카운터를 넘겨받는다
  old.conn.chain = temp.conn.chain
  old.conn.pending = temp.conn.pending
  old.conn.pendingRelay = temp.conn.pendingRelay
  old.conn.pendingRelayBytes = temp.conn.pendingRelayBytes
  old.conn.msgBucket = temp.conn.msgBucket
  old.conn.relayBucket = temp.conn.relayBucket
  old.conn.chatBucket = temp.conn.chatBucket
  old.conn.joinBucket = temp.conn.joinBucket
  old.conn.authBucket = temp.conn.authBucket
  old.conn.dropped = 0

  removeClient(temp.id)
  ctx.client = old
  ctx.id = old.id
  // P1-16: 반쯤 열린 옛 소켓. 유예에서 온 경우는 이미 닫혀 있어 아무 일도 없다
  if (prevWs !== ws && prevWs.readyState !== prevWs.CLOSED) {
    console.log(`[RESUME] ${old.id.slice(0, 8)} taking over a live connection - terminating the old socket`)
    prevWs.terminate()
  }
  return temp
}

/**
 * 세션 재개 2단계: 재개 알림. 방 참가자에게는 leave/joined를 보내지 않는다(그들 입장에서는 끊긴 적이 없음).
 * 순서: welcome(resumed) → auth-result(resumed) → room-state → (유예 중 이양됐으면) metronome-host-only
 * → (유예 중 관람자로 내려졌으면) demoted-to-viewer.
 * 익명 재개(P2-18c)는 auth-result가 authenticated:false에 새 재개 토큰(resumeToken)을 싣는다 — 쓴 토큰은 이로써 무효가 된다.
 */
export function announceResumed(ctx: Ctx<ResumeSvc>) {
  const { ws, client, id } = ctx
  const { hasRoom } = ctx.svc.registry
  const anonymous = client.auth.state === 'anonymous'
  console.log(`[RESUME] ${id.slice(0, 8)} resumed session (${anonymous ? 'anonymous' : `user ${client.auth.userId?.slice(0, 8)}`}, room ${client.member.roomId?.slice(0, 8) || 'none'}, performing: ${client.perform.isPerforming})`)
  send(ws, { type: 'welcome', clientId: id, resumed: true })
  send(ws, anonymous
    ? { type: 'auth-result', authenticated: false, userId: null, resumed: true, resumeToken: ctx.svc.resume.issueAnonToken(client) }
    : { type: 'auth-result', authenticated: true, userId: client.auth.userId ?? null, resumed: true })
  if (client.member.roomId && hasRoom(client.member.roomId)) sendRoomState(ctx, client.member.roomId)
  // 유예 중에 이양으로 메트로놈이 꺼졌다: 그때 못 받은 알림을 지금 보내 내 기준 클릭도 끄게 한다(PR #53 L2)
  if (client.resume.pendingMetronomeStop) {
    client.resume.pendingMetronomeStop = false
    send(ws, metronomeHostChanged())
  }
  // 유예 중에 방장이 관람자로 내렸다(P2-9): 그때 못 받은 알림을 지금 보내 엔진 연주를 멈추게 한다
  if (client.resume.pendingDemoted) {
    client.resume.pendingDemoted = false
    send(ws, { type: 'demoted-to-viewer' })
  }
}
