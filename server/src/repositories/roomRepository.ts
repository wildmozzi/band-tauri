import type { SupabaseClient } from '@supabase/supabase-js'
import { toRepositoryError, StoreUnavailableError } from './errors'

export interface RoomMeta {
  hostId: string | null
  freeJoin: boolean
  /** 비공개 방(목록에 안 보임, 링크/방 코드로만 입장). 마이그레이션 008 전이면 false */
  isPrivate?: boolean
  /** room_secrets의 비밀번호 해시 (없으면 null). 서버 메모리에서만 쓰고 클라이언트에 보내지 않는다 */
  passwordHash?: string | null
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
  /** 방 생성 (has_password=false). 생성된 행 반환 */
  create(room: NewRoom): Promise<RoomRow>
  /** 방 삭제 (생성 직후 비밀번호 저장 실패 시 되돌리기용) */
  remove(roomId: string): Promise<void>
  /** 비밀번호 해시 저장/삭제(null) + rooms.has_password 갱신 */
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
        .select('host_id, free_join, is_private')
        .eq('id', roomId)
        .maybeSingle()
      if (res.error && res.error.code && UNDEFINED_COLUMN.has(res.error.code)) {
        res = await supabase.from('rooms').select('host_id, free_join').eq('id', roomId).maybeSingle()
      }
      const { data, error } = res
      if (error) throw toRepositoryError(error, 'rooms.findMeta')
      if (!data) return null
      const row = data as { host_id?: string | null; free_join?: boolean | null; is_private?: boolean | null }
      return {
        hostId: row.host_id ?? null,
        freeJoin: row.free_join !== false,
        isPrivate: row.is_private === true,
        passwordHash: await findPasswordHash(roomId),
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

    async create(room) {
      const { data, error } = await supabase
        .from('rooms')
        .insert({
          title: room.title,
          description: room.description,
          host_id: room.hostId,
          max_participants: room.maxParticipants,
          genre: room.genre,
          tags: room.tags,
          status: 'open',
          free_join: room.freeJoin,
          is_private: room.isPrivate,
          instrument_slots: room.instrumentSlots,
        })
        .select()
        .single()
      if (error) {
        if (error.code && UNDEFINED_COLUMN.has(error.code)) {
          throw new StoreUnavailableError(`rooms.create: ${error.message}`, error.code)
        }
        throw toRepositoryError(error, 'rooms.create')
      }
      return data as RoomRow
    },

    async remove(roomId) {
      const { error } = await supabase.from('rooms').delete().eq('id', roomId)
      if (error) throw toRepositoryError(error, 'rooms.remove')
    },

    async setPasswordHash(roomId, hash) {
      if (hash) {
        const { error } = await supabase
          .from('room_secrets')
          .upsert({ room_id: roomId, password_hash: hash, updated_at: new Date().toISOString() }, { onConflict: 'room_id' })
        if (error) throw toRepositoryError(error, 'room_secrets.upsert')
      } else {
        const { error } = await supabase.from('room_secrets').delete().eq('room_id', roomId)
        if (error) throw toRepositoryError(error, 'room_secrets.delete')
      }
      const { error } = await supabase.from('rooms').update({ has_password: !!hash }).eq('id', roomId)
      if (error) throw toRepositoryError(error, 'rooms.setHasPassword')
    },
  }
}
