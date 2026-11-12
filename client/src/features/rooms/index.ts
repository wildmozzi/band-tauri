// features/rooms 공개 API — 방 카탈로그(목록·만들기·조회/수정)와 입장 규칙. 다른 기능·app은 이 파일로만 import한다(docs/frontend-architecture.md).
export { CreateRoom } from './CreateRoom'
export { Rooms } from './Rooms'
export {
  forgetRoomDetail,
  patchRoomDetailCache,
  roomKeys,
  RoomNotFoundError,
  useDeleteRoomMutation,
  useRoomDetailQuery,
  useSyncParticipantsMutation,
  useUpdateRoomMutation,
} from './queries'
export {
  describeJoinError,
  forgetRoomPassword,
  inviteLink,
  joinErrorFromSignal,
  joinErrorStopsSession,
  recallRoomPassword,
  rememberRoomPassword,
  validateRoomPasswordInput,
} from './roomAccess'
export type { JoinError } from './roomAccess'
export { setRoomPassword } from './roomApi'
export {
  AVAILABLE_INSTRUMENTS,
  buildRoomUpdate,
  COMMON_TAGS,
  effectiveRoomCapacity,
  GENRES,
  instrumentsNotInSlots,
  MAX_PARTICIPANTS_HINT,
  MAX_PARTICIPANTS_LABEL,
  maxParticipantOptionsFor,
  maxParticipantsOptionLabel,
  ROOM_CAPACITY_MAX,
  setSlotCount,
  toggleTag,
  totalSlotCount,
} from './roomModel'
export type { DetailRoom, InstrumentSlot } from './roomModel'
