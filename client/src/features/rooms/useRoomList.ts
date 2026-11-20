import { useCallback, useEffect, useMemo, useState } from 'react'
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

  // 목록을 한 번도 못 받았는데 조회가 실패했다(오프라인·DB 장애): "방이 없습니다"가 아니라 오류로 보여 준다
  const loadError = roomsQuery.isError && !roomsQuery.data
  const { refetch } = roomsQuery
  const retry = useCallback(() => { void refetch() }, [refetch])

  return { rooms, loading, loadError, retry, roomFilter, setRoomFilter, filteredRooms }
}
