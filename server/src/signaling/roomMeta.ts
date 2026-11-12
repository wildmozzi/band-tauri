import type { RoomMeta } from '../repositories/roomRepository'
import { ROOM_META_TTL_MS } from './protocol'
import type { Client, WsDeps } from './types'

/**
 * 방 정보·방장 판정 (설계 docs/server-architecture.md 2.1절 D, 계층 1).
 * roomMetaCache·roomMetaInflight의 소유자. join은 maxAgeMs 0으로 캐시 없이 새로 읽는다.
 */
export function createRoomMeta(deps: WsDeps, insecureDevMode: boolean) {
  const roomMetaCache = new Map<string, { meta: RoomMeta | null; fetchedAt: number }>()
  /** roomId → 진행 중인 DB 조회 (동시에 여러 연결이 같은 방을 물으면 한 번만 조회, PR #53 L3) */
  const roomMetaInflight = new Map<string, Promise<RoomMeta | null>>()

  /**
   * 방 정보(캐시 maxAgeMs). null = 방 없음. 조회 실패는 throw하고 캐시하지 않는다(PR #53 H1).
   * maxAgeMs > 0이면 진행 중인 조회를 함께 기다린다(방금 시작한 조회라 나이 조건을 만족, L3).
   * maxAgeMs 0(join)은 요청 뒤에 시작한 조회만 믿는다.
   */
  async function getRoomMeta(roomId: string, maxAgeMs = ROOM_META_TTL_MS): Promise<RoomMeta | null> {
    const cached = roomMetaCache.get(roomId)
    if (cached && Date.now() - cached.fetchedAt <= maxAgeMs) return cached.meta
    const inflight = roomMetaInflight.get(roomId)
    if (inflight && maxAgeMs > 0) return inflight
    const p = deps.fetchRoomMeta(roomId).then(meta => {
      roomMetaCache.set(roomId, { meta, fetchedAt: Date.now() })
      return meta
    })
    roomMetaInflight.set(roomId, p)
    const clear = () => { if (roomMetaInflight.get(roomId) === p) roomMetaInflight.delete(roomId) }
    p.then(clear, clear)
    return p
  }

  /** getRoomMeta, 조회 실패는 로그 후 "방 없음"으로(입장·연주 허용 등 fail-closed 경로 — 기존 동작) */
  async function getRoomMetaOrNull(roomId: string, maxAgeMs = ROOM_META_TTL_MS): Promise<RoomMeta | null> {
    try {
      return await getRoomMeta(roomId, maxAgeMs)
    } catch (err) {
      console.warn('[DB] room lookup failed:', err instanceof Error ? err.message : err)
      return null
    }
  }

  /**
   * 방장 여부 재확인 (L2). 개발 모드에서는 join 때 값 유지.
   * hostUserId = DB 방장(방이 없으면 null). DB 조회가 실패하면 마지막으로 읽은 값(나이 무관)으로 권한만 판단하고
   * stale=true — 이때는 이양 판단(메트로놈 정지)을 하지 않는다(PR #53 H1). 읽은 적도 없으면 권한 없음.
   */
  async function checkHost(client: Client, maxAgeMs = ROOM_META_TTL_MS): Promise<{ isHost: boolean; hostUserId: string | null; stale: boolean }> {
    if (!client.member.roomId || insecureDevMode) return { isHost: client.member.isHost, hostUserId: null, stale: true }
    const roomId = client.member.roomId
    let meta: RoomMeta | null
    let stale = false
    try {
      meta = await getRoomMeta(roomId, maxAgeMs)
    } catch (err) {
      console.warn('[DB] host check failed:', err instanceof Error ? err.message : err)
      meta = roomMetaCache.get(roomId)?.meta ?? null
      stale = true
    }
    client.member.isHost = !!client.auth.userId && !!meta && meta.hostId === client.auth.userId
    return { isHost: client.member.isHost, hostUserId: meta?.hostId ?? null, stale }
  }

  async function refreshHost(client: Client): Promise<boolean> {
    return (await checkHost(client)).isHost
  }

  /** 방이 비었을 때: 캐시와 진행 중 조회 기록을 지운다 */
  function forget(roomId: string) {
    roomMetaCache.delete(roomId)
    roomMetaInflight.delete(roomId)
  }

  return { getRoomMeta, getRoomMetaOrNull, checkHost, refreshHost, forget }
}

export type RoomMetaStore = ReturnType<typeof createRoomMeta>
