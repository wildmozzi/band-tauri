/**
 * 시그널링 ↔ 네이티브 엔진 연결 훅 (설계 5·6절).
 *
 * 흐름: start(config) → engine_start → (호출자가 start-performing 전송) → announce() → native-descriptor 브로드캐스트.
 * 서버는 연주자(isPerforming || isHost)의 native-descriptor만 중계하므로 announce는 start-performing 뒤에 불러야 한다.
 * 상태 판단은 NativeSignalMachine(순수)이 하고, 이 훅은 그 액션을 실행만 한다.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import {
  engineAddPeer,
  engineErrorMessage,
  engineRemovePeer,
  engineSetPeerGain,
  engineStart,
  engineStats,
  engineStop,
  isNativeApp,
  type EngineConfig,
  type EngineStats,
  type LocalDescriptor,
} from './engineApi'
import { NativeSignalMachine, isValidDescriptor, type SessionAction } from './sessionLogic'

export const STATS_POLL_MS = 500

type SendFn = (payload: Record<string, unknown> & { type: string }) => void

export interface NativeSession {
  /** 엔진이 돌고 있음 (engine_start 성공 ~ stop) */
  running: boolean
  starting: boolean
  error: string | null
  /** 500ms 폴링 결과 (running일 때만) */
  stats: EngineStats | null
  /** engine_start. 실패하면 throw (호출자가 연주자 상태를 롤백) */
  start: (config: EngineConfig, selfId: string) => Promise<LocalDescriptor>
  /** descriptor 브로드캐스트. knownPerformers = 지금 알고 있는 다른 연주자 id */
  announce: (knownPerformers: string[]) => void
  /** native-leave + engine_stop */
  stop: (notify?: boolean) => Promise<void>
  /** native-descriptor / native-leave 처리. 처리했으면 true */
  handleSignal: (payload: Record<string, unknown>) => boolean
  /** 다른 클라이언트가 연주자가 됨 */
  performerAppeared: (peerId: string) => void
  /** 피어 이탈 (participant-left / 연주 중단 / leave) */
  peerGone: (peerId: string) => void
  setPeerGain: (peerId: string, gain: number, muted: boolean) => void
  clearError: () => void
}

export function useNativeSession(send: SendFn): NativeSession {
  const machineRef = useRef(new NativeSignalMachine())
  const descriptorRef = useRef<LocalDescriptor | null>(null)
  const selfIdRef = useRef<string | null>(null)
  const runningRef = useRef(false)
  const sendRef = useRef(send)
  sendRef.current = send
  // 엔진 커맨드 순서 보장 (add → remove가 뒤바뀌지 않도록)
  const queueRef = useRef<Promise<unknown>>(Promise.resolve())

  const [running, setRunning] = useState(false)
  const [starting, setStarting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [stats, setStats] = useState<EngineStats | null>(null)

  const enqueue = useCallback((label: string, fn: () => Promise<unknown>) => {
    queueRef.current = queueRef.current
      .then(fn)
      .catch((err) => {
        console.warn(`[Native] ${label} 실패:`, engineErrorMessage(err))
      })
    return queueRef.current
  }, [])

  const runActions = useCallback((actions: SessionAction[]) => {
    for (const action of actions) {
      switch (action.kind) {
        case 'broadcast':
          sendRef.current({ type: 'native-descriptor', descriptor: action.descriptor })
          break
        case 'sendTo':
          sendRef.current({ type: 'native-descriptor', to: action.peerId, descriptor: action.descriptor })
          break
        case 'addPeer': {
          const { peerId, descriptor } = action
          enqueue(`engine_add_peer(${peerId.slice(0, 8)})`, async () => {
            try {
              await engineAddPeer(peerId, descriptor)
            } catch (err) {
              machineRef.current.addPeerFailed(peerId)
              throw err
            }
          })
          break
        }
        case 'removePeer':
          enqueue(`engine_remove_peer(${action.peerId.slice(0, 8)})`, () => engineRemovePeer(action.peerId))
          break
        case 'rejectPeer':
          console.warn('[Native] 최대 연주자 수 초과로 피어 무시:', action.peerId.slice(0, 8))
          break
      }
    }
  }, [enqueue])

  const start = useCallback(async (config: EngineConfig, selfId: string) => {
    if (!isNativeApp()) throw new Error('연주는 데스크톱 앱에서만 할 수 있습니다.')
    if (runningRef.current) {
      if (descriptorRef.current && selfIdRef.current === selfId) return descriptorRef.current
      throw new Error('엔진이 이미 다른 세션으로 실행 중입니다.')
    }
    setStarting(true)
    setError(null)
    try {
      const descriptor = await engineStart(config, selfId)
      if (!isValidDescriptor(descriptor)) {
        await engineStop().catch(() => {})
        throw new Error('엔진이 올바르지 않은 연결 정보를 반환했습니다.')
      }
      descriptorRef.current = descriptor
      selfIdRef.current = selfId
      runningRef.current = true
      setRunning(true)
      return descriptor
    } catch (err) {
      const msg = engineErrorMessage(err)
      setError(msg)
      throw new Error(msg)
    } finally {
      setStarting(false)
    }
  }, [])

  const announce = useCallback((knownPerformers: string[]) => {
    const descriptor = descriptorRef.current
    const selfId = selfIdRef.current
    if (!runningRef.current || !descriptor || !selfId) return
    runActions(machineRef.current.started(selfId, descriptor, knownPerformers))
  }, [runActions])

  const stop = useCallback(async (notify = true) => {
    if (!runningRef.current) return
    runningRef.current = false
    machineRef.current.stopped()
    descriptorRef.current = null
    selfIdRef.current = null
    setRunning(false)
    setStats(null)
    if (notify) sendRef.current({ type: 'native-leave' })
    await enqueue('engine_stop', () => engineStop())
  }, [enqueue])

  const handleSignal = useCallback((payload: Record<string, unknown>) => {
    const from = payload.from
    if (payload.type === 'native-descriptor') {
      if (typeof from !== 'string') return true
      if (!isValidDescriptor(payload.descriptor)) {
        console.warn('[Native] 잘못된 descriptor 무시:', from.slice(0, 8))
        return true
      }
      runActions(machineRef.current.descriptorReceived(from, payload.descriptor))
      return true
    }
    if (payload.type === 'native-leave') {
      if (typeof from === 'string') runActions(machineRef.current.peerGone(from))
      return true
    }
    return false
  }, [runActions])

  const performerAppeared = useCallback((peerId: string) => {
    runActions(machineRef.current.performerAppeared(peerId))
  }, [runActions])

  const peerGone = useCallback((peerId: string) => {
    runActions(machineRef.current.peerGone(peerId))
  }, [runActions])

  const setPeerGain = useCallback((peerId: string, gain: number, muted: boolean) => {
    if (!runningRef.current) return
    enqueue('engine_set_peer_gain', () => engineSetPeerGain(peerId, gain, muted))
  }, [enqueue])

  // engine_stats 500ms 폴링
  useEffect(() => {
    if (!running) return
    let cancelled = false
    let inFlight = false
    const poll = async () => {
      if (inFlight) return
      inFlight = true
      try {
        const s = await engineStats()
        if (!cancelled) setStats(s)
      } catch {
        // 엔진이 막 정지된 경우 등 — 다음 폴링에서 재시도
      } finally {
        inFlight = false
      }
    }
    void poll()
    const timer = setInterval(poll, STATS_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [running])

  // 언마운트 시 엔진 정지
  useEffect(() => {
    return () => {
      if (runningRef.current) {
        runningRef.current = false
        sendRef.current({ type: 'native-leave' })
        engineStop().catch(() => {})
      }
    }
  }, [])

  const clearError = useCallback(() => setError(null), [])

  return {
    running,
    starting,
    error,
    stats,
    start,
    announce,
    stop,
    handleSignal,
    performerAppeared,
    peerGone,
    setPeerGain,
    clearError,
  }
}
