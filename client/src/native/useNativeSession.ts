/**
 * PerformSession(연주 세션 컨트롤러)을 React에 연결하는 얇은 훅.
 * 로직은 performSession.ts에 있다(단위 테스트 대상). 세션 본체는 데스크톱 앱에서만 지연 로드되고(runtimeLoader),
 * 웹은 엔진 없는 세션(idleSession)을 받는다.
 */
import { useEffect, useRef, useState } from 'react'
import { isNativeApp } from './engineApi'
import type { PerformSessionApi } from './idleSession'
import type { SessionEnv } from './performSession'
import { createPerformSession } from './runtimeLoader'

export const STATS_POLL_MS = 500

/**
 * env는 ref로 최신 값을 읽으므로 한 번만 만든 세션을 계속 쓴다.
 * 세션 상태는 돌려주지 않는다 — 화면·Provider는 스토어 미러(RoomSessionStore.perform) 하나에서 읽는다.
 * 이 훅은 폴링 on/off에 필요한 running 여부만 React 상태로 들고 있어서 통계 틱(500ms)에는 호출한 컴포넌트를 다시 그리지 않는다.
 */
export function useNativeSession(env: SessionEnv): { session: PerformSessionApi } {
  const envRef = useRef(env)
  envRef.current = env
  const sessionRef = useRef<PerformSessionApi | null>(null)
  if (!sessionRef.current) {
    sessionRef.current = createPerformSession({
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
