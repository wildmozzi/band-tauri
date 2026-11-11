/**
 * 방 정원(관람 포함, rooms.max_participants) 범위. 한 곳에서 정한다 (P3-10g, PR #23 L2).
 * - 방 생성 스키마(POST /api/rooms), WS 정원 상한(wsService maxRoomParticipants 기본값),
 *   DB CHECK(docs/migrations/013), 클라이언트 선택지(client/src/pages/rooms/roomModel.ts)가 모두 이 범위다.
 */
export const ROOM_CAPACITY_MIN = 2
export const ROOM_CAPACITY_MAX = 20

/** DB 값 → 서버가 쓸 정원. 범위 밖(013 전 기존 행)·정수가 아니면 undefined → 서버 상한(20)만 적용 */
export function parseRoomCapacity(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isInteger(value) && value >= ROOM_CAPACITY_MIN && value <= ROOM_CAPACITY_MAX
    ? value
    : undefined
}
