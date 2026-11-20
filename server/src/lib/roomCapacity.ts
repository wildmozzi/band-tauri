/**
 * 방 정원(관람 포함, rooms.max_participants) 범위. 한 곳에서 정한다 (P3-10g, PR #23 L2).
 * - 방 생성 스키마(POST /api/rooms), WS 정원 상한(wsService maxRoomParticipants 기본값),
 *   DB CHECK(docs/migrations/013), 클라이언트 선택지(client/src/pages/rooms/roomModel.ts)가 모두 이 범위다.
 */
export const ROOM_CAPACITY_MIN = 2
export const ROOM_CAPACITY_MAX = 20

/**
 * DB 값 → 서버가 쓸 정원. 013 전 기존 행 규칙(마이그레이션 013과 같음):
 * - 2~20 → 그대로
 * - 1 → 2 (P3-10g 전에는 정원 1로 강제돼 방장 혼자만 들어갔다. 20으로 넓히지 않고 가장 가까운 유효값 2로 본다)
 * - 0·음수·21 이상·정수 아님·없음 → undefined → 서버 상한(20)만 적용(기존과 같음)
 */
export function parseRoomCapacity(value: unknown): number | undefined {
  if (typeof value !== 'number' || !Number.isInteger(value)) return undefined
  if (value >= ROOM_CAPACITY_MIN && value <= ROOM_CAPACITY_MAX) return value
  if (value >= 1 && value < ROOM_CAPACITY_MIN) return ROOM_CAPACITY_MIN
  return undefined
}
