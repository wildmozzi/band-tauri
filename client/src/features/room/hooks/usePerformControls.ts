import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '../../auth'
import { useRoom } from '../state/RoomContext'

/** 승인 없이 바로 연주를 시작하는가 (자유참여 방이거나 방장) */
export function startsWithoutApproval(freeJoin: boolean | undefined, isHost: boolean): boolean {
  return Boolean(freeJoin) || isHost
}

/**
 * 연주자/관람자 전환 흐름 (데스크톱 앱).
 * 악기 선택 → 자유참여·방장이면 엔진 시작, 아니면 방장에게 요청. 엔진 시작이 성공해야 연주자가 된다.
 */
export function usePerformControls(roomId: string | undefined, freeJoin: boolean | undefined, isHost: boolean) {
  const { user } = useAuth()
  const navigate = useNavigate()
  const {
    canPerform,
    startPerforming,
    stopPerforming,
    myRequestStatus,
    myRequestInstrument,
    requestPerform,
    cancelRequest,
  } = useRoom()
  const [showInstrumentSelect, setShowInstrumentSelect] = useState(false)
  // 연주 시작 처리 중
  const [performBusy, setPerformBusy] = useState(false)

  // 연주자로 전환 요청 (악기 선택 모달 열기) — 데스크톱 앱 전용
  const handleBecomePerformer = () => {
    if (!user) {
      navigate('/auth', { state: { from: `/rooms/${roomId}` } })
      return
    }
    if (!canPerform) return

    // 악기 선택 모달 열기 (자유참여든 승인필요든 일단 악기 선택)
    setShowInstrumentSelect(true)
  }

  // 엔진 시작 → 성공해야 악기 설정/연주자 전환 (실패하면 관람자로 남음, 에러는 rtcError로 표시)
  const beginPerforming = async (instrumentId: string) => {
    if (performBusy) return
    setPerformBusy(true)
    try {
      await startPerforming(instrumentId)
    } finally {
      setPerformBusy(false)
    }
  }

  // 악기 선택 후 연주 시작 또는 요청
  const handleSelectInstrument = async (instrumentId: string) => {
    setShowInstrumentSelect(false)

    if (startsWithoutApproval(freeJoin, isHost)) {
      // 자유참여 또는 방장: 바로 연주 시작
      await beginPerforming(instrumentId)
    } else {
      // 승인 필요: 방장에게 요청 전송
      requestPerform(instrumentId)
    }
  }

  // 승인되면 연주 시작
  const handleStartAfterApproval = async () => {
    if (myRequestInstrument) {
      await beginPerforming(myRequestInstrument)
    }
  }

  // 관람자로 전환
  const handleBecomeViewer = () => {
    void stopPerforming()
    // 요청 중이었다면 취소
    if (myRequestStatus === 'pending') {
      cancelRequest()
    }
  }

  return {
    showInstrumentSelect,
    closeInstrumentSelect: () => setShowInstrumentSelect(false),
    performBusy,
    handleBecomePerformer,
    handleSelectInstrument,
    handleStartAfterApproval,
    handleBecomeViewer,
  }
}
