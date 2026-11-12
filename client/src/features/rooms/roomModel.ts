// 합주실 목록/내 합주실/설정 모달에서 쓰는 타입·상수·순수 함수.
import type { RoomStatus } from './roomFilters'
import { MAX_PERFORMERS } from '../../native/engineApi'

export const GENRES = [
  '록', '재즈', '블루스', '클래식', '팝', '힙합',
  '일렉트로닉', '포크', '메탈', '펑크', '레게', '기타'
]

export const COMMON_TAGS = [
  '초보환영', '경력자', '세션구함', '정기모임',
  '즉흥연주', '커버곡', '자작곡', '녹음가능'
]

export const AVAILABLE_INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

/**
 * 정원(관람 포함) 범위. 서버 server/src/lib/roomCapacity.ts(방 생성 스키마·WS 상한)와
 * DB CHECK(docs/migrations/013)와 같다. 바꾸면 세 곳을 함께 바꾼다.
 */
export const ROOM_CAPACITY_MIN = 2
export const ROOM_CAPACITY_MAX = 20

export const MAX_PARTICIPANT_OPTIONS: readonly number[] = [2, 4, 6, 8, 10, 12, 16, 20]
  .filter(n => n >= ROOM_CAPACITY_MIN && n <= ROOM_CAPACITY_MAX)

/**
 * DB 값 → 표시·편집용 정원. 범위 밖·없음(013 전 기존 행)은 서버가 실제로 쓰는 상한(20)으로 본다
 * (서버 parseRoomCapacity와 같은 규칙) — 목록이 '3/50명'처럼 서버와 다른 정원을 보이지 않게.
 */
export function effectiveRoomCapacity(value: number | null | undefined): number {
  if (typeof value !== 'number' || !Number.isInteger(value)) return ROOM_CAPACITY_MAX
  if (value >= ROOM_CAPACITY_MIN && value <= ROOM_CAPACITY_MAX) return value
  // 1 → 2: 013 전 '정원 1' 방을 20으로 넓히지 않는다(서버 parseRoomCapacity·마이그레이션 013과 같음)
  if (value >= 1 && value < ROOM_CAPACITY_MIN) return ROOM_CAPACITY_MIN
  return ROOM_CAPACITY_MAX
}

/** 편집 선택지: 기본 선택지에 없는 현재 값(예: 3)도 그대로 고를 수 있게 넣는다 */
export function maxParticipantOptionsFor(current: number): readonly number[] {
  return MAX_PARTICIPANT_OPTIONS.includes(current) ? MAX_PARTICIPANT_OPTIONS : [...MAX_PARTICIPANT_OPTIONS, current].sort((a, b) => a - b)
}

/** 방 최대 인원은 관람자를 포함한 정원이다. 동시 연주자는 이와 별개로 최대 MAX_PERFORMERS명 (P3-12 ⑧) */
export const MAX_PARTICIPANTS_LABEL = '최대 인원 (관람 포함)'
export const MAX_PARTICIPANTS_HINT = `연주자는 최대 ${MAX_PERFORMERS}명이고, 나머지 인원은 관람으로 입장합니다. 로그인하지 않은 관람자는 빈자리가 있을 때만 들어옵니다.`

/** 선택지 표시: 연주 가능 인원을 함께 보여 준다 (예: '8명 (연주 6 + 관람 2)') */
export function maxParticipantsOptionLabel(n: number): string {
  const performers = Math.min(n, MAX_PERFORMERS)
  const viewers = n - performers
  return viewers > 0 ? `${n}명 (연주 ${performers} + 관람 ${viewers})` : `${n}명 (모두 연주 가능)`
}

export const MAX_SLOT_COUNT = 10

export interface InstrumentSlot {
  instrument: string
  count: number
}

export interface DbRoom {
  id: string
  title: string
  description: string | null
  host_id: string
  max_participants: number
  current_participants: number
  status: string
  genre: string | null
  tags: string[]
  created_at: string
  free_join?: boolean
  instrument_slots?: InstrumentSlot[]
  /** 마이그레이션 008 (P2-6). 전이면 undefined */
  is_private?: boolean
  has_password?: boolean
}

/** 내가 만든 방(DbRoom과 동일) */
export type MyRoom = DbRoom

export interface Room {
  id: string
  title: string
  vibe: string
  genre: string
  musicians: number
  capacity: number
  status: RoomStatus
  tags: string[]
  instrumentSlots: InstrumentSlot[]
  freeJoin: boolean
  /** 입장 시 비밀번호 필요 */
  hasPassword: boolean
}

/** 기존 방에 악기 구성이 없을 때 설정 모달의 기본값 */
export const DEFAULT_EDIT_SLOTS: InstrumentSlot[] = [
  { instrument: 'vocal', count: 1 },
  { instrument: 'guitar', count: 1 },
  { instrument: 'bass', count: 1 },
  { instrument: 'keyboard', count: 1 },
  { instrument: 'drums', count: 1 },
]

/** DB 행을 목록 카드용 형식으로 변환 */
export function toRoom(room: DbRoom): Room {
  return {
    id: room.id,
    title: room.title,
    vibe: room.description || '함께 음악을 만들어요',
    genre: room.genre || '기타',
    musicians: room.current_participants,
    capacity: effectiveRoomCapacity(room.max_participants),
    status: room.status as RoomStatus,
    tags: room.tags || [],
    instrumentSlots: room.instrument_slots || [],
    freeJoin: room.free_join ?? true,
    hasPassword: room.has_password === true,
  }
}

/**
 * 공개 목록에 보일 방만. 남의 비공개 방은 RLS가 이미 숨기고,
 * 내 비공개 방(RLS상 보임)은 '내 합주실'에서만 보여준다.
 */
export function publicRooms(rooms: DbRoom[]): DbRoom[] {
  return rooms.filter(room => room.is_private !== true)
}

export function formatDate(dateString: string) {
  const date = new Date(dateString)
  return date.toLocaleDateString('ko-KR', { year: 'numeric', month: 'short', day: 'numeric' })
}

export function findInstrument(id: string) {
  return AVAILABLE_INSTRUMENTS.find(i => i.id === id)
}

export function toggleTag(tags: string[], tag: string): string[] {
  return tags.includes(tag) ? tags.filter(t => t !== tag) : [...tags, tag]
}

/** 해당 악기 슬롯 수를 0..MAX_SLOT_COUNT로 잘라 설정 */
export function setSlotCount(slots: InstrumentSlot[], instrument: string, count: number): InstrumentSlot[] {
  return slots.map(slot =>
    slot.instrument === instrument
      ? { ...slot, count: Math.max(0, Math.min(MAX_SLOT_COUNT, count)) }
      : slot
  )
}

/** 아직 슬롯에 없는 악기 목록 */
export function instrumentsNotInSlots(slots: InstrumentSlot[]) {
  return AVAILABLE_INSTRUMENTS.filter(inst => !slots.find(s => s.instrument === inst.id))
}

export function totalSlotCount(slots: InstrumentSlot[]) {
  return slots.reduce((sum, s) => sum + s.count, 0)
}

export interface RoomEditFields {
  title: string
  description: string
  genre: string
  maxParticipants: number
  freeJoin: boolean
  tags: string[]
  instrumentSlots: InstrumentSlot[]
}

/** 설정 모달 입력 → rooms 테이블 update 페이로드(0개 슬롯 제외, 공백 정리) */
export function buildRoomUpdate(fields: RoomEditFields) {
  return {
    title: fields.title.trim(),
    description: fields.description.trim() || null,
    genre: fields.genre,
    max_participants: effectiveRoomCapacity(fields.maxParticipants),
    free_join: fields.freeJoin,
    tags: fields.tags,
    instrument_slots: fields.instrumentSlots.filter(s => s.count > 0),
  }
}

export function roomStatusLabel(status: string) {
  return status === 'open' ? '입장 가능' : status === 'recording' ? '녹음 중' : '잠김'
}

export function myRoomStatusLabel(status: string) {
  return status === 'open' ? '열림' : status === 'recording' ? '녹음중' : '잠김'
}
