import { useState } from 'react'

/** 방장: 연주 참여 요청 거절 사유 입력 상태 (모달을 닫았다 열어도 유지된다) */
export function useRejectPicker(rejectRequest: (oderId: string, reason?: string) => void) {
  // 거절 사유 입력 중인 요청
  const [rejectingId, setRejectingId] = useState<string | null>(null)
  const [rejectReason, setRejectReason] = useState('')

  const startReject = (oderId: string) => {
    setRejectingId(oderId)
    setRejectReason('')
  }

  const cancelReject = () => {
    setRejectingId(null)
    setRejectReason('')
  }

  // 요청 거절 (사유 선택)
  const confirmReject = (targetId: string, reason: string) => {
    rejectRequest(targetId, reason)
    setRejectingId(null)
    setRejectReason('')
  }

  return { rejectingId, rejectReason, setRejectReason, startReject, cancelReject, confirmReject }
}

export type RejectPicker = ReturnType<typeof useRejectPicker>
