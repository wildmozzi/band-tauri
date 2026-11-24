import { useEffect, useState } from 'react'
import { engineStats, type EngineStats, type PeerStats } from '../../../native/engineApi'

export const PEER_STATS_POLL_MS = 1000

const NO_PEERS: PeerStats[] = []

/**
 * 합주 중(엔진 사용 중)일 때만 엔진 통계를 1초마다 읽는다(지터 버퍼 상태·스레드 정책 결과 표시용).
 * 꺼져 있거나 첫 응답 전이면 null.
 */
export function useLiveEngineStats(enabled: boolean): EngineStats | null {
  const [stats, setStats] = useState<EngineStats | null>(null)
  useEffect(() => {
    if (!enabled) {
      setStats(null)
      return
    }
    let cancelled = false
    // 앞 요청이 아직이면(느린 응답) 이번 주기는 건너뛰고 마지막 표시를 그대로 둔다(#214 리뷰 L4)
    let inFlight = false
    const poll = async () => {
      if (inFlight) return
      inFlight = true
      try {
        const s = await engineStats()
        if (!cancelled) setStats(s)
      } catch {
        // 엔진이 막 멈춘 경우 등: 다음 주기에 다시
      } finally {
        inFlight = false
      }
    }
    void poll()
    const timer = setInterval(() => void poll(), PEER_STATS_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [enabled])
  return stats
}

/** 합주 중(엔진 사용 중)일 때만 피어 통계를 1초마다 읽는다(지터 버퍼 상태 표시용). */
export function useLivePeerStats(enabled: boolean): PeerStats[] {
  return useLiveEngineStats(enabled)?.peers ?? NO_PEERS
}
