import { useCallback, useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { isSupabaseConfigured } from '../../lib/supabaseClient'
import { patchMyRoomCache, useDeleteRoomMutation, useMyRoomsQuery } from '../../features/rooms/queries'
import type { MyRoom } from './roomModel'

const EMPTY: MyRoom[] = []

/** 내가 만든 합주실 목록 조회·삭제·로컬 갱신. 목록은 fetchMyRooms를 처음 부를 때부터 조회한다(모달 열기). */
export function useMyRooms(userId: string | undefined) {
  const qc = useQueryClient()
  const [requested, setRequested] = useState(false)
  const myRoomsQuery = useMyRoomsQuery(userId, requested)
  const deleteRoom = useDeleteRoomMutation()
  const { refetch } = myRoomsQuery

  useEffect(() => {
    if (myRoomsQuery.error) console.error('Failed to fetch my rooms:', myRoomsQuery.error)
  }, [myRoomsQuery.error])

  /** 모달을 열 때마다 최신 목록을 다시 읽는다 */
  const fetchMyRooms = useCallback(async () => {
    if (!isSupabaseConfigured() || !userId) return
    if (!requested) {
      setRequested(true) // 쿼리가 켜지면서 조회
      return
    }
    await refetch()
  }, [userId, requested, refetch])

  const deleteMyRoom = async (roomId: string) => {
    if (!isSupabaseConfigured() || !userId) return

    const confirmed = window.confirm('정말로 이 합주실을 삭제하시겠습니까?')
    if (!confirmed) return

    try {
      // 성공하면 캐시에서 제거된다
      await deleteRoom.mutateAsync({ roomId, userId })
    } catch (err) {
      console.error('Failed to delete room:', err)
      alert('삭제에 실패했습니다.')
    }
  }

  /** 저장 성공 후 목록의 해당 방을 갱신 */
  const patchMyRoom = useCallback((roomId: string, patch: Partial<MyRoom>) => {
    patchMyRoomCache(qc, userId, roomId, patch)
  }, [qc, userId])

  return {
    myRooms: myRoomsQuery.data ?? EMPTY,
    myRoomsLoading: myRoomsQuery.isFetching,
    fetchMyRooms,
    deleteMyRoom,
    patchMyRoom,
  }
}
