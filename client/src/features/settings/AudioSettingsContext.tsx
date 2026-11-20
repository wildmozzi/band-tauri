/**
 * 네이티브 엔진 오디오 설정 (EngineConfig) 상태.
 * - 저장: profiles.audio_settings = { version: 2, engine: EngineConfig } (+ localStorage 캐시)
 * - 기기별 값(스레드 정책 `threadPolicy`)은 계정에 올리지 않고 이 PC에만 둔다(deviceLocalSettings, #245 재리뷰 L4).
 * - 구형 형식(lib/tauri.ts 시절)은 읽을 때 감지해서 새 형식으로 변환하고 다시 저장한다.
 * - 웹(비Tauri)에서는 장치 조회/테스트를 하지 않는다(연주는 앱 전용).
 * - 저장된 장치·드라이버가 이 PC에 없으면 엔진 시작 직전(`prepareRunConfig`)에 이번 실행만 기본 장치로 넘어간다(P2-27).
 *   `config`(저장값)는 고치지 않는다 — 원래 PC로 돌아가면 그대로 동작해야 하고, 사용자가 직접 바꿀 때만 저장된다.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import type { Json } from '../../shared/lib/database.types'
import { isSupabaseConfigured } from '../../shared/lib/supabaseClient'
import { profileApi, useAudioSettingsQuery, useAuth } from '../auth'
import {
  engineErrorMessage,
  engineListDevices,
  engineStats,
  engineSupportedDrivers,
  engineTestStart,
  engineTestStop,
  isNativeApp,
  type DeviceInfo,
  type DriverKind,
  type EngineConfig,
  type EngineStats,
  type ThreadPolicyConfig,
} from '../../native/engineApi'
import {
  DEFAULT_ENGINE_CONFIG,
  applyPreset as applyPresetToConfig,
  isPayloadAllowed,
  matchPreset,
  normalizeInputChannels,
  parseStoredSettings,
  toAccountStored,
  withDeviceLocal,
  validateEngineConfig,
  type ConfigIssue,
  type EnginePresetId,
} from '../../native/engineConfig'
import { SettingsSaveGate, TestMonitorControl } from '../../native/settingsSync'
import { reapplyLoopbackOffsetInBackground } from '../../native/loopbackOffsetStore'
import { fallbackNotice, needsDeviceRecheck, noDeviceMessage, resolveRunConfig, runInputDevice, type RunConfig } from '../../native/deviceFallback'
import { loadDeviceThreadPolicy, saveDeviceThreadPolicy } from './deviceLocalSettings'

const STORAGE_KEY = 'band_engine_config_v2'
const SAVE_DEBOUNCE_MS = 600
const TEST_POLL_MS = 100

/** 엔진 시작 직전에 쓸 이번 실행 설정(P2-27) */
export interface PreparedRun {
  /** 엔진에 넘길 설정(저장값에 폴백을 적용한 사본) */
  config: EngineConfig
  /** 폴백 알림. 같은 내용을 이 앱 실행에서 이미 알렸으면 null(한 번만 알린다) */
  notice: string | null
  /** 시작을 막는 문제(첫 항목을 보인다). 빈 배열이면 시작 가능 */
  issues: ConfigIssue[]
}

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
  /** 이 빌드가 지원하는 드라이버. null = 엔진이 알려 주지 않음(옛 엔진, 모두 표시) — P2-7 */
  supportedDrivers: DriverKind[] | null
  /** 저장된 드라이버·장치를 이 PC에서 쓸 수 없어 이번 실행은 기본 장치로 시작한다는 안내(저장값은 그대로). 없으면 null */
  fallbackNotice: string | null
  /** 저장된 장치 id가 지금 장치 목록에 없는 방향(설정 화면 선택지 표시용) */
  missingDevices: { input: string | null; output: string | null }
  /**
   * 엔진 시작 직전에 부른다: 저장된 장치가 목록에 없으면 목록을 새로 받아 보고, 그래도 없으면 이번 실행만 기본 장치로.
   * 저장값(`config`)·계정·로컬 캐시는 바꾸지 않는다.
   */
  prepareRunConfig: () => Promise<PreparedRun>
  /** 지금(마지막으로) 시작한 엔진에 넘긴 스레드 정책 — 환경변수가 덮었는지 판단용(#249 리뷰 Low-1). 모르면 null */
  engineThreadPolicy: ThreadPolicyConfig | null

  // 테스트 모니터 (engine_test_start/stop)
  isTesting: boolean
  testStats: EngineStats | null
  testError: string | null
  startTest: () => Promise<void>
  stopTest: () => Promise<void>

  /** 합주실에서 엔진 사용 중(테스트 불가) — RoomProvider가 설정 */
  engineInUse: boolean
  setEngineInUse: (inUse: boolean) => void
}

const AudioSettingsContext = createContext<AudioSettingsContextType | null>(null)

/** 로컬 캐시(계정 설정 사본) + 이 기기의 값. 옛 캐시에 들어 있던 `threadPolicy`는 쓰지 않는다(deviceLocalSettings 참고) */
function loadLocal(): EngineConfig {
  let config: EngineConfig = { ...DEFAULT_ENGINE_CONFIG }
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) config = parseStoredSettings(JSON.parse(raw)).config
  } catch {
    // 저장소 접근 불가/손상
  }
  return withDeviceLocal(config, loadDeviceThreadPolicy())
}

/** 이름 없는 장치는 쓰지 않는다 */
function usableDevices(list: DeviceInfo[]): DeviceInfo[] {
  return list.filter((d) => d.name && d.name.trim() !== '')
}

function saveLocal(config: EngineConfig) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(toAccountStored(config)))
  } catch {
    // 무시
  }
  saveDeviceThreadPolicy(config.threadPolicy)
}

export function AudioSettingsProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth()
  const [isNativeMode] = useState(isNativeApp)
  const [devices, setDevices] = useState<DeviceInfo[]>([])
  /** 장치 목록을 한 번이라도 받았는지(못 받았으면 폴백 판단을 하지 않는다 — 엔진이 직접 찾게 둔다) */
  const [devicesKnown, setDevicesKnown] = useState(false)
  const [config, setConfig] = useState<EngineConfig>(loadLocal)
  const [isLoading, setIsLoading] = useState(isNativeMode)
  const [error, setError] = useState<string | null>(null)
  const [isSaving, setIsSaving] = useState(false)
  const [migrated, setMigrated] = useState(false)
  const [supportedDrivers, setSupportedDrivers] = useState<DriverKind[] | null>(null)
  const [engineThreadPolicy, setEngineThreadPolicy] = useState<ThreadPolicyConfig | null>(null)
  const [isTesting, setIsTesting] = useState(false)
  const [testStats, setTestStats] = useState<EngineStats | null>(null)
  const [testError, setTestError] = useState<string | null>(null)
  const [engineInUse, setEngineInUse] = useState(false)

  // 저장 게이트: 사용자 전환 시 초기화, 그 사용자의 DB 값을 읽기 전에는 저장하지 않음 (M7, L1)
  const gateRef = useRef(new SettingsSaveGate())
  const localDirtyRef = useRef(false)
  const configRef = useRef(config)
  configRef.current = config

  // --- DB 로드 (로그인 사용자, features/auth의 useAudioSettingsQuery) ---
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
          // 계정 값에 옛 threadPolicy가 있어도 쓰지 않고 이 기기의 값을 유지한다(#245 재리뷰 L4)
          const next = withDeviceLocal(parsed.config, configRef.current.threadPolicy ?? loadDeviceThreadPolicy())
          setConfig(next)
          saveLocal(next)
          if (parsed.migrated) {
            setMigrated(true)
            // 변환 저장 실패는 무시한다(다음 변경 때 다시 저장됨 — 기존 동작)
            await profileApi.saveAudioSettings(userId, toAccountStored(parsed.config) as unknown as Json).catch(() => {})
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
        await profileApi.saveAudioSettings(userId, toAccountStored(config) as unknown as Json)
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
      setDevices(usableDevices(list))
      setDevicesKnown(true)
    } catch (err) {
      setError(engineErrorMessage(err))
    } finally {
      setIsLoading(false)
    }
  }, [isNativeMode])

  useEffect(() => {
    void refreshDevices()
  }, [refreshDevices])

  // --- 지원 드라이버 (P2-7): 빌드마다 고정이라 한 번만 묻는다 ---
  // 엔진 시작 직전에 아직 응답 전이면 같은 요청을 기다린다(prepareRunConfig)
  const supportedPromiseRef = useRef<Promise<DriverKind[] | null> | null>(null)
  useEffect(() => {
    if (!isNativeMode) return
    let cancelled = false
    supportedPromiseRef.current ??= engineSupportedDrivers()
    void supportedPromiseRef.current.then((list) => {
      if (!cancelled) setSupportedDrivers(list)
    })
    return () => {
      cancelled = true
    }
  }, [isNativeMode])

  // 저장된 드라이버를 이 배포본이 지원하지 않거나 저장된 장치가 이 PC에 없으면 이번 실행만 기본 장치로(P2-27).
  // 예전(P2-7)에는 여기서 config(저장값)를 지원 드라이버로 바꿔 다음 편집 때 계정에 저장됐다 — 다른 PC의 ASIO 설정을 지웠다.
  // 이제는 저장값을 두고 안내만 한다(설정 화면은 시작 전 안내, 엔진 시작 때는 prepareRunConfig가 고친 사본을 넘긴다).
  const knownDevices = devicesKnown ? devices : null
  const runPreview = useMemo(() => resolveRunConfig(config, knownDevices, supportedDrivers), [config, knownDevices, supportedDrivers])
  const previewNotice = useMemo(() => fallbackNotice(runPreview.fallbacks, 'future'), [runPreview])
  const missingDevices = useMemo(() => {
    const missing = (dir: 'input' | 'output') => {
      const f = runPreview.fallbacks.find((x) => x.kind === 'device' && x.dir === dir)
      return f && f.kind === 'device' ? f.savedId : null
    }
    return { input: missing('input'), output: missing('output') }
  }, [runPreview])

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

  // --- 엔진 시작 직전: 이번 실행 설정 (P2-27) ---
  const devicesRef = useRef<DeviceInfo[] | null>(knownDevices)
  devicesRef.current = knownDevices
  /** 이 앱 실행에서 이미 알린 폴백 문구(같은 폴백은 한 번만 알린다) */
  const shownNoticesRef = useRef(new Set<string>())
  const prepareRunConfig = useCallback(async (): Promise<PreparedRun> => {
    const saved = configRef.current
    const supported = supportedPromiseRef.current ? await supportedPromiseRef.current : null
    let list = devicesRef.current
    // 저장된 장치가 목록에 없으면 시작 전에 한 번 새로 받아 본다(앱을 켠 뒤 인터페이스를 꽂은 경우). 있으면 바로 시작
    if (list && needsDeviceRecheck(saved, list)) {
      try {
        list = usableDevices(await engineListDevices())
        setDevices(list)
      } catch {
        // 목록을 다시 못 받으면 마지막 목록으로 판단한다
      }
    }
    const run: RunConfig = resolveRunConfig(saved, list, supported)
    const issues = validateEngineConfig(run.config, runInputDevice(run.config, list))
    const noDevice = noDeviceMessage(run.noDevices, run.config)
    if (noDevice) issues.unshift({ field: run.noDevices[0] === 'input' ? 'inputDevice' : 'outputDevice', message: noDevice })
    let notice = run.notice
    if (notice && issues.length === 0) {
      if (shownNoticesRef.current.has(notice)) notice = null
      else shownNoticesRef.current.add(notice)
    }
    if (issues.length === 0) setEngineThreadPolicy(run.config.threadPolicy ?? null)
    return { config: run.config, notice: issues.length === 0 ? notice : null, issues }
  }, [])

  // --- 테스트 모니터 ---
  // 시작 중에 stop이 와도 시작이 끝난 뒤 정지 (L8: 합주 엔진 시작과 겹치지 않게)
  const testControlRef = useRef<TestMonitorControl | null>(null)
  const testRunConfigRef = useRef<EngineConfig | null>(null)
  /** 테스트 모니터를 이번 실행에서 폴백으로 시작했을 때 알림(설정 화면에 보인다) */
  const [testNotice, setTestNotice] = useState<string | null>(null)
  if (!testControlRef.current) {
    testControlRef.current = new TestMonitorControl({
      start: async () => {
        // startTest가 prepareRunConfig로 고친 이번 실행 설정(저장값이 아님)
        const cfg = testRunConfigRef.current ?? configRef.current
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
    const run = await prepareRunConfig()
    if (run.issues.length > 0) {
      setTestError(run.issues[0].message)
      return
    }
    setTestError(null)
    testRunConfigRef.current = run.config
    try {
      await testControl.start()
      setIsTesting(testControl.isActive)
      if (run.notice) setTestNotice(run.notice)
    } catch (err) {
      setTestError(engineErrorMessage(err))
    }
  }, [isNativeMode, engineInUse, config, selectedInputDevice, testControl, prepareRunConfig])

  useEffect(() => {
    if (!isTesting) return
    let cancelled = false
    // 앞 요청이 아직이면(느린 응답) 이번 주기는 건너뛰고 마지막 표시를 둔다(#214 리뷰 L4)
    let inFlight = false
    const timer = setInterval(async () => {
      if (inFlight) return
      inFlight = true
      try {
        const s = await engineStats()
        if (!cancelled) setTestStats(s)
      } catch {
        // 무시
      } finally {
        inFlight = false
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
    supportedDrivers,
    fallbackNotice: testNotice && isTesting ? testNotice : previewNotice,
    missingDevices,
    prepareRunConfig,
    engineThreadPolicy,
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
