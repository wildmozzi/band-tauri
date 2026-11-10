import { supabaseAdmin } from '../lib/supabase'
import { hashRoomPassword, validateRoomPassword } from '../lib/roomPassword'
import { createRoomRepository, type NewRoom, type RoomRepository, type RoomRow } from '../repositories/roomRepository'
import { StoreUnavailableError } from '../repositories/errors'
import { ServiceError } from './errors'

/**
 * 방 생성 / 비밀번호 설정 (P2-6).
 * - 비밀번호는 서버에서 scrypt로 해시해 room_secrets에만 저장한다(평문 저장·로그 금지). rooms에는 has_password만.
 * - 방장(host_id)은 토큰으로 확정한 userId. 클라이언트가 보낸 host_id는 무시한다.
 */
export interface CreateRoomInput extends Omit<NewRoom, 'hostId'> {
  password?: string | null
}

export interface RoomService {
  readonly available: boolean
  create(userId: string, input: CreateRoomInput): Promise<RoomRow>
  /** 방장만. password가 null/빈 문자열이면 비밀번호 해제 */
  setPassword(userId: string, roomId: string, password: string | null): Promise<{ hasPassword: boolean }>
}

export function createRoomService(rooms: RoomRepository | null): RoomService {
  const repo = (): RoomRepository => {
    if (!rooms) throw new StoreUnavailableError('rooms: backend not configured')
    return rooms
  }

  const checkPassword = (password: string) => {
    const problem = validateRoomPassword(password)
    if (problem) throw new ServiceError(400, { success: false, error: problem })
  }

  return {
    available: !!rooms,

    async create(userId, input) {
      const { password, ...room } = input
      const pw = password ? password : null
      if (pw !== null) checkPassword(pw)
      // 해시를 먼저 만들어 두면 DB 쓰기 사이 간격이 짧아진다
      const hash = pw !== null ? await hashRoomPassword(pw) : null
      const created = await repo().create({ ...room, hostId: userId })
      if (hash) {
        try {
          await repo().setPasswordHash(created.id, hash)
        } catch (err) {
          // 비밀번호 없는 방이 열린 채 남지 않도록 되돌린다
          await repo().remove(created.id).catch((e) => console.error('[ROOMS] rollback failed:', e instanceof Error ? e.message : e))
          throw err
        }
        return { ...created, has_password: true }
      }
      return created
    },

    async setPassword(userId, roomId, password) {
      const meta = await repo().findMeta(roomId)
      if (!meta) throw new ServiceError(404, { success: false, error: '방을 찾을 수 없습니다.' })
      if (meta.hostId !== userId) throw new ServiceError(403, { success: false, error: '방장만 비밀번호를 바꿀 수 있습니다.' })
      const pw = password ? password : null
      if (pw !== null) checkPassword(pw)
      await repo().setPasswordHash(roomId, pw !== null ? await hashRoomPassword(pw) : null)
      return { hasPassword: pw !== null }
    },
  }
}

let defaultService: RoomService | null = null
export function getDefaultRoomService(): RoomService {
  if (!defaultService) defaultService = createRoomService(supabaseAdmin ? createRoomRepository(supabaseAdmin) : null)
  return defaultService
}
