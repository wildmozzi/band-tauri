import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { RoomNotFoundError, useRoomDetailQuery } from '../../rooms/queries'
import { useProfileNicknameQuery } from '../../auth/profileQueries'
import type { DetailRoom } from '../roomDetailModel'

export interface RoomDetailData {
  room: DetailRoom | null
  loading: boolean
  /** 조회 중이면 null, 없거나 실패면 '알 수 없음' */
  hostNickname: string | null
}

/** 방 상세 + 방장 닉네임. 방을 못 찾으면 목록으로 이동한다 */
export function useRoomDetailData(roomId: string | undefined): RoomDetailData {
  const navigate = useNavigate()
  const roomQuery = useRoomDetailQuery(roomId)
  const room = roomQuery.data ?? null
  // 방이 없어졌으면(삭제) 캐시가 있어도 이동, 일시적 오류면 보던 화면 유지
  const failed = roomQuery.isError && (!roomQuery.data || roomQuery.error instanceof RoomNotFoundError)

  useEffect(() => {
    if (!failed) return
    console.error('Failed to fetch room:', roomQuery.error)
    navigate('/rooms')
  }, [failed, roomQuery.error, navigate])

  // 실패하면 바로 '알 수 없음' (재시도 없음 — 기존 동작)
  const hostQuery = useProfileNicknameQuery(room?.host_id, true, false)
  const hostNickname = hostQuery.isPending ? null : (hostQuery.data || '알 수 없음')

  return { room, loading: roomQuery.isLoading, hostNickname }
}
