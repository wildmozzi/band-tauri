import { useCallback, useState } from 'react'
import { supabase } from '../../lib/supabaseClient'
import type { MyRoom } from './roomModel'

/** 내가 만든 합주실 목록 조회·삭제·로컬 갱신. */
export function useMyRooms(userId: string | undefined) {
  const [myRooms, setMyRooms] = useState<MyRoom[]>([])
  const [myRoomsLoading, setMyRoomsLoading] = useState(false)

  const fetchMyRooms = useCallback(async () => {
    if (!supabase || !userId) return

    setMyRoomsLoading(true)
    try {
      const { data, error } = await supabase
        .from('rooms')
        .select('*')
        .eq('host_id', userId)
        .order('created_at', { ascending: false })

      if (error) throw error
      setMyRooms(data as MyRoom[])
    } catch (err) {
      console.error('Failed to fetch my rooms:', err)
    } finally {
      setMyRoomsLoading(false)
    }
  }, [userId])

  const deleteMyRoom = async (roomId: string) => {
    if (!supabase || !userId) return

    const confirmed = window.confirm('정말로 이 합주실을 삭제하시겠습니까?')
    if (!confirmed) return

    try {
      const { error } = await supabase
        .from('rooms')
        .delete()
        .eq('id', roomId)
        .eq('host_id', userId)

      if (error) throw error

      // 목록에서 제거
      setMyRooms(prev => prev.filter(r => r.id !== roomId))
    } catch (err) {
      console.error('Failed to delete room:', err)
      alert('삭제에 실패했습니다.')
    }
  }

  /** 저장 성공 후 목록의 해당 방을 갱신 */
  const patchMyRoom = useCallback((roomId: string, patch: Partial<MyRoom>) => {
    setMyRooms(prev => prev.map(r => (r.id === roomId ? { ...r, ...patch } : r)))
  }, [])

  return { myRooms, myRoomsLoading, fetchMyRooms, deleteMyRoom, patchMyRoom }
}
