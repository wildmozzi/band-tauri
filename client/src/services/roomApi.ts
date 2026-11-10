// 방 생성/비밀번호 API (P2-6). 비밀번호는 서버가 해시해서 저장하므로 방 생성은 서버를 거친다.
// 방 조회는 Supabase: 비공개 방도 id를 알면 get_room(id)로 조회한다(목록에는 RLS로 숨김).
import { authHeaders } from '../lib/authHeaders'
import { supabase } from '../lib/supabaseClient'

function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') {
    return `${window.location.protocol}//${window.location.hostname}:8080`
  }
  return envUrl
}

export interface CreateRoomInput {
  title: string
  description: string | null
  genre: string
  maxParticipants: number
  tags: string[]
  freeJoin: boolean
  isPrivate: boolean
  instrumentSlots: { instrument: string; count: number }[]
  /** 비우면 비밀번호 없음 */
  password: string | null
}

export type RoomRecord = Record<string, unknown> & { id: string }

async function call<T>(method: string, path: string, body: unknown): Promise<T> {
  let res: Response
  try {
    res = await fetch(`${getApiUrl()}${path}`, {
      method,
      headers: { 'Content-Type': 'application/json', ...(await authHeaders()) },
      body: JSON.stringify(body),
    })
  } catch {
    throw new Error('서버에 연결할 수 없습니다. 잠시 후 다시 시도해주세요.')
  }
  let data: { success?: boolean; error?: string } & Record<string, unknown> = {}
  try {
    data = await res.json()
  } catch {
    // 본문 없음
  }
  if (!res.ok || data.success === false) {
    throw new Error(typeof data.error === 'string' ? data.error : `요청 실패 (${res.status})`)
  }
  return data as T
}

export async function createRoom(input: CreateRoomInput): Promise<RoomRecord> {
  const { room } = await call<{ room: RoomRecord }>('POST', '/api/rooms', input)
  return room
}

/** 방장만. null이면 비밀번호 해제 */
export async function setRoomPassword(roomId: string, password: string | null): Promise<boolean> {
  const { hasPassword } = await call<{ hasPassword: boolean }>('PUT', `/api/rooms/${encodeURIComponent(roomId)}/password`, { password })
  return hasPassword
}

/** id로 방 하나 조회 (비공개 방 포함). 없으면 null. 마이그레이션 008 전이면 일반 select로 폴백 */
export async function fetchRoomById(roomId: string): Promise<RoomRecord | null> {
  if (!supabase) return null
  const rpc = await supabase.rpc('get_room', { p_room_id: roomId })
  if (!rpc.error) {
    const rows = (rpc.data ?? []) as RoomRecord[]
    return rows[0] ?? null
  }
  const { data, error } = await supabase.from('rooms').select('*').eq('id', roomId).maybeSingle()
  if (error) throw error
  return (data as RoomRecord | null) ?? null
}
