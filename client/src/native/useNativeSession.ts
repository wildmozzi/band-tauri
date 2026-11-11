/**
 * PerformSession(연주 세션 컨트롤러)을 React에 연결하는 얇은 훅.
 * 로직은 performSession.ts에 있다(단위 테스트 대상).
 */
import { useEffect, useRef, useState } from 'react'
import {
  engineAddPeer,
  engineRemovePeer,
  engineSetRelay,
  engineSetInputMuted,
  engineSetMetronome,
  engineSetPeerGain,
  engineStart,
  engineStats,
  engineStop,
  isNativeApp,
} from './engineApi'
import { PerformSession, type EngineBackend, type PerformState, type SessionEnv } from './performSession'

export const STATS_POLL_MS = 500

export const tauriEngineBackend: EngineBackend = {
  start: engineStart,
  stop: engineStop,
  addPeer: engineAddPeer,
  setRelay: engineSetRelay,
  removePeer: engineRemovePeer,
  setPeerGain: engineSetPeerGain,
  setInputMuted: engineSetInputMuted,
  setMetronome: engineSetMetronome,
  stats: engineStats,
}

/** env는 ref로 최신 값을 읽으므로 한 번만 만든 세션을 계속 쓴다 */
export function useNativeSession(env: SessionEnv): { session: PerformSession; state: PerformState } {
  const envRef = useRef(env)
  envRef.current = env
  const sessionRef = useRef<PerformSession | null>(null)
  if (!sessionRef.current) {
    sessionRef.current = new PerformSession(tauriEngineBackend, {
      send: (p) => envRef.current.send(p),
      getSelfId: () => envRef.current.getSelfId(),
      getRoomId: () => envRef.current.getRoomId(),
      getOtherPerformers: () => envRef.current.getOtherPerformers(),
    })
  }
  const session = sessionRef.current
  const [state, setState] = useState<PerformState>(session.getState())

  useEffect(() => session.subscribe(setState), [session])

  // engine_stats 500ms 폴링 (연주 중에만)
  const running = state.phase === 'running'
  useEffect(() => {
    if (!running || !isNativeApp()) return
    void session.pollStats()
    const timer = setInterval(() => void session.pollStats(), STATS_POLL_MS)
    return () => clearInterval(timer)
  }, [running, session])

  // 언마운트 시 정지
  useEffect(() => () => session.dispose(), [session])

  return { session, state }
}
