import { useCallback, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { RoomNotFoundError, useRoomDetailQuery } from '../../rooms'
import { useProfileNicknameQuery } from '../../auth'
import type { DetailRoom } from '../roomDetailModel'

export interface RoomDetailData {
  room: DetailRoom | null
  loading: boolean
  /** 조회 중이면 null, 없거나 실패면 '알 수 없음' */
  hostNickname: string | null
  /** 방 정보를 한 번도 받지 못했는데 일시 오류로 실패했다(오프라인·DB 장애) — 오류 + 다시 시도를 보인다 */
  loadError: boolean
  retry: () => void
}

/** 방 상세 + 방장 닉네임. 방을 못 찾으면 목록으로 이동한다 */
export function useRoomDetailData(roomId: string | undefined): RoomDetailData {
  const navigate = useNavigate()
  const roomQuery = useRoomDetailQuery(roomId)
  const room = roomQuery.data ?? null
  // 방이 없어졌으면(삭제) 캐시가 있어도 목록으로 이동한다. 일시적 오류(오프라인·DB 장애)면 이동하지 않는다:
  // 보던 방이 있으면 그대로 두고, 처음 여는 중이면 오류 + 다시 시도를 보인다(초대 링크로 들어온 사람이 말없이 목록으로 가지 않게)
  const notFound = roomQuery.isError && roomQuery.error instanceof RoomNotFoundError
  const loadError = roomQuery.isError && !notFound && !roomQuery.data
  const { refetch } = roomQuery
  const retry = useCallback(() => { void refetch() }, [refetch])

  useEffect(() => {
    if (!notFound) return
    navigate('/rooms')
  }, [notFound, navigate])

  useEffect(() => {
    if (roomQuery.isError && !notFound) console.error('Failed to fetch room:', roomQuery.error)
  }, [roomQuery.isError, roomQuery.error, notFound])

  // 실패하면 바로 '알 수 없음' (재시도 없음 — 기존 동작)
  const hostQuery = useProfileNicknameQuery(room?.host_id, true, false)
  const hostNickname = hostQuery.isPending ? null : (hostQuery.data || '알 수 없음')

  return { room, loading: roomQuery.isLoading, hostNickname, loadError, retry }
}
