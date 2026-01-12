/**
 * Tauri Native Audio Hook
 *
 * Tauri 네이티브 오디오 엔진을 React에서 쉽게 사용하기 위한 훅
 */

import { useState, useEffect, useCallback, useRef } from 'react'
import {
  isTauri,
  startAudio,
  stopAudio,
  getAudioStats,
  setMonitoring,
  getMonitoring,
  initNetwork,
  shutdownNetwork,
  getNetworkInfo,
  connectToPeer,
  disconnectPeer,
  type AudioStats,
  type AudioSettings,
  type NetworkInfo,
  DEFAULT_AUDIO_SETTINGS,
} from './tauri'

interface UseTauriAudioReturn {
  // 상태
  isNativeMode: boolean
  isRunning: boolean
  isMonitoring: boolean
  stats: AudioStats | null
  networkInfo: NetworkInfo | null
  error: string | null

  // 오디오 액션
  start: (settings?: AudioSettings) => Promise<void>
  stop: () => Promise<void>
  updateSettings: (settings: AudioSettings) => Promise<void>

  // 모니터링 액션
  toggleMonitoring: () => Promise<void>
  setMonitoringEnabled: (enabled: boolean) => Promise<void>

  // P2P 네트워크 액션
  startNetwork: (myId: string) => Promise<void>
  stopNetwork: () => Promise<void>
  connectPeer: (peerId: string, publicAddr: string) => Promise<void>
  disconnectFromPeer: (peerId: string) => Promise<void>
}

/**
 * Tauri 네이티브 오디오 훅
 *
 * @example
 * ```tsx
 * const {
 *   isNativeMode,
 *   isRunning,
 *   isMonitoring,
 *   start,
 *   stop,
 *   toggleMonitoring,
 *   startNetwork,
 *   connectPeer
 * } = useTauriAudio()
 *
 * // Tauri 네이티브 오디오 시작
 * await start()
 *
 * // 모니터링 토글 (본인 소리 듣기)
 * await toggleMonitoring()
 *
 * // P2P 네트워크 시작
 * await startNetwork('my-peer-id')
 *
 * // 피어에 연결
 * await connectPeer('peer-123', '1.2.3.4:5678')
 * ```
 */
export function useTauriAudio(): UseTauriAudioReturn {
  const [isRunning, setIsRunning] = useState(false)
  const [isMonitoring, setIsMonitoring] = useState(false)
  const [stats, setStats] = useState<AudioStats | null>(null)
  const [networkInfo, setNetworkInfo] = useState<NetworkInfo | null>(null)
  const [error, setError] = useState<string | null>(null)

  const isNativeMode = isTauri()
  const statsIntervalRef = useRef<NodeJS.Timeout | null>(null)

  // 오디오 엔진 시작
  const start = useCallback(async (settings?: AudioSettings) => {
    if (!isNativeMode) {
      setError('Not in Tauri environment')
      return
    }

    try {
      setError(null)
      const audioSettings = settings || DEFAULT_AUDIO_SETTINGS
      await startAudio(audioSettings)
      setIsRunning(true)
      console.log('[TAURI] Native audio started with settings:', audioSettings)
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      setError(message)
      console.error('[TAURI] Failed to start audio:', message)
    }
  }, [isNativeMode])

  // 오디오 엔진 중지
  const stop = useCallback(async () => {
    if (!isNativeMode) return

    try {
      await stopAudio()
      setIsRunning(false)
      setStats(null)
      console.log('[TAURI] Native audio stopped')
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      setError(message)
      console.error('[TAURI] Failed to stop audio:', message)
    }
  }, [isNativeMode])

  // 설정 변경 (오디오 재시작 필요)
  const updateSettings = useCallback(
    async (settings: AudioSettings) => {
      if (!isNativeMode) return

      try {
        // 새 API에서는 설정 변경 시 오디오 재시작 필요
        await stopAudio()
        await startAudio(settings)
        console.log('[TAURI] Audio settings updated:', settings)
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        setError(message)
        console.error('[TAURI] Failed to update settings:', message)
      }
    },
    [isNativeMode]
  )

  // 모니터링 토글
  const toggleMonitoring = useCallback(async () => {
    if (!isNativeMode) return

    try {
      const newState = !isMonitoring
      await setMonitoring(newState)
      setIsMonitoring(newState)
      console.log('[TAURI] Monitoring:', newState ? 'enabled' : 'disabled')
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      setError(message)
      console.error('[TAURI] Failed to toggle monitoring:', message)
    }
  }, [isNativeMode, isMonitoring])

  // 모니터링 설정
  const setMonitoringEnabled = useCallback(
    async (enabled: boolean) => {
      if (!isNativeMode) return

      try {
        await setMonitoring(enabled)
        setIsMonitoring(enabled)
        console.log('[TAURI] Monitoring:', enabled ? 'enabled' : 'disabled')
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        setError(message)
        console.error('[TAURI] Failed to set monitoring:', message)
      }
    },
    [isNativeMode]
  )

  // P2P 네트워크 시작
  const startNetwork = useCallback(
    async (myId: string) => {
      if (!isNativeMode) return

      try {
        setError(null)
        const info = await initNetwork(myId)
        setNetworkInfo(info)
        console.log('[TAURI] P2P network started:', info)
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        setError(message)
        console.error('[TAURI] Failed to start network:', message)
      }
    },
    [isNativeMode]
  )

  // P2P 네트워크 중지
  const stopNetwork = useCallback(async () => {
    if (!isNativeMode) return

    try {
      await shutdownNetwork()
      setNetworkInfo(null)
      console.log('[TAURI] P2P network stopped')
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      setError(message)
      console.error('[TAURI] Failed to stop network:', message)
    }
  }, [isNativeMode])

  // 피어 연결
  const connectPeer = useCallback(
    async (peerId: string, publicAddr: string) => {
      if (!isNativeMode) return

      try {
        await connectToPeer(peerId, publicAddr)
        console.log('[TAURI] Connected to peer:', peerId)
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        setError(message)
        console.error('[TAURI] Failed to connect peer:', message)
      }
    },
    [isNativeMode]
  )

  // 피어 연결 해제
  const disconnectFromPeer = useCallback(
    async (peerId: string) => {
      if (!isNativeMode) return

      try {
        await disconnectPeer(peerId)
        console.log('[TAURI] Disconnected from peer:', peerId)
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err)
        setError(message)
        console.error('[TAURI] Failed to disconnect peer:', message)
      }
    },
    [isNativeMode]
  )

  // 통계 주기적 업데이트
  useEffect(() => {
    if (!isNativeMode || !isRunning) {
      if (statsIntervalRef.current) {
        clearInterval(statsIntervalRef.current)
        statsIntervalRef.current = null
      }
      return
    }

    const updateStats = async () => {
      try {
        const newStats = await getAudioStats()
        setStats(newStats)

        // 네트워크 정보도 업데이트
        try {
          const info = await getNetworkInfo()
          if (info.is_running) {
            setNetworkInfo(info)
          }
        } catch {
          // 네트워크가 시작되지 않은 경우 무시
        }
      } catch (err) {
        console.error('[TAURI] Failed to get stats:', err)
      }
    }

    // 초기 통계 로드
    updateStats()

    // 초기 모니터링 상태 로드
    getMonitoring().then(setIsMonitoring).catch(console.error)

    // 500ms마다 업데이트
    statsIntervalRef.current = setInterval(updateStats, 500)

    return () => {
      if (statsIntervalRef.current) {
        clearInterval(statsIntervalRef.current)
        statsIntervalRef.current = null
      }
    }
  }, [isNativeMode, isRunning])

  // 컴포넌트 언마운트 시 정리
  useEffect(() => {
    return () => {
      if (isNativeMode && isRunning) {
        stopAudio().catch(console.error)
        shutdownNetwork().catch(console.error)
      }
    }
  }, [])

  return {
    isNativeMode,
    isRunning,
    isMonitoring,
    stats,
    networkInfo,
    error,
    start,
    stop,
    updateSettings,
    toggleMonitoring,
    setMonitoringEnabled,
    startNetwork,
    stopNetwork,
    connectPeer,
    disconnectFromPeer,
  }
}
