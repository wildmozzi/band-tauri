import type { RoomRepository } from '../repositories/roomRepository'
import type { WsDeps } from './types'

/**
 * rooms 테이블 접근 (repository). 방 조회 오류는 로그 후 다시 던진다: "방 없음(null)"으로 삼키면
 * 메트로놈 재전송이 DB 일시 장애를 방장 이양으로 오인한다(PR #53 H1). 참가자 수 갱신 오류는 무시 — 기존 동작
 */
export function roomDeps(rooms: RoomRepository | null): Pick<WsDeps, 'fetchRoomMeta' | 'updateParticipants'> {
  return {
    fetchRoomMeta: async (roomId) => {
      if (!rooms) return null
      try {
        return await rooms.findMeta(roomId)
      } catch (err) {
        console.error('[DB] Failed to look up room:', err instanceof Error ? err.message : err)
        throw err
      }
    },
    updateParticipants: async (roomId, count) => {
      if (!rooms) return
      try {
        await rooms.updateParticipantCount(roomId, count)
      } catch (err) {
        console.error('[DB] Failed to update participants:', err instanceof Error ? err.message : err)
      }
    },
  }
}
