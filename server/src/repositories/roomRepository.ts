import type { SupabaseClient } from '@supabase/supabase-js'
import { toRepositoryError } from './errors'

export interface RoomMeta {
  hostId: string | null
  freeJoin: boolean
}

/** rooms 테이블 접근 (시그널링 서버용) */
export interface RoomRepository {
  /** 방이 없으면 null. DB 오류는 RepositoryError로 throw */
  findMeta(roomId: string): Promise<RoomMeta | null>
  updateParticipantCount(roomId: string, count: number): Promise<void>
  /** 서버 시작 시 모든 방의 참가자 수를 0으로 */
  resetAllParticipantCounts(): Promise<void>
}

export function createRoomRepository(supabase: SupabaseClient): RoomRepository {
  return {
    async findMeta(roomId) {
      const { data, error } = await supabase
        .from('rooms')
        .select('host_id, free_join')
        .eq('id', roomId)
        .maybeSingle()
      if (error) throw toRepositoryError(error, 'rooms.findMeta')
      if (!data) return null
      return { hostId: data.host_id ?? null, freeJoin: data.free_join !== false }
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
  }
}
