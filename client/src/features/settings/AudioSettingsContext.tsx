/**
 * 네이티브 엔진 오디오 설정 (EngineConfig) 상태.
 * - 저장: profiles.audio_settings = { version: 2, engine: EngineConfig } (+ localStorage 캐시)
 * - 구형 형식(lib/tauri.ts 시절)은 읽을 때 감지해서 새 형식으로 변환하고 다시 저장한다.
 * - 웹(비Tauri)에서는 장치 조회/테스트를 하지 않는다(연주는 앱 전용).
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import type { Json } from '../../shared/lib/database.types'
import { isSupabaseConfigured } from '../../shared/lib/supabaseClient'
import { profileApi, useAudioSettingsQuery } from '../auth/profileQueries'
import { useAuth } from '../auth/useAuth'
import {
  engineErrorMessage,
  engineListDevices,
  engineStats,
  engineTestStart,
  engineTestStop,
  isNativeApp,
  type DeviceInfo,
  type EngineConfig,
  type EngineStats,
} from '../../native/engineApi'
import {
  DEFAULT_ENGINE_CONFIG,
  applyPreset as applyPresetToConfig,
  isPayloadAllowed,
  matchPreset,
  normalizeInputChannels,
  parseStoredSettings,
  toStored,
  validateEngineConfig,
  type ConfigIssue,
  type EnginePresetId,
} from '../../native/engineConfig'
import { SettingsSaveGate, TestMonitorControl } from '../../native/settingsSync'
import { reapplyLoopbackOffsetInBackground } from '../../native/loopbackOffsetStore'

const STORAGE_KEY = 'band_engine_config_v2'
const SAVE_DEBOUNCE_MS = 600
const TEST_POLL_MS = 100

interface AudioSettingsContextType {
  isNativeMode: boolean
  devices: DeviceInfo[]
  inputDevices: DeviceInfo[]
  outputDevices: DeviceInfo[]
  selectedInputDevice: DeviceInfo | null
  config: EngineConfig
  issues: ConfigIssue[]
  selectedPreset: EnginePresetId | null
  /** 적용하면 패킷 1200B를 넘는 변경은 거부하고 false */
  updateConfig: (patch: Partial<EngineConfig>) => boolean
  applyPreset: (preset: EnginePresetId) => void
  refreshDevices: () => Promise<void>
  isLoading: boolean
  error: string | null
  isSaving: boolean
  /** 구형 설정을 변환했으면 true (안내 표시용) */
  migrated: boolean

  // 테스트 모니터 (engine_test_start/stop)
  isTesting: boolean
  testStats: EngineStats | null
  testError: string | null
  startTest: () => Promise<void>
  stopTest: () => Promise<void>

  /** 합주실에서 엔진 사용 중(테스트 불가) — RoomContext가 설정 */
  engineInUse: boolean
  setEngineInUse: (inUse: boolean) => void
}

const AudioSettingsContext = createContext<AudioSettingsContextType | null>(null)

function loadLocal(): { config: EngineConfig; migrated: boolean } {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) return parseStoredSettings(JSON.parse(raw))
  } catch {
    // 저장소 접근 불가/손상
  }
  return { config: { ...DEFAULT_ENGINE_CONFIG }, migrated: false }
}

function saveLocal(config: EngineConfig) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(toStored(config)))
  } catch {
    // 무시
  }
}

export function AudioSettingsProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth()
  const [isNativeMode] = useState(isNativeApp)
  const [devices, setDevices] = useState<DeviceInfo[]>([])
  const [config, setConfig] = useState<EngineConfig>(() => loadLocal().config)
  const [isLoading, setIsLoading] = useState(isNativeMode)
  const [error, setError] = useState<string | null>(null)
  const [isSaving, setIsSaving] = useState(false)
  const [migrated, setMigrated] = useState(false)
  const [isTesting, setIsTesting] = useState(false)
  const [testStats, setTestStats] = useState<EngineStats | null>(null)
  const [testError, setTestError] = useState<string | null>(null)
  const [engineInUse, setEngineInUse] = useState(false)

  // 저장 게이트: 사용자 전환 시 초기화, 그 사용자의 DB 값을 읽기 전에는 저장하지 않음 (M7, L1)
  const gateRef = useRef(new SettingsSaveGate())
  const localDirtyRef = useRef(false)
  const configRef = useRef(config)
  configRef.current = config

  // --- DB 로드 (로그인 사용자, features/profile의 useAudioSettingsQuery) ---
  const userId = user?.id ?? null
  const stored = useAudioSettingsQuery(userId)
  const storedReady = stored.isSuccess
  const storedData = stored.data
  const storedError = stored.error
  useEffect(() => {
    if (storedError) console.warn('[AudioSettings] DB 설정 로드 실패:', storedError)
  }, [storedError])
  useEffect(() => {
    const gate = gateRef.current
    gate.setUser(userId)
    // 읽기 실패(행 없음 포함)면 loaded로 표시하지 않는다 → 그 사용자 동안 DB 저장 안 함
    if (!userId || !isSupabaseConfigured() || !gate.shouldLoad(userId) || !storedReady) return
    let cancelled = false
    void (async () => {
      try {
        if (storedData) {
          const parsed = parseStoredSettings(storedData)
          setConfig(parsed.config)
          saveLocal(parsed.config)
          if (parsed.migrated) {
            setMigrated(true)
            // 변환 저장 실패는 무시한다(다음 변경 때 다시 저장됨 — 기존 동작)
            await profileApi.saveAudioSettings(userId, toStored(parsed.config) as unknown as Json).catch(() => {})
            console.log('[AudioSettings] 구형 오디오 설정을 새 형식으로 변환했습니다')
          }
        }
        // 취소된 로드(StrictMode 이중 effect, 사용자 전환)는 loaded로 표시하지 않는다 → 다음 effect가 다시 적용
        if (!cancelled) gate.markLoaded(userId)
      } catch (err) {
        console.warn('[AudioSettings] DB 설정 로드 실패:', err)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [userId, storedReady, storedData])

  // --- 저장 (변경 시 디바운스) ---
  useEffect(() => {
    if (localDirtyRef.current) saveLocal(config)
    const gate = gateRef.current
    gate.setUser(userId)
    if (!userId || !isSupabaseConfigured() || !gate.shouldSave(userId)) return
    const timer = setTimeout(async () => {
      if (!gate.shouldSave(userId)) return
      setIsSaving(true)
      try {
        await profileApi.saveAudioSettings(userId, toStored(config) as unknown as Json)
      } catch (err) {
        console.warn('[AudioSettings] DB 저장 실패:', err)
      } finally {
        setIsSaving(false)
      }
    }, SAVE_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [config, userId])

  // --- 장치 ---
  const refreshDevices = useCallback(async () => {
    if (!isNativeMode) return
    setIsLoading(true)
    setError(null)
    try {
      const list = await engineListDevices()
      setDevices(list.filter((d) => d.name && d.name.trim() !== ''))
    } catch (err) {
      setError(engineErrorMessage(err))
    } finally {
      setIsLoading(false)
    }
  }, [isNativeMode])

  useEffect(() => {
    void refreshDevices()
  }, [refreshDevices])

  const inputDevices = useMemo(
    () => devices.filter((d) => d.isInput && d.driver === config.inputDriver),
    [devices, config.inputDriver],
  )
  const outputDevices = useMemo(
    () => devices.filter((d) => !d.isInput && d.driver === config.outputDriver),
    [devices, config.outputDriver],
  )
  const selectedInputDevice = useMemo(() => {
    if (config.inputDevice) return inputDevices.find((d) => d.id === config.inputDevice) ?? null
    return inputDevices.find((d) => d.isDefault) ?? null
  }, [inputDevices, config.inputDevice])

  const issues = useMemo(() => validateEngineConfig(config, selectedInputDevice), [config, selectedInputDevice])
  const selectedPreset = useMemo(() => matchPreset(config), [config])

  // --- 테스트 모니터 ---
  // 시작 중에 stop이 와도 시작이 끝난 뒤 정지 (L8: 합주 엔진 시작과 겹치지 않게)
  const testControlRef = useRef<TestMonitorControl | null>(null)
  if (!testControlRef.current) {
    testControlRef.current = new TestMonitorControl({
      start: async () => {
        const cfg = configRef.current
        await engineTestStart(cfg)
        // 앱 재시작 뒤에도 같은 장치 구성이면 메트로놈 출력 지연 보정값을 다시 넣는다(P2-3c)
        reapplyLoopbackOffsetInBackground(cfg)
        return null
      },
      stop: () => engineTestStop(),
    })
  }
  const testControl = testControlRef.current

  const stopTest = useCallback(async () => {
    setIsTesting(false)
    setTestStats(null)
    try {
      await testControl.stop()
    } catch (err) {
      console.warn('[AudioSettings] 테스트 정지 실패:', engineErrorMessage(err))
    }
  }, [testControl])

  const startTest = useCallback(async () => {
    if (!isNativeMode || testControl.isActive || testControl.isStarting) return
    if (engineInUse) {
      setTestError('합주 중에는 테스트할 수 없습니다. 연주를 멈춘 뒤 다시 시도하세요.')
      return
    }
    const problems = validateEngineConfig(config, selectedInputDevice)
    if (problems.length > 0) {
      setTestError(problems[0].message)
      return
    }
    setTestError(null)
    try {
      await testControl.start()
      setIsTesting(testControl.isActive)
    } catch (err) {
      setTestError(engineErrorMessage(err))
    }
  }, [isNativeMode, engineInUse, config, selectedInputDevice, testControl])

  useEffect(() => {
    if (!isTesting) return
    let cancelled = false
    const timer = setInterval(async () => {
      try {
        const s = await engineStats()
        if (!cancelled) setTestStats(s)
      } catch {
        // 무시
      }
    }, TEST_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [isTesting])

  // 합주 엔진이 시작되면 테스트 중지
  useEffect(() => {
    if (engineInUse) void stopTest()
  }, [engineInUse, stopTest])

  // --- 설정 변경 ---
  const commit = useCallback((next: EngineConfig) => {
    localDirtyRef.current = true
    gateRef.current.markEdited()
    setConfig(next)
    // 테스트 중 설정이 바뀌면 이전 설정의 테스트는 의미가 없으므로 중지
    if (testControl.isActive || testControl.isStarting) void stopTest()
  }, [stopTest, testControl])

  const updateConfig = useCallback((patch: Partial<EngineConfig>): boolean => {
    const next: EngineConfig = { ...config, ...patch }

    // 드라이버가 바뀌면 다른 드라이버의 장치 id는 무효
    if (patch.inputDriver && patch.inputDriver !== config.inputDriver && patch.inputDevice === undefined) {
      next.inputDevice = null
    }
    if (patch.outputDriver && patch.outputDriver !== config.outputDriver && patch.outputDevice === undefined) {
      next.outputDevice = null
    }
    // 채널 수/입력 장치 변경 시 입력 채널 정리
    if (patch.channels !== undefined || patch.inputDevice !== undefined || patch.inputDriver !== undefined || patch.inputChannels !== undefined) {
      const device = next.inputDevice
        ? devices.find((d) => d.id === next.inputDevice)
        : devices.find((d) => d.isInput && d.isDefault && d.driver === next.inputDriver)
      next.inputChannels = normalizeInputChannels(next.channels, next.inputChannels, device?.channels)
    }
    if (!isPayloadAllowed(next.codec, next.channels, next.frameMs, next.opusBitrate)) {
      return false
    }
    commit(next)
    return true
  }, [config, devices, commit])

  const applyPreset = useCallback((preset: EnginePresetId) => {
    commit(applyPresetToConfig(config, preset, selectedInputDevice))
  }, [config, selectedInputDevice, commit])

  const value: AudioSettingsContextType = {
    isNativeMode,
    devices,
    inputDevices,
    outputDevices,
    selectedInputDevice,
    config,
    issues,
    selectedPreset,
    updateConfig,
    applyPreset,
    refreshDevices,
    isLoading,
    error,
    isSaving,
    migrated,
    isTesting,
    testStats,
    testError,
    startTest,
    stopTest,
    engineInUse,
    setEngineInUse,
  }

  return <AudioSettingsContext.Provider value={value}>{children}</AudioSettingsContext.Provider>
}

// eslint-disable-next-line react-refresh/only-export-components
export function useAudioSettings() {
  const context = useContext(AudioSettingsContext)
  if (!context) throw new Error('useAudioSettings must be used within AudioSettingsProvider')
  return context
}
