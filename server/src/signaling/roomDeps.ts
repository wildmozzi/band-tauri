import type { RoomRepository } from '../repositories/roomRepository'
import type { WsDeps } from './types'

/**
 * rooms 테이블 접근 (repository). 방 조회 오류는 로그 후 다시 던진다: "방 없음(null)"으로 삼키면
 * 메트로놈 재전송이 DB 일시 장애를 방장 이양으로 오인한다(PR #53 H1). 참가자 수 갱신 오류는 무시 — 기존 동작
 *
 * writeParticipantCounts=false(주 서버가 아님 — lib/maintenanceMode.ts)면 rooms.current_participants를 쓰지 않는다.
 * 로컬 개발 서버도 운영 DB를 쓰므로, 쓰면 운영 방의 인원을 이 서버가 센 값으로 덮어쓰고 Ctrl+C 뒤 숫자가 남는다(PR #243 리뷰 M1).
 * 대가: 로컬 서버로 테스트하는 동안 방 목록 인원은 0(또는 운영 서버가 쓴 값)으로 보인다. 기본값 true는 기존 동작
 */
export function roomDeps(
  rooms: RoomRepository | null,
  { writeParticipantCounts = true }: { writeParticipantCounts?: boolean } = {},
): Pick<WsDeps, 'fetchRoomMeta' | 'updateParticipants'> {
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
      if (!rooms || !writeParticipantCounts) return
      try {
        await rooms.updateParticipantCount(roomId, count)
      } catch (err) {
        console.error('[DB] Failed to update participants:', err instanceof Error ? err.message : err)
      }
    },
  }
}
