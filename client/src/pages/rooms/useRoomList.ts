import { useEffect, useMemo, useState } from 'react'
import { supabase } from '../../lib/supabaseClient'
import { ROOM_FILTERS } from '../../data/rooms'
import { publicRooms, toRoom, type DbRoom, type Room } from './roomModel'

/** 공개 합주실 목록(실시간 갱신, 비공개 방 제외) + 장르 필터. */
export function useRoomList() {
  const [rooms, setRooms] = useState<Room[]>([])
  const [loading, setLoading] = useState(true)
  const [roomFilter, setRoomFilter] = useState<string>('all')

  useEffect(() => {
    const fetchRooms = async () => {
      if (!supabase) {
        setLoading(false)
        return
      }

      try {
        const { data, error } = await supabase
          .from('rooms')
          .select('*')
          .order('created_at', { ascending: false })

        if (error) throw error
        setRooms(publicRooms(data as DbRoom[]).map(toRoom))
      } catch (err) {
        console.error('Failed to fetch rooms:', err)
      } finally {
        setLoading(false)
      }
    }

    fetchRooms()

    // 실시간 업데이트 구독
    if (!supabase) return

    const sb = supabase // 타입 narrowing을 위한 로컬 참조
    const channel = sb
      .channel('rooms-changes')
      .on('postgres_changes', { event: '*', schema: 'public', table: 'rooms' }, () => {
        fetchRooms()
      })
      .subscribe()

    return () => {
      sb.removeChannel(channel)
    }
  }, [])

  const filteredRooms = useMemo(() => {
    const filter = ROOM_FILTERS.find((item) => item.id === roomFilter)
    if (!filter || !filter.match) return rooms
    return rooms.filter((room) => Boolean(filter.match?.(room)))
  }, [rooms, roomFilter])

  return { rooms, loading, roomFilter, setRoomFilter, filteredRooms }
}
