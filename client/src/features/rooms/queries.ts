/**
 * 합주실 목록·내 방·방 상세 서버 상태 (TanStack Query). 화면/페이지 훅은 Supabase를 직접 부르지 않는다.
 * 방 생성·비밀번호는 서버 API(services/roomApi.ts) 경유. 방 상세 조회는 roomApi.fetchRoomById(get_room RPC).
 *
 * 쿼리 키
 *   ['rooms', 'list']            공개 합주실 목록 (DB 행 전체, 화면에서 publicRooms/toRoom 변환)
 *   ['rooms', 'mine', userId]    내가 만든 방
 *   ['rooms', 'detail', roomId]  방 상세 (RoomDetail, 기본값을 채운 DetailRoom). 비공개 방도 id로 조회된다
 *
 * 무효화 규칙
 *   rooms 테이블 실시간 변경 → 목록·내 방·상세 모두 무효화(구독은 useRoomsQuery가 켜져 있는 동안만).
 *     비활성 쿼리(닫힌 모달, 떠난 상세)는 stale 표시만 하므로 요청은 늘지 않는다
 *   내 방 모달 열기 → 열 때마다 서버에서 다시 읽는다(첫 열기 포함)
 *   내 방 삭제   → 내 방 캐시에서 제거 + 목록 무효화
 *   내 방 수정   → 내 방 캐시에 패치 적용 + 목록 무효화
 *   방 상세 설정 저장(RoomDetail) → 위 '내 방 수정' + 비밀번호(서버 API)까지 성공한 뒤 patchRoomDetailCache로 상세 갱신
 *   참여자 수 동기화 → 서버가 기록한 current_participants를 다시 읽어 상세 캐시 갱신 + 목록 무효화
 *                     (참여자 수는 서버(WebSocket 접속 수)만 쓴다. 클라이언트 UPDATE는 DB가 무시한다 — 011)
 *   방 상세에서 삭제 → 위 '내 방 삭제' + 화면을 떠난 뒤 forgetRoomDetail로 상세 캐시 제거
 *                     (상세가 마운트된 채로 지우면 지운 방을 다시 조회한다)
 *   상세 조회 실패는 재시도하지 않는다(없는 방이면 바로 목록으로 — 기존 동작). 마운트마다 다시 확인(refetchOnMount: always)
 *   사용자 전환(features/auth) 때 상세·공개 목록은 사용자와 무관해 초기화하지 않는다
 */
import { useEffect } from 'react'
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import type { TablesUpdate } from '../../lib/database.types'
import { isSupabaseConfigured, supabase } from '../../lib/supabaseClient'
import { fetchRoomById } from '../../services/roomApi'
import type { DbRoom, MyRoom } from '../../pages/rooms/roomModel'
import { normalizeDetailRoom, type DetailRoom } from '../../pages/room/roomDetailModel'

export const roomKeys = {
  all: ['rooms'] as const,
  list: () => [...roomKeys.all, 'list'] as const,
  mine: (userId: string | null | undefined) => [...roomKeys.all, 'mine', userId ?? null] as const,
  detail: (roomId: string | null | undefined) => [...roomKeys.all, 'detail', roomId ?? null] as const,
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

  /** 방 상세 (비공개 방 포함). 없으면 'room not found' 오류 */
  async getRoomDetail(roomId: string): Promise<DetailRoom> {
    const data = await fetchRoomById(roomId)
    if (!data) throw new RoomNotFoundError(roomId)
    return normalizeDetailRoom(data)
  },

  /** 서버가 기록한 참여자 수 (방장 화면의 '참여자 수 동기화'). 없는 방이면 RoomNotFoundError */
  async getParticipantCount(roomId: string): Promise<number> {
    const room = await roomsApi.getRoomDetail(roomId)
    return room.current_participants ?? 0
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

// ---------- 방 상세 (RoomDetail) ----------

/** 그 id의 방이 없음(삭제됨). 일시적 조회 실패와 구분해 화면이 목록으로 이동한다 */
export class RoomNotFoundError extends Error {
  constructor(roomId: string) {
    super('room not found')
    this.name = 'RoomNotFoundError'
    this.roomId = roomId
  }
  roomId: string
}

/**
 * 방 상세. 실패는 재시도하지 않는다(없는 방 → 화면이 목록으로 이동).
 * 마운트할 때마다 다시 확인한다(미니 플레이어에서 돌아온 사이 삭제·수정된 방 반영 — 기존 동작). 캐시는 그동안 바로 보여준다
 */
export function useRoomDetailQuery(roomId: string | undefined) {
  return useQuery({
    queryKey: roomKeys.detail(roomId),
    queryFn: () => roomsApi.getRoomDetail(roomId!),
    enabled: Boolean(roomId) && isSupabaseConfigured(),
    retry: false,
    refetchOnMount: 'always',
  })
}

export function patchRoomDetailCache(qc: QueryClient, roomId: string, patch: Partial<DetailRoom>) {
  qc.setQueryData<DetailRoom>(roomKeys.detail(roomId), prev => (prev ? { ...prev, ...patch } : prev))
}

/** 삭제한 방의 상세 캐시 제거. 상세 화면을 떠난 뒤(navigate 뒤)에 부른다 */
export function forgetRoomDetail(qc: QueryClient, roomId: string) {
  qc.removeQueries({ queryKey: roomKeys.detail(roomId) })
}

export function useSyncParticipantsMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ roomId }: { roomId: string }) => roomsApi.getParticipantCount(roomId),
    onSuccess: (count, { roomId }) => {
      patchRoomDetailCache(qc, roomId, { current_participants: count })
      return qc.invalidateQueries({ queryKey: roomKeys.list() })
    },
  })
}
