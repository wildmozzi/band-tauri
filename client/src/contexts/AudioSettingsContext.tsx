import { createContext, useContext, useState, useEffect, useCallback, ReactNode } from 'react'
import {
  isTauri,
  getAudioDevices,
  getAudioStats,
  setAudioSettings as setTauriAudioSettings,
  AudioDeviceInfo,
  AudioHostInfo,
  AudioStats,
  LegacyAudioSettings as TauriAudioSettings,
} from '../lib/tauri'

// 스테레오 출력 모드 (웹 모드용)
export type StereoMode = 'stereo' | 'mono'

// 오디오 설정 타입 (하이브리드 - 네이티브 + 웹 호환)
export interface AudioSettings {
  // 공통
  inputDeviceId: string | null
  outputDeviceId: string | null
  sampleRate: number

  // 네이티브 전용
  hostId: string | null  // ASIO, WASAPI 등
  bufferSize: number

  // 웹(WebRTC) 전용 - 하위 호환성
  channelCount: number
  echoCancellation: boolean
  noiseSuppression: boolean
  autoGainControl: boolean
  stereoMode: StereoMode
}

// 실제 적용된 오디오 설정 (웹 모드용 - 하위 호환성)
export interface ActualAudioSettings {
  deviceId: string | null
  sampleRate: number | null
  channelCount: number | null
  echoCancellation: boolean | null
  noiseSuppression: boolean | null
  autoGainControl: boolean | null
  latency: number | null
}

// 프리셋 타입
export type AudioPreset = 'low-latency' | 'balanced' | 'quality' | 'custom'

interface AudioSettingsContextType {
  // Tauri 네이티브 모드 여부
  isNativeMode: boolean

  // 호스트/장치 목록 (네이티브)
  hosts: AudioHostInfo[]
  inputDevices: AudioDeviceInfo[]
  outputDevices: AudioDeviceInfo[]

  // 현재 설정
  settings: AudioSettings

  // 오디오 통계 (네이티브)
  stats: AudioStats | null

  // 설정 변경
  setInputDevice: (deviceId: string) => void
  setOutputDevice: (deviceId: string) => void
  setHost: (hostId: string) => void
  setSampleRate: (rate: number) => void
  setBufferSize: (size: number) => void
  setChannelCount: (count: number) => void
  setEchoCancellation: (enabled: boolean) => void
  setNoiseSuppression: (enabled: boolean) => void
  setAutoGainControl: (enabled: boolean) => void
  setStereoMode: (mode: StereoMode) => void

  // 프리셋
  applyPreset: (preset: AudioPreset) => void

  // 장치 목록 새로고침
  refreshDevices: () => Promise<void>

  // 초기화 상태
  isInitialized: boolean
  isLoading: boolean
  error: string | null

  // 입력 테스트 모드 (합주실 오디오 일시 중지)
  isInputTesting: boolean
  setInputTesting: (testing: boolean) => void
}

const defaultSettings: AudioSettings = {
  // 공통
  inputDeviceId: null,
  outputDeviceId: null,
  sampleRate: 48000,

  // 네이티브
  hostId: null,
  bufferSize: 256,

  // 웹
  channelCount: 1,
  echoCancellation: false,
  noiseSuppression: false,
  autoGainControl: false,
  stereoMode: 'mono',
}

// 프리셋 설정
const presets: Record<AudioPreset, Partial<AudioSettings>> = {
  'low-latency': {
    sampleRate: 48000,
    bufferSize: 64,
  },
  'balanced': {
    sampleRate: 48000,
    bufferSize: 256,
  },
  'quality': {
    sampleRate: 96000,
    bufferSize: 512,
  },
  'custom': {},
}

const STORAGE_KEY = 'bandspace_audio_settings_v2'

const AudioSettingsContext = createContext<AudioSettingsContextType | null>(null)

export function AudioSettingsProvider({ children }: { children: ReactNode }) {
  const [isNativeMode] = useState(isTauri())
  const [hosts, setHosts] = useState<AudioHostInfo[]>([])
  const [inputDevices, setInputDevices] = useState<AudioDeviceInfo[]>([])
  const [outputDevices, setOutputDevices] = useState<AudioDeviceInfo[]>([])
  const [settings, setSettings] = useState<AudioSettings>(defaultSettings)
  const [stats, setStats] = useState<AudioStats | null>(null)
  const [isInitialized, setIsInitialized] = useState(false)
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [isInputTesting, setInputTesting] = useState(false)

  // localStorage에서 설정 로드
  useEffect(() => {
    const savedSettings = localStorage.getItem(STORAGE_KEY)
    if (savedSettings) {
      try {
        const parsed = JSON.parse(savedSettings)
        setSettings({ ...defaultSettings, ...parsed })
      } catch (e) {
        console.error('Failed to parse saved audio settings:', e)
      }
    }
  }, [])

  // 설정 변경 시 localStorage에 저장
  useEffect(() => {
    if (isInitialized) {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(settings))
    }
  }, [settings, isInitialized])

  // 장치 목록 가져오기
  const refreshDevices = useCallback(async () => {
    if (!isNativeMode) {
      // 웹 모드: 브라우저 장치 사용
      setIsLoading(false)
      setIsInitialized(true)
      return
    }

    setIsLoading(true)
    setError(null)

    try {
      // 새 API는 flat array 반환, driver 필드로 호스트 구분
      const devices: AudioDeviceInfo[] = await getAudioDevices()

      // driver별로 호스트 목록 생성
      const driverSet = new Set(devices.map(d => d.driver))
      const hostList: AudioHostInfo[] = Array.from(driverSet).map(driver => ({
        id: driver,
        name: driver,
      }))

      // 입력/출력 장치 분리
      const inputList = devices.filter(d => d.is_input)
      const outputList = devices.filter(d => !d.is_input)

      setHosts(hostList)
      setInputDevices(inputList)
      setOutputDevices(outputList)

      // 저장된 장치가 없으면 기본 장치 선택
      const savedInput = settings.inputDeviceId
      const savedOutput = settings.outputDeviceId
      const savedHost = settings.hostId

      // 호스트 자동 선택: ASIO 우선, 없으면 첫 번째
      if (!savedHost && hostList.length > 0) {
        const asioHost = hostList.find(h => h.name.toLowerCase().includes('asio'))
        const defaultHost = asioHost || hostList[0]
        setSettings(prev => ({ ...prev, hostId: defaultHost.id }))
      }

      // 입력 장치 자동 선택
      if (!savedInput && inputList.length > 0) {
        const defaultDevice = inputList.find(d => d.is_default) || inputList[0]
        setSettings(prev => ({ ...prev, inputDeviceId: defaultDevice.id }))
      }

      // 출력 장치 자동 선택
      if (!savedOutput && outputList.length > 0) {
        const defaultDevice = outputList.find(d => d.is_default) || outputList[0]
        setSettings(prev => ({ ...prev, outputDeviceId: defaultDevice.id }))
      }

      setIsInitialized(true)
    } catch (err) {
      console.error('Failed to get audio devices:', err)
      setError(err instanceof Error ? err.message : 'Failed to get audio devices')
    } finally {
      setIsLoading(false)
    }
  }, [isNativeMode, settings.inputDeviceId, settings.outputDeviceId, settings.hostId])

  // 초기화
  useEffect(() => {
    refreshDevices()
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  // 오디오 통계 주기적 업데이트 (네이티브 모드만)
  useEffect(() => {
    if (!isNativeMode || !isInitialized) return

    const updateStats = async () => {
      try {
        const audioStats = await getAudioStats()
        setStats(audioStats)
      } catch {
        // 조용히 실패 (오디오 엔진이 아직 시작 안 됐을 수 있음)
      }
    }

    updateStats()
    const interval = setInterval(updateStats, 1000)
    return () => clearInterval(interval)
  }, [isNativeMode, isInitialized])

  // 백엔드에 오디오 설정 적용
  const applyAudioSettings = useCallback(async (newSettings: AudioSettings) => {
    if (!isNativeMode) return

    try {
      const tauriSettings: TauriAudioSettings = {
        sample_rate: newSettings.sampleRate,
        buffer_size: newSettings.bufferSize,
        opus_bitrate: 64000,  // 기본값
        monitor_buffer_ms: Math.max(10, Math.floor(newSettings.bufferSize * 1000 / newSettings.sampleRate * 2)),
        jitter_buffer_ms: 30,
        host_id: newSettings.hostId,
        input_device_id: newSettings.inputDeviceId,
        output_device_id: newSettings.outputDeviceId,
      }
      console.log('[AudioSettings] Applying:', tauriSettings)
      await setTauriAudioSettings(tauriSettings)
    } catch (err) {
      console.error('Failed to apply audio settings:', err)
    }
  }, [isNativeMode])

  // 설정 변경 함수들
  const setInputDevice = useCallback((deviceId: string) => {
    setSettings(prev => {
      const newSettings = { ...prev, inputDeviceId: deviceId }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  const setOutputDevice = useCallback((deviceId: string) => {
    setSettings(prev => {
      const newSettings = { ...prev, outputDeviceId: deviceId }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  const setHost = useCallback((hostId: string) => {
    setSettings(prev => {
      // 호스트 변경 시 장치 ID도 초기화 (다른 호스트의 장치는 호환 안 됨)
      const newSettings = { ...prev, hostId, inputDeviceId: null, outputDeviceId: null }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  const setSampleRate = useCallback((rate: number) => {
    setSettings(prev => {
      const newSettings = { ...prev, sampleRate: rate }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  const setBufferSize = useCallback((size: number) => {
    setSettings(prev => {
      const newSettings = { ...prev, bufferSize: size }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  const setChannelCount = useCallback((count: number) => {
    setSettings(prev => ({ ...prev, channelCount: count }))
  }, [])

  const setEchoCancellation = useCallback((enabled: boolean) => {
    setSettings(prev => ({ ...prev, echoCancellation: enabled }))
  }, [])

  const setNoiseSuppression = useCallback((enabled: boolean) => {
    setSettings(prev => ({ ...prev, noiseSuppression: enabled }))
  }, [])

  const setAutoGainControl = useCallback((enabled: boolean) => {
    setSettings(prev => ({ ...prev, autoGainControl: enabled }))
  }, [])

  const setStereoMode = useCallback((mode: StereoMode) => {
    setSettings(prev => ({ ...prev, stereoMode: mode }))
  }, [])

  // 프리셋 적용
  const applyPreset = useCallback((preset: AudioPreset) => {
    const presetSettings = presets[preset]
    setSettings(prev => {
      const newSettings = { ...prev, ...presetSettings }
      applyAudioSettings(newSettings)
      return newSettings
    })
  }, [applyAudioSettings])

  return (
    <AudioSettingsContext.Provider
      value={{
        isNativeMode,
        hosts,
        inputDevices,
        outputDevices,
        settings,
        stats,
        setInputDevice,
        setOutputDevice,
        setHost,
        setSampleRate,
        setBufferSize,
        setChannelCount,
        setEchoCancellation,
        setNoiseSuppression,
        setAutoGainControl,
        setStereoMode,
        applyPreset,
        refreshDevices,
        isInitialized,
        isLoading,
        error,
        isInputTesting,
        setInputTesting,
      }}
    >
      {children}
    </AudioSettingsContext.Provider>
  )
}

export function useAudioSettings() {
  const context = useContext(AudioSettingsContext)
  if (!context) {
    throw new Error('useAudioSettings must be used within AudioSettingsProvider')
  }
  return context
}

// 하위 호환성을 위한 타입 export
export type { AudioDeviceInfo as AudioDevice }
