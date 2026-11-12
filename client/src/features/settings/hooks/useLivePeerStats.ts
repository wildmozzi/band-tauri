import { useEffect, useState } from 'react'
import { engineStats, type PeerStats } from '../../../native/engineApi'

export const PEER_STATS_POLL_MS = 1000

/** 합주 중(엔진 사용 중)일 때만 피어 통계를 1초마다 읽는다(지터 버퍼 상태 표시용). */
export function useLivePeerStats(enabled: boolean): PeerStats[] {
  const [peers, setPeers] = useState<PeerStats[]>([])
  useEffect(() => {
    if (!enabled) {
      setPeers([])
      return
    }
    let cancelled = false
    const poll = async () => {
      try {
        const s = await engineStats()
        if (!cancelled) setPeers(s.peers)
      } catch {
        // 엔진이 막 멈춘 경우 등: 다음 주기에 다시
      }
    }
    void poll()
    const timer = setInterval(() => void poll(), PEER_STATS_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [enabled])
  return peers
}
