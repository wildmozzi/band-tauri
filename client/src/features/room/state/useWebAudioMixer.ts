/**
 * 수신 스트림(관람자: 방송 믹스) 재생용 Web Audio 믹서.
 * stream → (숨긴 muted <audio>로 스트림 소비) → source → analyser → gain → panner → destination
 * 페이지를 벗어나도(미니 플레이어) 같은 그래프가 계속 재생한다.
 * 수신 스트림(viewer)·믹서 설정(mixer)은 스토어에서 읽고, 설정과 레벨 미터(levels)는 스토어에 쓴다.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { RoomSessionStore, RoomSnapshot } from './roomSessionStore'
import { useRoomSelector } from './useRoomSelector'

export interface MixSettings {
  volume: number // 0 ~ 1
  pan: number // -1 ~ 1
  muted: boolean
}

export const DEFAULT_MIX: MixSettings = { volume: 1, pan: 0, muted: false }

interface Nodes {
  stream: MediaStream
  audio: HTMLAudioElement
  source: MediaStreamAudioSourceNode
  analyser: AnalyserNode
  gain: GainNode
  panner: StereoPannerNode
}

const LEVEL_INTERVAL_MS = 66

const selectRemoteAudioMap = (s: RoomSnapshot) => s.viewer.remoteAudioMap
const selectMixer = (s: RoomSnapshot) => s.mixer

export function useWebAudioMixer(store: RoomSessionStore) {
  const remoteAudioMap = useRoomSelector(store, selectRemoteAudioMap)
  const { mixSettingsMap, masterVolume, masterMuted, masterPan } = useRoomSelector(store, selectMixer)
  const ctxRef = useRef<AudioContext | null>(null)
  const nodesRef = useRef<Map<string, Nodes>>(new Map())
  const recordingDestRef = useRef<MediaStreamAudioDestinationNode | null>(null)

  const { setMixSettingsMap, setMasterVolumeState, setMasterMuted, setMasterPanState } = useMemo(() => ({
    setMixSettingsMap: store.fieldSetter('mixer', 'mixSettingsMap'),
    setMasterVolumeState: store.fieldSetter('mixer', 'masterVolume'),
    setMasterMuted: store.fieldSetter('mixer', 'masterMuted'),
    setMasterPanState: store.fieldSetter('mixer', 'masterPan'),
  }), [store])
  const [nodeCount, setNodeCount] = useState(0)

  const settingsRef = useRef({ mixSettingsMap, masterVolume, masterMuted, masterPan })
  settingsRef.current = { mixSettingsMap, masterVolume, masterMuted, masterPan }

  const getContext = useCallback((): AudioContext => {
    if (!ctxRef.current || ctxRef.current.state === 'closed') {
      ctxRef.current = new AudioContext({ latencyHint: 'interactive', sampleRate: 48000 })
    }
    return ctxRef.current
  }, [])

  const applyNode = useCallback((peerId: string, n: Nodes) => {
    const { mixSettingsMap: map, masterVolume: mv, masterMuted: mm, masterPan: mp } = settingsRef.current
    const s = map[peerId] ?? DEFAULT_MIX
    n.gain.gain.value = mm || s.muted ? 0 : s.volume * mv
    n.panner.pan.value = Math.max(-1, Math.min(1, s.pan + mp))
  }, [])

  const disconnect = useCallback((peerId: string) => {
    const n = nodesRef.current.get(peerId)
    if (!n) return
    nodesRef.current.delete(peerId)
    try {
      n.source.disconnect()
      n.analyser.disconnect()
      n.gain.disconnect()
      n.panner.disconnect()
    } catch {
      // 이미 끊김
    }
    n.audio.srcObject = null
    n.audio.remove()
  }, [])

  const connect = useCallback((peerId: string, stream: MediaStream) => {
    const existing = nodesRef.current.get(peerId)
    if (existing?.stream === stream) return
    if (existing) disconnect(peerId)
    try {
      const ctx = getContext()
      if (ctx.state === 'suspended') ctx.resume().catch(() => {})
      // Chrome은 원격 WebRTC 스트림을 미디어 요소가 소비하지 않으면 Web Audio에 데이터가 오지 않는다
      const audio = document.createElement('audio')
      audio.muted = true
      audio.autoplay = true
      audio.setAttribute('playsinline', 'true')
      audio.srcObject = stream
      audio.play().catch(() => {})

      const source = ctx.createMediaStreamSource(stream)
      const analyser = ctx.createAnalyser()
      analyser.fftSize = 256
      analyser.smoothingTimeConstant = 0.8
      const gain = ctx.createGain()
      const panner = ctx.createStereoPanner()
      source.connect(analyser)
      analyser.connect(gain)
      gain.connect(panner)
      panner.connect(ctx.destination)
      if (recordingDestRef.current) panner.connect(recordingDestRef.current)

      const nodes: Nodes = { stream, audio, source, analyser, gain, panner }
      nodesRef.current.set(peerId, nodes)
      applyNode(peerId, nodes)
    } catch (err) {
      console.error('[Mixer] Web Audio 연결 실패:', err)
    }
  }, [applyNode, disconnect, getContext])

  // 스트림 목록과 노드 동기화
  useEffect(() => {
    for (const [peerId, stream] of Object.entries(remoteAudioMap)) connect(peerId, stream)
    for (const peerId of [...nodesRef.current.keys()]) {
      if (!remoteAudioMap[peerId]) disconnect(peerId)
    }
    setNodeCount(nodesRef.current.size)
  }, [remoteAudioMap, connect, disconnect])

  // 믹서 설정 적용
  useEffect(() => {
    nodesRef.current.forEach((n, peerId) => applyNode(peerId, n))
  }, [mixSettingsMap, masterVolume, masterMuted, masterPan, applyNode])

  // 레벨 측정
  useEffect(() => {
    if (nodeCount === 0) {
      store.patch('levels', { audioLevels: {}, masterLevel: 0 })
      return
    }
    const timer = setInterval(() => {
      const levels: Record<string, number> = {}
      let max = 0
      nodesRef.current.forEach((n, peerId) => {
        const data = new Uint8Array(n.analyser.frequencyBinCount)
        n.analyser.getByteFrequencyData(data)
        const avg = data.reduce((a, b) => a + b, 0) / data.length
        const level = Math.min(100, Math.round((avg / 256) * 150))
        levels[peerId] = level
        max = Math.max(max, level)
      })
      store.patch('levels', { audioLevels: levels, masterLevel: max })
    }, LEVEL_INTERVAL_MS)
    return () => clearInterval(timer)
  }, [nodeCount, store])

  // 언마운트 정리
  useEffect(() => {
    const nodes = nodesRef.current
    return () => {
      for (const peerId of [...nodes.keys()]) {
        const n = nodes.get(peerId)
        n?.audio.remove()
      }
      nodes.clear()
      ctxRef.current?.close().catch(() => {})
      ctxRef.current = null
    }
  }, [])

  const updateMix = useCallback((peerId: string, patch: Partial<MixSettings>) => {
    setMixSettingsMap((prev) => ({ ...prev, [peerId]: { ...DEFAULT_MIX, ...prev[peerId], ...patch } }))
  }, [setMixSettingsMap])

  const setMixVolume = useCallback((peerId: string, volume: number) => {
    updateMix(peerId, { volume: Math.max(0, Math.min(1, volume)) })
  }, [updateMix])

  const setMixPan = useCallback((peerId: string, pan: number) => {
    updateMix(peerId, { pan: Math.max(-1, Math.min(1, pan)) })
  }, [updateMix])

  const setMixMuted = useCallback((peerId: string, muted: boolean) => {
    updateMix(peerId, { muted })
  }, [updateMix])

  const setMasterVolume = useCallback((v: number) => setMasterVolumeState(Math.max(0, Math.min(1, v))), [setMasterVolumeState])
  const setMasterPan = useCallback((p: number) => setMasterPanState(Math.max(-1, Math.min(1, p))), [setMasterPanState])
  const toggleMasterMute = useCallback(() => setMasterMuted((m) => !m), [setMasterMuted])

  const resume = useCallback(() => {
    const ctx = ctxRef.current
    if (ctx?.state === 'suspended') ctx.resume().catch(() => {})
  }, [])

  /** 현재 재생 중인 모든 채널을 녹음용 스트림으로 (채널이 없으면 null) */
  const getRecordingStream = useCallback((): MediaStream | null => {
    if (nodesRef.current.size === 0) return null
    const ctx = getContext()
    if (!recordingDestRef.current) recordingDestRef.current = ctx.createMediaStreamDestination()
    const dest = recordingDestRef.current
    nodesRef.current.forEach((n) => {
      try {
        n.panner.disconnect(dest)
      } catch {
        // 연결 안 돼 있었음
      }
      n.panner.connect(dest)
    })
    return dest.stream
  }, [getContext])

  /** 방 퇴장 시 */
  const reset = useCallback(() => {
    for (const peerId of [...nodesRef.current.keys()]) disconnect(peerId)
    recordingDestRef.current = null
    setMixSettingsMap({})
    setNodeCount(0)
  }, [disconnect, setMixSettingsMap])

  return {
    setMixVolume,
    setMixPan,
    setMixMuted,
    setMasterVolume,
    toggleMasterMute,
    setMasterPan,
    resume,
    getRecordingStream,
    reset,
  }
}
