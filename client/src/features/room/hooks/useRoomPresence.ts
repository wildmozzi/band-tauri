import { useEffect, useRef } from 'react'
import { useAuth } from '../../contexts/useAuth'
import { useRoom } from '../../contexts/RoomContext'
import { recallRoomPassword } from '../../contexts/room/roomAccess'
import type { DetailRoom } from './roomDetailModel'

/**
 * 방 입장/이탈 수명주기.
 * - 시그널링 연결 후 자동 joinRoom (같은 방에 이미 연결돼 있으면 미니 플레이어만 해제)
 * - 페이지를 떠나면: 입장 완료 전이면 퇴장, 입장했으면 미니 플레이어로 전환
 */
export function useRoomPresence(roomId: string | undefined, room: DetailRoom | null) {
  const { user, loading: authLoading } = useAuth()
  const {
    signalStatus,
    joinRoom,
    roomJoined,
    leaveRoom,
    enterMiniPlayerMode,
    clearMiniPlayerMode,
    currentRoomId: contextRoomId,
  } = useRoom()
  const hasJoinedRef = useRef(false)

  // clearMiniPlayerMode를 ref로 저장 (의존성 문제 방지)
  const clearMiniPlayerModeRef = useRef(clearMiniPlayerMode)
  useEffect(() => {
    clearMiniPlayerModeRef.current = clearMiniPlayerMode
  }, [clearMiniPlayerMode])

  // 방 입장 시 자동으로 joinRoom 호출
  useEffect(() => {
    // 인증 로딩이 완료될 때까지 대기 (isHost 정확히 계산하기 위해)
    if (authLoading) return

    // 이미 같은 방에 연결되어 있으면 (미니 플레이어에서 돌아온 경우 등) joinRoom 불필요
    if (contextRoomId === roomId) {
      if (!hasJoinedRef.current) {
        console.log('[ROOM] Already connected to this room, skipping joinRoom')
        hasJoinedRef.current = true
        // 미니 플레이어 모드 해제
        clearMiniPlayerModeRef.current()
      }
      return
    }

    // 다른 방에 연결되어 있으면 먼저 나가기 (미니 플레이어 모드도 해제됨)
    if (contextRoomId && contextRoomId !== roomId) {
      console.log('[ROOM] Connected to different room, leaving first:', contextRoomId.slice(0, 8))
      leaveRoom()
      hasJoinedRef.current = false
    }

    if (room && roomId && signalStatus === 'connected' && !hasJoinedRef.current) {
      hasJoinedRef.current = true
      // isHost를 여기서 다시 계산 (최신 user 상태 반영)
      const hostFlag = user && room.host_id === user.id
      // 비밀번호 방: 이번 세션에서 입력했던 비밀번호로 먼저 시도 (틀리면 서버가 다시 묻는다)
      joinRoom(roomId, hostFlag || false, hostFlag ? null : recallRoomPassword(roomId))
      console.log('[ROOM] Joining room with isHost:', hostFlag, 'user:', user?.id, 'host_id:', room.host_id)
      // 참여자 수 동기화는 room-state 수신 후 peers 변경 시 자동으로 처리됨
    }
  }, [room, roomId, signalStatus, authLoading, user, joinRoom, leaveRoom, contextRoomId])

  // 함수들을 ref로 저장 (의존성 문제 방지)
  const leaveRoomRef = useRef(leaveRoom)
  const enterMiniPlayerModeRef = useRef(enterMiniPlayerMode)
  const roomJoinedRef = useRef(roomJoined)
  roomJoinedRef.current = roomJoined
  useEffect(() => {
    leaveRoomRef.current = leaveRoom
    enterMiniPlayerModeRef.current = enterMiniPlayerMode
  }, [leaveRoom, enterMiniPlayerMode])

  // 페이지 이탈 시 cleanup - 미니 플레이어 모드로 전환 (방에서 완전히 나가지 않음)
  useEffect(() => {
    if (!roomId) return

    // cleanup: 컴포넌트 언마운트 시
    return () => {
      if (hasJoinedRef.current && !roomJoinedRef.current) {
        // 서버가 아직 입장시키지 않음(비밀번호 화면 등): 미니 플레이어 대신 퇴장
        console.log('[CLEANUP] Page leaving before join completed, leaving:', roomId.slice(0, 8))
        hasJoinedRef.current = false
        leaveRoomRef.current()
      } else if (hasJoinedRef.current) {
        // hasJoinedRef는 유지 (미니 플레이어에서 돌아올 때 재사용)
        console.log('[CLEANUP] Page leaving, entering mini player mode:', roomId.slice(0, 8))
        // leaveRoom 대신 미니 플레이어 모드로 전환
        enterMiniPlayerModeRef.current()
      }
    }
  }, [roomId])
}
