/**
 * 합주실 목록·내 방 서버 상태 (TanStack Query). 화면/페이지 훅은 Supabase를 직접 부르지 않는다.
 * 방 생성·비밀번호는 서버 API(services/roomApi.ts) 경유, 방 상세(RoomDetail)는 이 계층 밖이다.
 *
 * 쿼리 키
 *   ['rooms', 'list']          공개 합주실 목록 (DB 행 전체, 화면에서 publicRooms/toRoom 변환)
 *   ['rooms', 'mine', userId]  내가 만든 방
 *
 * 무효화 규칙
 *   rooms 테이블 실시간 변경 → 목록·내 방 모두 무효화(구독은 useRoomsQuery가 켜져 있는 동안만).
 *     내 방이 비활성(모달 닫힘)이면 stale 표시만 하므로 요청은 늘지 않는다
 *   내 방 모달 열기 → 열 때마다 서버에서 다시 읽는다(첫 열기 포함)
 *   내 방 삭제   → 내 방 캐시에서 제거 + 목록 무효화
 *   내 방 수정   → 내 방 캐시에 패치 적용 + 목록 무효화
 */
import { useEffect } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { TablesUpdate } from '../../lib/database.types'
import { isSupabaseConfigured, supabase } from '../../lib/supabaseClient'
import type { DbRoom, MyRoom } from '../../pages/rooms/roomModel'

export const roomKeys = {
  all: ['rooms'] as const,
  list: () => [...roomKeys.all, 'list'] as const,
  mine: (userId: string | null | undefined) => [...roomKeys.all, 'mine', userId ?? null] as const,
}

function requireClient() {
  if (!supabase) throw new Error('Supabase not configured')
  return supabase
}

// ---------- 데이터 접근 ----------

export const roomsApi = {
  /** 보이는 방 전체(최신순). 남의 비공개 방은 RLS가 숨긴다 */
  async listRooms(): Promise<DbRoom[]> {
    const { data, error } = await requireClient()
      .from('rooms')
      .select('*')
      .order('created_at', { ascending: false })
    if (error) throw error
    return data as DbRoom[]
  },

  async listMyRooms(userId: string): Promise<MyRoom[]> {
    const { data, error } = await requireClient()
      .from('rooms')
      .select('*')
      .eq('host_id', userId)
      .order('created_at', { ascending: false })
    if (error) throw error
    return data as MyRoom[]
  },

  /** 방장 본인 방만 삭제 */
  async deleteRoom(roomId: string, userId: string): Promise<void> {
    const { error } = await requireClient()
      .from('rooms')
      .delete()
      .eq('id', roomId)
      .eq('host_id', userId)
    if (error) throw error
  },

  /** 방장 본인 방만 수정 */
  async updateRoom(roomId: string, userId: string, update: TablesUpdate<'rooms'>): Promise<void> {
    const { error } = await requireClient()
      .from('rooms')
      .update(update)
      .eq('id', roomId)
      .eq('host_id', userId)
    if (error) throw error
  },

  /** rooms 테이블 변경 구독. 반환값을 호출하면 해제 */
  subscribeRoomChanges(onChange: () => void): () => void {
    if (!supabase) return () => {}
    const sb = supabase
    const channel = sb
      .channel('rooms-changes')
      .on('postgres_changes', { event: '*', schema: 'public', table: 'rooms' }, onChange)
      .subscribe()
    return () => {
      sb.removeChannel(channel)
    }
  },
}

// ---------- 훅 ----------

/** 보이는 방 목록 + 실시간 변경 시 재조회 */
export function useRoomsQuery() {
  const qc = useQueryClient()
  const enabled = isSupabaseConfigured()

  useEffect(() => {
    if (!enabled) return
    return roomsApi.subscribeRoomChanges(() => {
      // RoomDetail(이 계층 밖)의 수정·삭제도 실시간으로 들어오므로 내 방까지 무효화한다
      void qc.invalidateQueries({ queryKey: roomKeys.all })
    })
  }, [enabled, qc])

  return useQuery({
    queryKey: roomKeys.list(),
    queryFn: roomsApi.listRooms,
    enabled,
  })
}

export function useMyRoomsQuery(userId: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: roomKeys.mine(userId),
    queryFn: () => roomsApi.listMyRooms(userId!),
    enabled: enabled && Boolean(userId) && isSupabaseConfigured(),
  })
}

export function useDeleteRoomMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ roomId, userId }: { roomId: string; userId: string }) => roomsApi.deleteRoom(roomId, userId),
    onSuccess: (_void, { roomId, userId }) => {
      qc.setQueryData<MyRoom[]>(roomKeys.mine(userId), prev => prev?.filter(r => r.id !== roomId))
      return qc.invalidateQueries({ queryKey: roomKeys.list() })
    },
  })
}

export function useUpdateRoomMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ roomId, userId, update }: { roomId: string; userId: string; update: TablesUpdate<'rooms'> & Partial<MyRoom> }) =>
      roomsApi.updateRoom(roomId, userId, update),
    onSuccess: (_void, { roomId, userId, update }) => {
      patchMyRoomCache(qc, userId, roomId, update)
      return qc.invalidateQueries({ queryKey: roomKeys.list() })
    },
  })
}

export function patchMyRoomCache(
  qc: ReturnType<typeof useQueryClient>,
  userId: string | undefined,
  roomId: string,
  patch: Partial<MyRoom>,
) {
  qc.setQueryData<MyRoom[]>(roomKeys.mine(userId), prev =>
    prev?.map(r => (r.id === roomId ? { ...r, ...patch } : r)),
  )
}
