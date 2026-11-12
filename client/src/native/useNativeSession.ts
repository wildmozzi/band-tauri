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
import { reapplyLoopbackOffsetInBackground } from './loopbackOffsetStore'
import { PerformSession,type EngineBackend, type SessionEnv } from './performSession'

export const STATS_POLL_MS = 500

export const tauriEngineBackend: EngineBackend = {
  start: async (config, peerId) => {
    const descriptor = await engineStart(config, peerId)
    // 앱 재시작 뒤에도 같은 장치 구성이면 메트로놈 출력 지연 보정값을 다시 넣는다(P2-3c)
    reapplyLoopbackOffsetInBackground(config)
    return descriptor
  },
  stop: engineStop,
  addPeer: engineAddPeer,
  setRelay: engineSetRelay,
  removePeer: engineRemovePeer,
  setPeerGain: engineSetPeerGain,
  setInputMuted: engineSetInputMuted,
  setMetronome: engineSetMetronome,
  stats: engineStats,
}

/**
 * env는 ref로 최신 값을 읽으므로 한 번만 만든 세션을 계속 쓴다.
 * 세션 상태는 돌려주지 않는다 — 화면·Provider는 스토어 미러(RoomSessionStore.perform) 하나에서 읽는다.
 * 이 훅은 폴링 on/off에 필요한 running 여부만 React 상태로 들고 있어서 통계 틱(500ms)에는 호출한 컴포넌트를 다시 그리지 않는다.
 */
export function useNativeSession(env: SessionEnv): { session: PerformSession } {
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
  const [running, setRunning] = useState(() => session.getState().phase === 'running')

  useEffect(() => session.subscribe((s) => setRunning(s.phase === 'running')), [session])

  // engine_stats 500ms 폴링 (연주 중에만)
  useEffect(() => {
    if (!running || !isNativeApp()) return
    void session.pollStats()
    const timer = setInterval(() => void session.pollStats(), STATS_POLL_MS)
    return () => clearInterval(timer)
  }, [running, session])

  // 언마운트 시 정지
  useEffect(() => () => session.dispose(), [session])

  return { session }
}
