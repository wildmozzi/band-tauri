import { useNavigate } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { isSupabaseConfigured } from '../../lib/supabaseClient'
import { forgetRoomDetail, useDeleteRoomMutation, useSyncParticipantsMutation } from '../../features/rooms/queries'

/** 방장 전용: 참여자 수 동기화, 합주실 삭제 */
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

  // 참여자 수 동기화 (방장만 가능)
  const handleSyncParticipants = async () => {
    if (!isHost || !roomId || !isSupabaseConfigured()) return

    // 현재 WebSocket으로 연결된 실제 참여자 수 (peers + 나)
    const actualCount = peerCount + 1

    try {
      await syncParticipants.mutateAsync({ roomId, count: actualCount })
      console.log(`[SYNC] Participant count synced to ${actualCount}`)
      alert(`참여자 수가 ${actualCount}명으로 동기화되었습니다.`)
    } catch (err) {
      console.error('[SYNC] Failed to sync participants:', err)
      alert('참여자 수 동기화에 실패했습니다.')
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
