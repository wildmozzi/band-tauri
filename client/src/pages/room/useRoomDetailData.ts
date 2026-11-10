import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { useRoomDetailQuery } from '../../features/rooms/queries'
import { useProfileNicknameQuery } from '../../features/profile/queries'
import type { DetailRoom } from './roomDetailModel'

export interface RoomDetailData {
  room: DetailRoom | null
  loading: boolean
  /** 조회 중이면 null, 없거나 실패면 '알 수 없음' */
  hostNickname: string | null
}

/** 방 상세 + 방장 닉네임. 방을 못 찾으면(조회 실패) 목록으로 이동한다 */
export function useRoomDetailData(roomId: string | undefined): RoomDetailData {
  const navigate = useNavigate()
  const roomQuery = useRoomDetailQuery(roomId)
  const room = roomQuery.data ?? null
  const failed = roomQuery.isError && !roomQuery.data

  useEffect(() => {
    if (!failed) return
    console.error('Failed to fetch room:', roomQuery.error)
    navigate('/rooms')
  }, [failed, roomQuery.error, navigate])

  const hostQuery = useProfileNicknameQuery(room?.host_id)
  const hostNickname = hostQuery.isPending ? null : (hostQuery.data || '알 수 없음')

  return { room, loading: roomQuery.isLoading, hostNickname }
}
