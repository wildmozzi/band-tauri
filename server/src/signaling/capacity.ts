import { ipBucket } from '../lib/roomPassword'
import { APPROVAL_GRACE_MS, REJOIN_WINDOW_MS } from './protocol'
import type { Registry } from './registry'
import type { Client, LeaveReason, WsLimits } from './types'

/**
 * 정원·자리 예약·재입장 기록 (설계 docs/server-architecture.md 2.1절 E, 계층 2).
 * seatHolds·recentlyLeft의 소유자. 맵은 내보내지 않는다(S4-3): 입장은 checkCapacity·hasSeatHold·clearSeatHold·consumeRejoin, 퇴장은 onLeave.
 * 모든 함수는 동기다: handleJoin의 마지막 await 뒤 ~ rooms 추가 구간(1.2-2)에서 불린다.
 */
export function createCapacity(registry: Registry, limits: WsLimits) {
  const { roomMemberIds, getClient } = registry
  /** roomId → (userId → 만료 시각). 연결이 끊긴 사용자 (서버 판정 rejoin, M1) */
  const recentlyLeft = new Map<string, Map<string, number>>()
  /** roomId → (userId → 만료 시각). 유예 밖으로 끊긴 연주자·승인 보유자의 자리 예약: 이 동안 재입장은 정원 검사를 건너뛴다 (PR #23 M2) */
  const seatHolds = new Map<string, Map<string, number>>()

  /**
   * 방 정원 계산용 자리 수: 방 멤버(유예 중 세션 포함)에서 자기 자신과 같은 userId의 기존 연결(입장 시 대체됨)은 뺀다.
   * members = 로그인 사용자(userId 있음), anonymous = 익명 관람자, anonymousFromIp = 그중 같은 IP(/64)에서 온 연결.
   */
  function countRoomSeats(roomId: string, self: Client): { total: number; members: number; anonymous: number; anonymousFromIp: number } {
    const bucket = ipBucket(self.conn.ip)
    const seats = { total: 0, members: 0, anonymous: 0, anonymousFromIp: 0 }
    for (const pid of roomMemberIds(roomId)) {
      if (pid === self.id) continue
      const c = getClient(pid)
      if (self.auth.userId && c?.auth.userId === self.auth.userId) continue
      seats.total++
      if (c?.auth.userId) seats.members++
      else {
        seats.anonymous++
        if (c && ipBucket(c.conn.ip) === bucket) seats.anonymousFromIp++
      }
    }
    return seats
  }

  /**
   * 정원 검사 (P3-12b, PR #23 H1). 통과하면 null, 아니면 거부 사유.
   * - 로그인 사용자는 로그인 사용자 수로만 정원을 센다: 익명 관람자가 자리를 채워도 밴드 멤버·연주자는 막히지 않는다.
   * - 익명 관람자는 빈자리가 있을 때만(전체 < 정원), 방별 익명 상한·IP당 방별 익명 상한 안에서 들어온다.
   *   이미 있는 익명을 밀어내지 않으므로, 로그인 사용자가 뒤에 들어오면 전체 인원이 잠시 정원을 넘을 수 있다.
   */
  function checkCapacity(roomId: string, self: Client, capacity: number): 'room-full' | 'anonymous-limit' | null {
    const seats = countRoomSeats(roomId, self)
    if (self.auth.userId) return seats.members >= capacity ? 'room-full' : null
    if (seats.total >= capacity) return 'room-full'
    if (seats.anonymous >= limits.maxAnonymousPerRoom || seats.anonymousFromIp >= limits.maxAnonymousPerIpPerRoom) return 'anonymous-limit'
    return null
  }

  /** 자리 예약(유예 밖 재입장 우선권)이 살아 있는지. 확인만 하고 지우지 않는다 */
  function hasSeatHold(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const m = seatHolds.get(roomId)
    const exp = m?.get(userId)
    if (exp === undefined) return false
    if (exp > Date.now()) return true
    clearSeatHold(roomId, userId)
    return false
  }

  function clearSeatHold(roomId: string, userId: string | undefined) {
    if (!userId) return
    const m = seatHolds.get(roomId)
    m?.delete(userId)
    if (m && m.size === 0) seatHolds.delete(roomId)
  }

  function consumeRejoin(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const m = recentlyLeft.get(roomId)
    const exp = m?.get(userId)
    if (exp === undefined) return false
    m!.delete(userId)
    if (m!.size === 0) recentlyLeft.delete(roomId)
    return exp > Date.now()
  }

  /**
   * 퇴장 정리 단계 (membership.leave, 설계 2.2절). approved = performers.onLeave가 돌려준 "승인 기록이 남았나".
   * - 명시적 퇴장/방 이동: 자리 예약 삭제
   * - 연결 끊김: 연주 중이었거나 승인을 가진 사용자는 승인 유예와 같은 시간 동안 자리 예약 (PR #23 M2), 재입장 기록
   */
  function onLeave(client: Client, roomId: string, reason: LeaveReason, approved: boolean) {
    if (!client.auth.userId) return
    if (reason === 'leave' || reason === 'move') clearSeatHold(roomId, client.auth.userId)
    if (reason === 'close' && (client.perform.isPerforming || approved)) hold(roomId, client.auth.userId, Date.now() + APPROVAL_GRACE_MS)
    if (reason === 'close') noteLeft(roomId, client.auth.userId, Date.now() + REJOIN_WINDOW_MS)
  }

  /** 자리 예약: expiresAt까지 이 사용자의 재입장은 정원 검사를 건너뛴다 */
  function hold(roomId: string, userId: string, expiresAt: number) {
    if (!seatHolds.has(roomId)) seatHolds.set(roomId, new Map())
    seatHolds.get(roomId)!.set(userId, expiresAt)
  }

  /** 재입장 기록: expiresAt까지 이 사용자의 재입장은 서버 판정 rejoin이다 (consumeRejoin이 한 번 소비) */
  function noteLeft(roomId: string, userId: string, expiresAt: number) {
    if (!recentlyLeft.has(roomId)) recentlyLeft.set(roomId, new Map())
    recentlyLeft.get(roomId)!.set(userId, expiresAt)
  }

  /** 만료된 자리 예약 정리 (하트비트, L4) */
  function sweep(now: number) {
    for (const [roomId, holds] of seatHolds) {
      for (const [uid, exp] of holds) if (exp <= now) holds.delete(uid)
      if (holds.size === 0) seatHolds.delete(roomId)
    }
  }

  return { checkCapacity, hasSeatHold, hold, clearSeatHold, noteLeft, consumeRejoin, onLeave, sweep }
}

export type Capacity = ReturnType<typeof createCapacity>
