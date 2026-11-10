import { useNavigate } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { isSupabaseConfigured } from '../../lib/supabaseClient'
import { forgetRoomDetail, useDeleteRoomMutation, useSyncParticipantsMutation } from '../../features/rooms/queries'

/** 방장 전용: 참여자 수 새로고침(서버 기준 값 다시 읽기), 합주실 삭제 */
export function useRoomHostActions(opts: {
  roomId: string | undefined
  userId: string | undefined
  isHost: boolean
  peerCount: number
  leaveRoom: () => void
}) {
  const { roomId, userId, isHost, peerCount, leaveRoom } = opts
  const navigate = useNavigate()
  const qc = useQueryClient()
  const syncParticipants = useSyncParticipantsMutation()
  const deleteRoom = useDeleteRoomMutation()

  // 참여자 수 새로고침 (방장만 가능). 참여자 수는 서버가 WebSocket 접속 수로 기록한다 — 그 값을 다시 읽는다
  const handleSyncParticipants = async () => {
    if (!isHost || !roomId || !isSupabaseConfigured()) return

    try {
      const count = await syncParticipants.mutateAsync({ roomId })
      console.log(`[SYNC] Participant count from server: ${count} (connected peers + me: ${peerCount + 1})`)
      alert(`서버 기준 참여자 수: ${count}명`)
    } catch (err) {
      console.error('[SYNC] Failed to sync participants:', err)
      alert('참여자 수를 불러오지 못했습니다.')
    }
  }

  // 방 삭제 (방장만 가능)
  const handleDeleteRoom = async () => {
    if (!isHost || !roomId || !isSupabaseConfigured()) return

    const confirmed = window.confirm('정말로 이 합주실을 삭제하시겠습니까?\n삭제하면 되돌릴 수 없습니다.')
    if (!confirmed) return

    try {
      // 방장 본인만 삭제 가능 (host_id 조건)
      await deleteRoom.mutateAsync({ roomId, userId: userId! })

      leaveRoom()
      navigate('/rooms')
      // 화면을 떠난 뒤 상세 캐시 제거 (마운트 중에 지우면 지운 방을 다시 조회한다)
      forgetRoomDetail(qc, roomId)
    } catch (err) {
      console.error('Failed to delete room:', err)
      alert('합주실 삭제에 실패했습니다.')
    }
  }

  return { handleSyncParticipants, handleDeleteRoom }
}
