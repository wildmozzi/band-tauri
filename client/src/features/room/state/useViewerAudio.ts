/**
 * 관람자 수신 WebRTC (설계 6절).
 *
 * - 관람자는 offer를 보내지 않는다. 방송 담당 연주자 앱이 보낸 offer에 answer만 한다(Glare 없음).
 * - offer는 서버가 붙인 `from`이 현재 연주자일 때만 받는다(아무나 관람자에게 소리를 밀어 넣지 못하게).
 * - 방송 담당은 한 명이므로 다른 연주자의 offer가 오면(인계) 이전 연결을 닫는다.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { ICE_SERVERS, tuneOpusSdp } from '../../../shared/lib/webrtcAudio'
import type { SignalPayload } from './useSignaling'

export type RtcStatus = 'idle' | 'connecting' | 'live' | 'error'

export interface PeerNetworkStats {
  audioLatency: number | null
  jitter: number | null
  packetsLost: number
  packetLossRate: number
  quality: 'excellent' | 'good' | 'fair' | 'poor' | 'unknown'
}

export function getNetworkQuality(audioLatency: number | null, packetLossRate: number): PeerNetworkStats['quality'] {
  if (audioLatency === null) return 'unknown'
  if (audioLatency < 30 && packetLossRate < 1) return 'excellent'
  if (audioLatency < 50 && packetLossRate < 3) return 'good'
  if (audioLatency < 100 && packetLossRate < 5) return 'fair'
  return 'poor'
}

interface Incoming {
  pc: RTCPeerConnection
  pending: RTCIceCandidateInit[]
}

const UNKNOWN_STATS: PeerNetworkStats = { audioLatency: null, jitter: null, packetsLost: 0, packetLossRate: 0, quality: 'unknown' }

export function useViewerAudio(opts: {
  send: (p: SignalPayload) => void
  /** offer를 받아도 되는 발신자인지 (현재 연주자) */
  isAllowedSender: (peerId: string) => boolean
}) {
  const optsRef = useRef(opts)
  optsRef.current = opts
  const pcsRef = useRef<Map<string, Incoming>>(new Map())

  const [remoteAudioMap, setRemoteAudioMap] = useState<Record<string, MediaStream>>({})
  const [status, setStatus] = useState<RtcStatus>('idle')
  const [error, setError] = useState<string | null>(null)
  const [networkStats, setNetworkStats] = useState<Record<string, PeerNetworkStats>>({})

  const refreshStatus = useCallback(() => {
    const states = [...pcsRef.current.values()].map((i) => i.pc.connectionState)
    if (states.length === 0) setStatus('idle')
    else if (states.includes('connected')) setStatus('live')
    else setStatus('connecting')
  }, [])

  const closeFrom = useCallback((peerId: string) => {
    const incoming = pcsRef.current.get(peerId)
    if (incoming) {
      pcsRef.current.delete(peerId)
      incoming.pc.ontrack = null
      incoming.pc.onicecandidate = null
      incoming.pc.onconnectionstatechange = null
      incoming.pc.close()
    }
    setRemoteAudioMap((prev) => {
      if (!(peerId in prev)) return prev
      const next = { ...prev }
      delete next[peerId]
      return next
    })
    setNetworkStats((prev) => {
      if (!(peerId in prev)) return prev
      const next = { ...prev }
      delete next[peerId]
      return next
    })
    refreshStatus()
  }, [refreshStatus])

  const closeAll = useCallback(() => {
    for (const id of [...pcsRef.current.keys()]) closeFrom(id)
    setRemoteAudioMap({})
    setNetworkStats({})
    setStatus('idle')
  }, [closeFrom])

  const hasPeer = useCallback((peerId: string) => pcsRef.current.has(peerId), [])

  const handleOffer = useCallback(async (from: string, offer: RTCSessionDescriptionInit) => {
    if (!optsRef.current.isAllowedSender(from)) {
      console.warn('[Viewer] 연주자가 아닌 클라이언트의 offer 무시:', from.slice(0, 8))
      return
    }
    // 방송 담당 인계 또는 재협상: 기존 연결을 모두 닫고 새로 받는다
    for (const id of [...pcsRef.current.keys()]) closeFrom(id)

    const pc = new RTCPeerConnection({ iceServers: ICE_SERVERS })
    const incoming: Incoming = { pc, pending: [] }
    pcsRef.current.set(from, incoming)
    setStatus('connecting')
    setError(null)

    pc.ontrack = (event) => {
      const stream = event.streams[0] ?? new MediaStream([event.track])
      setRemoteAudioMap((prev) => ({ ...prev, [from]: stream }))
    }
    pc.onicecandidate = (event) => {
      if (event.candidate) optsRef.current.send({ type: 'ice-candidate', to: from, candidate: event.candidate.toJSON() })
    }
    pc.onconnectionstatechange = () => {
      if (pcsRef.current.get(from)?.pc !== pc) return
      if (pc.connectionState === 'failed') {
        setError('방송 연결에 실패했습니다. 잠시 후 다시 연결됩니다.')
        closeFrom(from)
        setStatus('error')
        return
      }
      if (pc.connectionState === 'closed') {
        closeFrom(from)
        return
      }
      refreshStatus()
    }

    try {
      await pc.setRemoteDescription(offer)
      for (const c of incoming.pending) await pc.addIceCandidate(c).catch(() => {})
      incoming.pending = []
      const answer = await pc.createAnswer()
      const tuned = { type: answer.type, sdp: tuneOpusSdp(answer.sdp ?? '', { stereo: true }) }
      await pc.setLocalDescription(tuned)
      optsRef.current.send({ type: 'answer', to: from, answer: tuned })
    } catch (err) {
      console.error('[Viewer] offer 처리 실패:', err)
      setError('방송 연결 협상에 실패했습니다.')
      closeFrom(from)
      setStatus('error')
    }
  }, [closeFrom, refreshStatus])

  const handleCandidate = useCallback(async (from: string, candidate: RTCIceCandidateInit) => {
    const incoming = pcsRef.current.get(from)
    if (!incoming || !candidate?.candidate) return
    if (!incoming.pc.remoteDescription) {
      incoming.pending.push(candidate)
      return
    }
    await incoming.pc.addIceCandidate(candidate).catch(() => {})
  }, [])

  // 방송 수신 품질 측정 (2초마다)
  useEffect(() => {
    const measure = async () => {
      const update: Record<string, PeerNetworkStats> = {}
      for (const [peerId, { pc }] of pcsRef.current.entries()) {
        if (pc.connectionState !== 'connected') {
          update[peerId] = UNKNOWN_STATS
          continue
        }
        try {
          const report = await pc.getStats()
          let rtt: number | null = null
          let jitter: number | null = null
          let jitterBufferMs: number | null = null
          let lost = 0
          let received = 0
          report.forEach((r) => {
            if (r.type === 'candidate-pair' && (r.state === 'succeeded' || r.nominated) && typeof r.currentRoundTripTime === 'number') {
              rtt = r.currentRoundTripTime * 1000
            }
            if (r.type === 'inbound-rtp' && r.kind === 'audio') {
              if (typeof r.jitter === 'number') jitter = Math.round(r.jitter * 1000)
              if (typeof r.packetsLost === 'number') lost = r.packetsLost
              if (typeof r.packetsReceived === 'number') received = r.packetsReceived
              if (typeof r.jitterBufferDelay === 'number' && r.jitterBufferEmittedCount > 0) {
                jitterBufferMs = (r.jitterBufferDelay / r.jitterBufferEmittedCount) * 1000
              }
            }
          })
          const total = lost + received
          const lossRate = total > 0 ? (lost / total) * 100 : 0
          const latency = rtt !== null ? Math.round((rtt as number) / 2 + (jitterBufferMs ?? 0) + 5) : null
          update[peerId] = {
            audioLatency: latency,
            jitter,
            packetsLost: lost,
            packetLossRate: Math.round(lossRate * 10) / 10,
            quality: getNetworkQuality(latency, lossRate),
          }
        } catch {
          update[peerId] = UNKNOWN_STATS
        }
      }
      if (Object.keys(update).length > 0) setNetworkStats((prev) => ({ ...prev, ...update }))
    }
    const timer = setInterval(() => {
      if (pcsRef.current.size > 0) void measure()
    }, 2000)
    return () => clearInterval(timer)
  }, [])

  // 언마운트 정리
  useEffect(() => {
    const pcs = pcsRef.current
    return () => {
      pcs.forEach(({ pc }) => pc.close())
      pcs.clear()
    }
  }, [])

  return { remoteAudioMap, status, error, networkStats, handleOffer, handleCandidate, closeFrom, closeAll, hasPeer }
}
