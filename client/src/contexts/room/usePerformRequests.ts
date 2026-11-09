/**
 * 연주 참여 요청 / 악기 변경 요청 (방장 승인 흐름).
 * 승인·거절 판단은 서버가 한다(서버가 방장 여부를 판정). 클라이언트는 서버가 보낸 결과만 반영한다.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import type { SignalPayload } from './useSignaling'

export interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

export interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

export type MyRequestStatus = 'none' | 'pending' | 'approved' | 'rejected'

function isPerformRequest(v: unknown): v is PerformRequest {
  const r = v as PerformRequest
  return !!r && typeof r.oderId === 'string' && typeof r.instrument === 'string'
}

function isInstrumentChangeRequest(v: unknown): v is InstrumentChangeRequest {
  const r = v as InstrumentChangeRequest
  return !!r && typeof r.oderId === 'string' && typeof r.newInstrument === 'string'
}

export function usePerformRequests(opts: {
  send: (p: SignalPayload) => void
  canSend: () => boolean
  /** 내 악기 변경이 승인됨 */
  onMyInstrumentChanged: (instrument: string) => void
  /** 다른 연주자의 악기 변경 */
  onPeerInstrumentChanged: (peerId: string, instrument: string) => void
}) {
  const { send, canSend } = opts
  const callbacksRef = useRef(opts)
  callbacksRef.current = opts
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([])

  const [pendingRequests, setPendingRequests] = useState<PerformRequest[]>([])
  const [myRequestStatus, setMyRequestStatus] = useState<MyRequestStatus>('none')
  const [myRequestInstrument, setMyRequestInstrument] = useState<string | null>(null)
  const [pendingInstrumentChanges, setPendingInstrumentChanges] = useState<InstrumentChangeRequest[]>([])
  const [myInstrumentChangeStatus, setMyInstrumentChangeStatus] = useState<MyRequestStatus>('none')
  const [myNewInstrument, setMyNewInstrument] = useState<string | null>(null)

  const later = useCallback((fn: () => void, ms: number) => {
    timersRef.current.push(setTimeout(fn, ms))
  }, [])
  useEffect(() => {
    const timers = timersRef.current
    return () => timers.forEach(clearTimeout)
  }, [])

  // --- 요청자 ---
  const requestPerform = useCallback((instrument: string) => {
    if (!canSend()) return
    send({ type: 'request-perform', instrument })
    setMyRequestInstrument(instrument)
  }, [send, canSend])

  const cancelRequest = useCallback(() => {
    if (canSend()) send({ type: 'cancel-perform-request' })
    setMyRequestStatus('none')
    setMyRequestInstrument(null)
  }, [send, canSend])

  /** 승인 후 연주를 시작했으면 요청 상태 정리 */
  const resetMyRequest = useCallback(() => {
    setMyRequestStatus('none')
    setMyRequestInstrument(null)
  }, [])

  const requestInstrumentChange = useCallback((newInstrument: string) => {
    if (!canSend()) return
    send({ type: 'request-instrument-change', newInstrument })
    setMyNewInstrument(newInstrument)
  }, [send, canSend])

  const cancelInstrumentChangeRequest = useCallback(() => {
    if (canSend()) send({ type: 'cancel-instrument-change-request' })
    setMyInstrumentChangeStatus('none')
    setMyNewInstrument(null)
  }, [send, canSend])

  // --- 방장 ---
  const approveRequest = useCallback((targetId: string) => {
    if (!canSend()) return
    send({ type: 'approve-perform', targetId })
    setPendingRequests((prev) => prev.filter((r) => r.oderId !== targetId))
  }, [send, canSend])

  const rejectRequest = useCallback((targetId: string, reason?: string) => {
    if (!canSend()) return
    send({ type: 'reject-perform', targetId, reason })
    setPendingRequests((prev) => prev.filter((r) => r.oderId !== targetId))
  }, [send, canSend])

  const approveInstrumentChange = useCallback((targetId: string) => {
    if (!canSend()) return
    send({ type: 'approve-instrument-change', targetId })
    setPendingInstrumentChanges((prev) => prev.filter((r) => r.oderId !== targetId))
  }, [send, canSend])

  const rejectInstrumentChange = useCallback((targetId: string, reason?: string) => {
    if (!canSend()) return
    send({ type: 'reject-instrument-change', targetId, reason })
    setPendingInstrumentChanges((prev) => prev.filter((r) => r.oderId !== targetId))
  }, [send, canSend])

  /** 참여자가 나가면 그 사람의 대기 요청 제거 */
  const dropPeer = useCallback((peerId: string) => {
    setPendingRequests((prev) => prev.filter((r) => r.oderId !== peerId))
    setPendingInstrumentChanges((prev) => prev.filter((r) => r.oderId !== peerId))
  }, [])

  const resetAll = useCallback(() => {
    setPendingRequests([])
    setPendingInstrumentChanges([])
    setMyRequestStatus('none')
    setMyRequestInstrument(null)
    setMyInstrumentChangeStatus('none')
    setMyNewInstrument(null)
  }, [])

  const handleRequestSignal = useCallback((p: SignalPayload): boolean => {
    switch (p.type) {
      case 'perform-request-sent':
        setMyRequestStatus('pending')
        return true
      case 'request-perform-error':
        setMyRequestStatus('none')
        setMyRequestInstrument(null)
        return true
      case 'perform-request-received':
        if (isPerformRequest(p.request)) {
          const req = p.request
          setPendingRequests((prev) => (prev.some((r) => r.oderId === req.oderId) ? prev : [...prev, req]))
        }
        return true
      case 'perform-request-cancelled':
        if (typeof p.oderId === 'string') {
          const id = p.oderId
          setPendingRequests((prev) => prev.filter((r) => r.oderId !== id))
        }
        return true
      case 'perform-request-approved':
        setMyRequestStatus('approved')
        if (typeof p.instrument === 'string') setMyRequestInstrument(p.instrument)
        return true
      case 'perform-request-rejected':
        setMyRequestStatus('rejected')
        return true
      case 'instrument-change-request-sent':
        setMyInstrumentChangeStatus('pending')
        return true
      case 'instrument-change-request-received':
        if (isInstrumentChangeRequest(p.request)) {
          const req = p.request
          setPendingInstrumentChanges((prev) => (prev.some((r) => r.oderId === req.oderId) ? prev : [...prev, req]))
        }
        return true
      case 'instrument-change-request-cancelled':
        if (typeof p.oderId === 'string') {
          const id = p.oderId
          setPendingInstrumentChanges((prev) => prev.filter((r) => r.oderId !== id))
        }
        return true
      case 'instrument-change-approved':
        if (typeof p.newInstrument === 'string') {
          setMyInstrumentChangeStatus('approved')
          callbacksRef.current.onMyInstrumentChanged(p.newInstrument)
          later(() => {
            setMyInstrumentChangeStatus('none')
            setMyNewInstrument(null)
          }, 2000)
        }
        return true
      case 'instrument-change-rejected':
        setMyInstrumentChangeStatus('rejected')
        later(() => {
          setMyInstrumentChangeStatus('none')
          setMyNewInstrument(null)
        }, 3000)
        return true
      case 'participant-instrument-changed':
        if (typeof p.oderId === 'string' && typeof p.instrument === 'string') {
          callbacksRef.current.onPeerInstrumentChanged(p.oderId, p.instrument)
        }
        return true
      default:
        return false
    }
  }, [later])

  return {
    pendingRequests,
    approveRequest,
    rejectRequest,
    myRequestStatus,
    myRequestInstrument,
    requestPerform,
    cancelRequest,
    resetMyRequest,
    pendingInstrumentChanges,
    approveInstrumentChange,
    rejectInstrumentChange,
    myInstrumentChangeStatus,
    myNewInstrument,
    requestInstrumentChange,
    cancelInstrumentChangeRequest,
    dropPeer,
    resetAll,
    handleRequestSignal,
  }
}
