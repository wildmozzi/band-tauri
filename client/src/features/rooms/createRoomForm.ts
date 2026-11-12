/**
 * 방 만들기 화면(CreateRoom.tsx)의 입력 검사·요청 만들기 — UI와 분리한 순수 함수 (P3-2 ③-4, vitest로 검증).
 * 태그·악기 슬롯 조작은 설정 모달과 같은 roomModel.ts 함수를 쓴다.
 */
import type { CreateRoomInput } from './roomApi'
import { validateRoomPasswordInput } from './roomAccess'
import type { InstrumentSlot } from './roomModel'

export interface CreateRoomFields {
  title: string
  description: string
  genre: string
  maxParticipants: number
  tags: string[]
  freeJoin: boolean
  isPrivate: boolean
  instrumentSlots: InstrumentSlot[]
  usePassword: boolean
  password: string
}

/** 제출 전 검사: 문제 문구, 없으면 null (제목 → 비밀번호 순) */
export function validateCreateRoom(fields: Pick<CreateRoomFields, 'title' | 'usePassword' | 'password'>): string | null {
  if (!fields.title.trim()) return '방 제목을 입력하세요.'
  if (fields.usePassword) return validateRoomPasswordInput(fields.password)
  return null
}

/** 입력 → 방 생성 요청(공백 정리, 빈 설명은 null, 0개 슬롯 제외, 비밀번호는 사용할 때만) */
export function buildCreateRoomInput(fields: CreateRoomFields): CreateRoomInput {
  return {
    title: fields.title.trim(),
    description: fields.description.trim() || null,
    genre: fields.genre,
    maxParticipants: fields.maxParticipants,
    tags: fields.tags,
    freeJoin: fields.freeJoin,
    isPrivate: fields.isPrivate,
    instrumentSlots: fields.instrumentSlots.filter(s => s.count > 0),
    password: fields.usePassword ? fields.password : null,
  }
}

/** 커스텀 태그 추가: 공백 정리 후 비었거나 이미 있으면 null(추가 안 함) */
export function customTagToAdd(tags: string[], raw: string): string | null {
  const tag = raw.trim()
  return tag && !tags.includes(tag) ? tag : null
}
