import { useEffect, useMemo, useState } from 'react'
import { ROOM_FILTERS } from './roomFilters'
import { useRoomsQuery } from './queries'
import { publicRooms, toRoom, type Room } from './roomModel'

const EMPTY: Room[] = []

/** 공개 합주실 목록(실시간 갱신, 비공개 방 제외) + 장르 필터. */
export function useRoomList() {
  const roomsQuery = useRoomsQuery()
  const [roomFilter, setRoomFilter] = useState<string>('all')

  useEffect(() => {
    if (roomsQuery.error) console.error('Failed to fetch rooms:', roomsQuery.error)
  }, [roomsQuery.error])

  const rooms = useMemo(
    () => (roomsQuery.data ? publicRooms(roomsQuery.data).map(toRoom) : EMPTY),
    [roomsQuery.data],
  )
  // 설정이 없으면(쿼리 비활성) 로딩 표시 없이 빈 목록
  const loading = roomsQuery.isLoading

  const filteredRooms = useMemo(() => {
    const filter = ROOM_FILTERS.find((item) => item.id === roomFilter)
    if (!filter || !filter.match) return rooms
    return rooms.filter((room) => Boolean(filter.match?.(room)))
  }, [rooms, roomFilter])

  return { rooms, loading, roomFilter, setRoomFilter, filteredRooms }
}
