import { isAuthenticated } from './registry'
import type { RoomMetaStore } from './roomMeta'
import type { Client } from './types'

/**
 * 연주 권한·승인 기록 (설계 docs/server-architecture.md 2.1절 F, 계층 2).
 * approvals의 소유자. S2에서는 handleJoin·handleLeave·승인 분기가 그대로라 맵도 내보낸다(S4에서 감춤).
 * 승인 유예(끊김 → APPROVAL_GRACE_MS 뒤 만료)는 handleLeave가 setApproval로 건다.
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
    if (!isAuthenticated(client) && !(insecureDevMode && client.authState === 'authenticated')) return false
    if (insecureDevMode) return true
    if (client.isHost) return true
    if (hasApproval(roomId, client.userId)) return true
    const meta = await getRoomMetaOrNull(roomId)
    return !!meta && meta.freeJoin
  }

  return { approvals, hasApproval, setApproval, clearApproval, canStartPerforming }
}

export type Performers = ReturnType<typeof createPerformers>
