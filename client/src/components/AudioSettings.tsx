import { useState, useEffect, useMemo, useCallback } from 'react'
import {
  isTauri,
  getAudioDevices,
  getAudioStats,
  startTestAudio,
  stopTestAudio,
  AudioDeviceInfo,
  AudioStats,
  AudioSettings as AudioSettingsType,
  AudioDriver,
  CompressionType,
  ChannelType,
  MonoChannelType,
  DEFAULT_AUDIO_SETTINGS,
  AUDIO_PRESETS,
} from '../lib/tauri'
import { supabase } from '../lib/supabaseClient'
import { useAuth } from '../contexts/AuthContext'
import { useAudioSettings } from '../contexts/AudioSettingsContext'
import './AudioSettings.css'

interface AudioSettingsProps {
  onClose?: () => void
  isModal?: boolean
  onSettingsChange?: (settings: AudioSettingsType) => void
  currentSettings?: AudioSettingsType
  onInputTestChange?: (testing: boolean) => void // 입력 테스트 상태 변경 콜백
}

type PresetType = 'high-quality' | 'standard' | 'low-latency' | 'bandwidth-priority' | 'custom'

export function AudioSettings({
  onClose,
  isModal = false,
  onSettingsChange,
  currentSettings,
  onInputTestChange,
}: AudioSettingsProps) {
  const { user } = useAuth()
  const { isInputTesting, setInputTesting } = useAudioSettings()
  const [isNativeMode] = useState(() => isTauri())
  const [devices, setDevices] = useState<AudioDeviceInfo[]>([])
  const [stats, setStats] = useState<AudioStats | null>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [selectedPreset, setSelectedPreset] = useState<PresetType>('standard')
  const [isSaving, setIsSaving] = useState(false)

  // 설정 상태
  const [settings, setSettings] = useState<AudioSettingsType>(
    currentSettings || DEFAULT_AUDIO_SETTINGS
  )

  // Supabase에서 설정 로드
  const loadSettingsFromDB = useCallback(async () => {
    if (!supabase || !user?.id) return

    try {
      const { data, error } = await supabase
        .from('profiles')
        .select('audio_settings')
        .eq('id', user.id)
        .single()

      if (error) throw error

      if (data?.audio_settings) {
        const dbSettings = data.audio_settings as AudioSettingsType
        setSettings({ ...DEFAULT_AUDIO_SETTINGS, ...dbSettings })
        console.log('[AudioSettings] Loaded from DB:', dbSettings)
      }
    } catch (err) {
      console.error('[AudioSettings] Failed to load from DB:', err)
    }
  }, [user?.id])

  // Supabase에 설정 저장
  const saveSettingsToDB = useCallback(async (newSettings: AudioSettingsType) => {
    if (!supabase || !user?.id) return

    setIsSaving(true)
    try {
      const { error } = await supabase
        .from('profiles')
        .update({ audio_settings: newSettings })
        .eq('id', user.id)

      if (error) throw error
      console.log('[AudioSettings] Saved to DB:', newSettings)
    } catch (err) {
      console.error('[AudioSettings] Failed to save to DB:', err)
    } finally {
      setIsSaving(false)
    }
  }, [user?.id])

  // 장치 목록 및 설정 로드
  useEffect(() => {
    if (!isNativeMode) {
      setIsLoading(false)
      return
    }

    const init = async () => {
      await loadDevices()
      await loadSettingsFromDB()
    }
    init()

    const interval = setInterval(loadStats, 1000)
    return () => clearInterval(interval)
  }, [isNativeMode, loadSettingsFromDB])

  const loadDevices = async () => {
    try {
      setIsLoading(true)
      setError(null)
      const deviceList = await getAudioDevices()
      setDevices(deviceList)
      await loadStats()
    } catch (err) {
      setError(err instanceof Error ? err.message : '장치를 불러오는데 실패했습니다')
    } finally {
      setIsLoading(false)
    }
  }

  const loadStats = async () => {
    try {
      const audioStats = await getAudioStats()
      setStats(audioStats)
    } catch {
      // Stats 로드 실패는 무시
    }
  }

  // 입력 테스트 토글
  const toggleInputTest = async () => {
    try {
      const newValue = !isInputTesting
      if (newValue) {
        // 테스트 시작 - 현재 설정으로 오디오 엔진 시작
        await startTestAudio(settings)
        console.log('[AudioSettings] Input test started with settings:', settings)
      } else {
        // 테스트 중지
        await stopTestAudio()
        console.log('[AudioSettings] Input test stopped')
      }
      setInputTesting(newValue)
      onInputTestChange?.(newValue)  // 레거시 콜백
    } catch (err) {
      console.error('[AudioSettings] Failed to toggle input test:', err)
    }
  }

  // 컴포넌트 언마운트 시 테스트 종료
  useEffect(() => {
    return () => {
      if (isInputTesting) {
        stopTestAudio().catch(() => {})
        setInputTesting(false)
        onInputTestChange?.(false)
      }
    }
  }, [isInputTesting, setInputTesting, onInputTestChange])

  // 선택한 드라이버에 해당하는 장치만 필터링
  const driverMap: Record<string, string[]> = {
    'Asio': ['ASIO'],
    'WasapiExclusive': ['WASAPI'],
    'WasapiShared': ['WASAPI'],
  }

  // 입력/출력에 각각 적용할 드라이버 결정
  const inputDriverKey = settings.input_driver || settings.driver
  const outputDriverKey = settings.output_driver || settings.driver
  const allowedInputDrivers = driverMap[inputDriverKey] || ['WASAPI']
  const allowedOutputDrivers = driverMap[outputDriverKey] || ['WASAPI']

  const filteredInputDevices = useMemo(() => {
    return devices.filter(d =>
      d.is_input &&
      d.name &&
      d.name.trim() !== '' &&
      allowedInputDrivers.includes(d.driver)
    )
  }, [devices, allowedInputDrivers])

  const filteredOutputDevices = useMemo(() => {
    return devices.filter(d =>
      !d.is_input &&
      d.name &&
      d.name.trim() !== '' &&
      allowedOutputDrivers.includes(d.driver)
    )
  }, [devices, allowedOutputDrivers])

  // 설정 변경 핸들러
  const updateSettings = (updates: Partial<AudioSettingsType>) => {
    const newSettings = { ...settings, ...updates }
    setSettings(newSettings)
    setSelectedPreset('custom')
    onSettingsChange?.(newSettings)
    saveSettingsToDB(newSettings)
  }

  // 프리셋 적용
  const applyPreset = (preset: PresetType) => {
    setSelectedPreset(preset)
    if (preset !== 'custom') {
      const presetSettings = AUDIO_PRESETS[preset]
      const newSettings = { ...settings, ...presetSettings }
      setSettings(newSettings)
      onSettingsChange?.(newSettings)
      saveSettingsToDB(newSettings)
    }
  }

  // 비 Tauri 환경
  if (!isNativeMode) {
    return (
      <div className={`audio-settings ${isModal ? 'modal' : ''}`}>
        <div className="audio-settings-content">
          <h2>오디오 설정</h2>
          <div className="native-only-notice">
            <div className="notice-icon">🎵</div>
            <h3>데스크톱 앱 전용 기능</h3>
            <p>
              네이티브 오디오 설정은 Band 데스크톱 앱에서만 사용 가능합니다.
              <br />
              ASIO/WASAPI 저지연 오디오를 사용하려면 앱을 설치해주세요.
            </p>
          </div>
          {onClose && (
            <div className="settings-actions">
              <button onClick={onClose} className="btn-secondary">닫기</button>
            </div>
          )}
        </div>
      </div>
    )
  }

  // 로딩 중
  if (isLoading) {
    return (
      <div className={`audio-settings ${isModal ? 'modal' : ''}`}>
        <div className="audio-settings-content">
          <h2>오디오 설정</h2>
          <div className="loading-state">
            <div className="loading-spinner" />
            <p>오디오 장치를 검색하는 중...</p>
          </div>
        </div>
      </div>
    )
  }

  // 에러
  if (error) {
    return (
      <div className={`audio-settings ${isModal ? 'modal' : ''}`}>
        <div className="audio-settings-content">
          <h2>오디오 설정</h2>
          <div className="error-state">
            <div className="error-icon">⚠️</div>
            <h3>오디오 장치 검색 실패</h3>
            <p>{error}</p>
            <button onClick={loadDevices} className="btn-primary">
              다시 시도
            </button>
          </div>
          {onClose && (
            <div className="settings-actions">
              <button onClick={onClose} className="btn-secondary">닫기</button>
            </div>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className={`audio-settings ${isModal ? 'modal' : ''}`}>
      {isModal && <div className="modal-backdrop" onClick={onClose} />}
      <div className="audio-settings-content">
        <div className="settings-header">
          <h2>오디오 설정</h2>
          {onClose && (
            <button onClick={onClose} className="close-btn">×</button>
          )}
        </div>

        {/* 음질 프리셋 */}
        <section className="settings-section">
          <h3>음질 프리셋</h3>
          <div className="preset-grid">
            {[
              { id: 'high-quality', label: '고음질', icon: '🎵', desc: '48kHz / 비압축 / 스테레오' },
              { id: 'standard', label: '표준', icon: '⚖️', desc: '48kHz / Opus / 모노' },
              { id: 'low-latency', label: '초저지연', icon: '⚡', desc: '48kHz / 비압축 / 64 samples' },
              { id: 'bandwidth-priority', label: '대역폭 우선', icon: '📶', desc: '44.1kHz / Opus / 모노' },
              { id: 'custom', label: '사용자 정의', icon: '⚙️', desc: '직접 설정' },
            ].map(preset => (
              <button
                key={preset.id}
                className={`preset-btn ${selectedPreset === preset.id ? 'active' : ''}`}
                onClick={() => applyPreset(preset.id as PresetType)}
              >
                <span className="preset-icon">{preset.icon}</span>
                <span className="preset-label">{preset.label}</span>
                <span className="preset-desc">{preset.desc}</span>
              </button>
            ))}
          </div>
        </section>

        {/* 드라이버 선택 */}
        <section className="settings-section">
          <h3>오디오 드라이버</h3>
          <div className="settings-row">
            <label>
              <input
                type="checkbox"
                checked={!!(settings.input_driver || settings.output_driver)}
                onChange={(e) => {
                  if (e.target.checked) {
                    // 입/출력 분리 모드 활성화 - 기본 드라이버로 초기화
                    updateSettings({
                      input_driver: settings.driver,
                      output_driver: settings.driver,
                    })
                  } else {
                    // 입/출력 분리 모드 비활성화
                    updateSettings({
                      input_driver: null,
                      output_driver: null,
                    })
                  }
                }}
              />
              입력/출력 드라이버 분리 (예: ASIO 입력 + WASAPI 출력)
            </label>
          </div>

          {/* 통합 드라이버 모드 */}
          {!settings.input_driver && !settings.output_driver && (
            <div className="driver-options">
              {[
                { id: 'Asio', label: 'ASIO', desc: '전문 오디오 인터페이스 (최저 지연)' },
                { id: 'WasapiExclusive', label: 'WASAPI Exclusive', desc: '낮은 지연 (단독 사용)' },
                { id: 'WasapiShared', label: 'WASAPI Shared', desc: '일반 사용 (다른 앱과 공유)' },
              ].map(driver => (
                <label key={driver.id} className="driver-option">
                  <input
                    type="radio"
                    name="driver"
                    checked={settings.driver === driver.id}
                    onChange={() => updateSettings({ driver: driver.id as AudioDriver })}
                  />
                  <span className="driver-info">
                    <span className="driver-label">{driver.label}</span>
                    <span className="driver-desc">{driver.desc}</span>
                  </span>
                </label>
              ))}
            </div>
          )}

          {/* 분리 드라이버 모드 */}
          {(settings.input_driver || settings.output_driver) && (
            <div className="split-driver-options">
              <div className="driver-column">
                <h4>입력 드라이버</h4>
                {[
                  { id: 'Asio', label: 'ASIO' },
                  { id: 'WasapiExclusive', label: 'WASAPI Exclusive' },
                  { id: 'WasapiShared', label: 'WASAPI Shared' },
                ].map(driver => (
                  <label key={driver.id} className="driver-option compact">
                    <input
                      type="radio"
                      name="input_driver"
                      checked={settings.input_driver === driver.id}
                      onChange={() => updateSettings({ input_driver: driver.id as AudioDriver })}
                    />
                    <span className="driver-label">{driver.label}</span>
                  </label>
                ))}
              </div>
              <div className="driver-column">
                <h4>출력 드라이버</h4>
                {[
                  { id: 'Asio', label: 'ASIO' },
                  { id: 'WasapiExclusive', label: 'WASAPI Exclusive' },
                  { id: 'WasapiShared', label: 'WASAPI Shared' },
                ].map(driver => (
                  <label key={driver.id} className="driver-option compact">
                    <input
                      type="radio"
                      name="output_driver"
                      checked={settings.output_driver === driver.id}
                      onChange={() => updateSettings({ output_driver: driver.id as AudioDriver })}
                    />
                    <span className="driver-label">{driver.label}</span>
                  </label>
                ))}
              </div>
            </div>
          )}
        </section>

        {/* 입력 장치 선택 */}
        <section className="settings-section">
          <h3>입력 장치 (마이크/오디오 인터페이스)</h3>
          <select
            value={settings.input_device || ''}
            onChange={(e) => updateSettings({ input_device: e.target.value || null })}
            className="device-select"
          >
            <option value="">기본 장치</option>
            {filteredInputDevices.map(device => (
              <option key={device.id} value={device.name}>
                {device.name} {device.is_default ? '(기본)' : ''}
              </option>
            ))}
          </select>
          <button onClick={loadDevices} className="refresh-btn">
            새로고침
          </button>
        </section>

        {/* 출력 장치 선택 */}
        <section className="settings-section">
          <h3>출력 장치 (스피커/헤드폰)</h3>
          <select
            value={settings.output_device || ''}
            onChange={(e) => updateSettings({ output_device: e.target.value || null })}
            className="device-select"
          >
            <option value="">기본 장치</option>
            {filteredOutputDevices.map(device => (
              <option key={device.id} value={device.name}>
                {device.name} {device.is_default ? '(기본)' : ''}
              </option>
            ))}
          </select>
        </section>

        {/* 입력 테스트 */}
        <section className="settings-section">
          <h3>입력 테스트</h3>
          <div className="settings-row">
            <label>내 마이크 소리 듣기</label>
            <button
              className={`toggle-btn ${isInputTesting ? 'active' : ''}`}
              onClick={toggleInputTest}
            >
              {isInputTesting ? '테스트 중' : '꺼짐'}
            </button>
          </div>
          <p className="setting-hint">
            ※ 테스트 중에는 합주실 마이크 전송이 일시 중지됩니다.
            <br />
            ※ 설정을 닫으면 자동으로 테스트가 종료됩니다.
          </p>
        </section>

        {/* 오디오 품질 설정 */}
        <section className="settings-section">
          <h3>오디오 품질</h3>
          <div className="settings-row">
            <label>샘플레이트</label>
            <select
              value={settings.sample_rate}
              onChange={(e) => updateSettings({ sample_rate: Number(e.target.value) })}
            >
              <option value={44100}>44.1 kHz</option>
              <option value={48000}>48 kHz</option>
            </select>
          </div>
          <div className="settings-row">
            <label>버퍼 크기</label>
            <select
              value={settings.buffer_size}
              onChange={(e) => updateSettings({ buffer_size: Number(e.target.value) })}
            >
              <option value={64}>64 samples (~1.3ms)</option>
              <option value={128}>128 samples (~2.7ms)</option>
              <option value={256}>256 samples (~5.3ms)</option>
              <option value={512}>512 samples (~10.7ms)</option>
              <option value={1024}>1024 samples (~21.3ms)</option>
            </select>
          </div>
          <div className="settings-row">
            <label>압축</label>
            <select
              value={settings.compression}
              onChange={(e) => updateSettings({ compression: e.target.value as CompressionType })}
            >
              <option value="Uncompressed">비압축 (최고 음질)</option>
              <option value="Opus">Opus (낮은 대역폭)</option>
            </select>
          </div>
          <div className="settings-row">
            <label>채널</label>
            <select
              value={settings.channels}
              onChange={(e) => updateSettings({ channels: e.target.value as ChannelType })}
            >
              <option value="Mono">모노</option>
              <option value="Stereo">스테레오</option>
            </select>
          </div>

          {/* 모노 채널 선택 (모노 모드일 때만 표시) */}
          {settings.channels === 'Mono' && (
            <div className="settings-row">
              <label>모노 입력 채널</label>
              <select
                value={settings.mono_channel || 'Ch1'}
                onChange={(e) => updateSettings({ mono_channel: e.target.value as MonoChannelType })}
              >
                <option value="Ch1">채널 1 (INPUT 1)</option>
                <option value="Ch2">채널 2 (INPUT 2)</option>
              </select>
              <p className="setting-hint">
                오디오 인터페이스의 어떤 입력을 사용할지 선택합니다.
              </p>
            </div>
          )}

          {/* 스테레오 L/R 스왑 (스테레오 모드일 때만 표시) */}
          {settings.channels === 'Stereo' && (
            <div className="settings-row">
              <label>L/R 채널 스왑</label>
              <button
                className={`toggle-btn ${settings.stereo_swap ? 'active' : ''}`}
                onClick={() => updateSettings({ stereo_swap: !settings.stereo_swap })}
              >
                {settings.stereo_swap ? '켜짐' : '꺼짐'}
              </button>
              <p className="setting-hint">
                좌우 채널을 바꿉니다. (INPUT 1↔INPUT 2)
              </p>
            </div>
          )}
        </section>

        {/* 현재 오디오 상태 */}
        {stats && (
          <section className="settings-section">
            <h3>현재 상태</h3>
            <div className="stats-grid">
              <div className="stat-item">
                <span className="stat-label">상태</span>
                <span className={`stat-value ${stats.is_running ? 'running' : 'stopped'}`}>
                  {stats.is_running ? '🟢 실행 중' : '🔴 정지'}
                </span>
              </div>
              <div className="stat-item">
                <span className="stat-label">드라이버</span>
                <span className="stat-value">{stats.driver}</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">샘플레이트</span>
                <span className="stat-value">{stats.sample_rate / 1000} kHz</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">버퍼</span>
                <span className="stat-value">{stats.buffer_size} samples</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">입력 레벨</span>
                <div className="level-meter">
                  <div
                    className="level-bar input"
                    style={{ width: `${Math.min(stats.input_level * 100, 100)}%` }}
                  />
                </div>
              </div>
              <div className="stat-item">
                <span className="stat-label">출력 레벨</span>
                <div className="level-meter">
                  <div
                    className="level-bar output"
                    style={{ width: `${Math.min(stats.output_level * 100, 100)}%` }}
                  />
                </div>
              </div>
            </div>
          </section>
        )}

        {/* 저장 안내 */}
        <div className="settings-footer">
          <p className="auto-save-notice">
            {isSaving ? '저장 중...' : '설정은 자동으로 저장됩니다'}
          </p>
          <p className="restart-notice">※ 설정 변경 시 방을 다시 입장해야 적용됩니다</p>
          {onClose && (
            <button onClick={onClose} className="btn-primary">
              완료
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
