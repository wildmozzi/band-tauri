import type { SupabaseClient } from '@supabase/supabase-js'
import { toRepositoryError, StoreUnavailableError } from './errors'

export interface RoomMeta {
  hostId: string | null
  freeJoin: boolean
  /** 비공개 방(목록에 안 보임, 링크/방 코드로만 입장). 마이그레이션 008 전이면 false */
  isPrivate?: boolean
  /** rooms.has_password 표시값. true인데 passwordHash가 없으면 생성·변경 중 → 입장 보류(fail-closed) */
  hasPassword?: boolean
  /** room_secrets의 비밀번호 해시 (없으면 null). 서버 메모리에서만 쓰고 클라이언트에 보내지 않는다 */
  passwordHash?: string | null
  /** rooms.max_participants(관람 포함 정원). 없거나 잘못된 값이면 undefined → 서버 기본 상한만 적용 */
  maxParticipants?: number
}

export interface NewRoom {
  hostId: string
  title: string
  description: string | null
  genre: string | null
  maxParticipants: number
  tags: string[]
  freeJoin: boolean
  isPrivate: boolean
  instrumentSlots: { instrument: string; count: number }[]
}

/** 클라이언트에 돌려주는 방 행 (비밀값 없음) */
export type RoomRow = Record<string, unknown> & { id: string; host_id: string }

/** rooms / room_secrets 테이블 접근 */
export interface RoomRepository {
  /** 방이 없으면 null. DB 오류는 RepositoryError로 throw */
  findMeta(roomId: string): Promise<RoomMeta | null>
  updateParticipantCount(roomId: string, count: number): Promise<void>
  /** 서버 시작 시 모든 방의 참가자 수를 0으로 */
  resetAllParticipantCounts(): Promise<void>
  /**
   * 방 생성. hasPassword=true면 has_password=true로 만들어 해시가 저장될 때까지 입장을 막는다(L1).
   * 마이그레이션 008 전이면 공개·비밀번호 없는 방만 만들 수 있고, 나머지는 StoreUnavailableError(M3).
   */
  create(room: NewRoom, opts?: { hasPassword?: boolean }): Promise<RoomRow>
  /** 방 삭제 (생성 직후 비밀번호 저장 실패 시 되돌리기용) */
  remove(roomId: string): Promise<void>
  /** 비밀번호 해시 저장/삭제(null) + rooms.has_password 갱신 — 한 트랜잭션(SQL 함수 set_room_password, L2) */
  setPasswordHash(roomId: string, hash: string | null): Promise<void>
}

/** 컬럼이 없음(마이그레이션 008 미실행) */
const UNDEFINED_COLUMN = new Set(['42703', 'PGRST204'])

export function createRoomRepository(supabase: SupabaseClient): RoomRepository {
  async function findPasswordHash(roomId: string): Promise<string | null> {
    const { data, error } = await supabase
      .from('room_secrets')
      .select('password_hash')
      .eq('room_id', roomId)
      .maybeSingle()
    if (error) {
      const e = toRepositoryError(error, 'room_secrets.find')
      if (e instanceof StoreUnavailableError) return null // 마이그레이션 008 전: 비밀번호 방 없음
      throw e
    }
    return typeof data?.password_hash === 'string' ? data.password_hash : null
  }

  return {
    async findMeta(roomId) {
      let res = await supabase
        .from('rooms')
        .select('host_id, free_join, is_private, has_password, max_participants')
        .eq('id', roomId)
        .maybeSingle()
      if (res.error && res.error.code && UNDEFINED_COLUMN.has(res.error.code)) {
        res = await supabase.from('rooms').select('host_id, free_join, max_participants').eq('id', roomId).maybeSingle()
      }
      const { data, error } = res
      if (error) throw toRepositoryError(error, 'rooms.findMeta')
      if (!data) return null
      const row = data as {
        host_id?: string | null; free_join?: boolean | null; is_private?: boolean | null; has_password?: boolean | null
        max_participants?: number | null
      }
      const max = row.max_participants
      return {
        hostId: row.host_id ?? null,
        freeJoin: row.free_join !== false,
        isPrivate: row.is_private === true,
        hasPassword: row.has_password === true,
        passwordHash: await findPasswordHash(roomId),
        maxParticipants: typeof max === 'number' && Number.isInteger(max) && max > 0 ? max : undefined,
      }
    },

    async updateParticipantCount(roomId, count) {
      const { error } = await supabase
        .from('rooms')
        .update({ current_participants: count })
        .eq('id', roomId)
      if (error) throw toRepositoryError(error, 'rooms.updateParticipantCount')
    },

    async resetAllParticipantCounts() {
      const { error } = await supabase
        .from('rooms')
        .update({ current_participants: 0 })
        .gt('current_participants', 0)
      if (error) throw toRepositoryError(error, 'rooms.resetAllParticipantCounts')
    },

    async create(room, opts = {}) {
      const hasPassword = opts.hasPassword === true
      const base = {
        title: room.title,
        description: room.description,
        host_id: room.hostId,
        max_participants: room.maxParticipants,
        genre: room.genre,
        tags: room.tags,
        status: 'open',
        free_join: room.freeJoin,
        instrument_slots: room.instrumentSlots,
      }
      const insert = (row: Record<string, unknown>) => supabase.from('rooms').insert(row).select().single()

      let { data, error } = await insert({ ...base, is_private: room.isPrivate, has_password: hasPassword })
      if (error?.code && UNDEFINED_COLUMN.has(error.code)) {
        // 마이그레이션 008 전: 공개·비밀번호 없는 방은 예전 컬럼만으로 만든다 (M3)
        if (room.isPrivate || hasPassword) {
          throw new StoreUnavailableError(`rooms.create: ${error.message}`, error.code)
        }
        ;({ data, error } = await insert(base))
      }
      if (error) throw toRepositoryError(error, 'rooms.create')
      return data as RoomRow
    },

    async remove(roomId) {
      const { error } = await supabase.from('rooms').delete().eq('id', roomId)
      if (error) throw toRepositoryError(error, 'rooms.remove')
    },

    async setPasswordHash(roomId, hash) {
      const { error } = await supabase.rpc('set_room_password', { p_room_id: roomId, p_hash: hash })
      if (error) throw toRepositoryError(error, 'set_room_password')
    },
  }
}
