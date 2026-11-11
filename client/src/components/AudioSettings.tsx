/**
 * 네이티브 엔진 오디오 설정 화면 (EngineConfig / DeviceInfo, 설계 4절).
 * 상태와 저장은 AudioSettingsContext가 맡고, 이 컴포넌트는 표시와 입력만 한다.
 */
import { useEffect, useRef, useState } from 'react'
import { useAudioSettings } from '../contexts/AudioSettingsContext'
import {
  MAX_PAYLOAD_BYTES,
  engineSetJitterProfile,
  engineStats,
  type CodecKind,
  type DriverKind,
  type FrameMs,
  type PeerStats,
} from '../native/engineApi'
import {
  BUFFER_OPTIONS,
  DRIVER_OPTIONS,
  ENGINE_PRESETS,
  FRAME_OPTIONS,
  JITTER_MAX_LIMIT_MS,
  JITTER_PROFILE_DEFAULT,
  bufferMs,
  clampJitterProfile,
  estimatePayloadBytes,
  formatPeerJitter,
  isPayloadAllowed,
  jitterProfileLabel,
  type EnginePresetId,
} from '../native/engineConfig'
import { LatencyMeasurePanel } from './LatencyMeasurePanel'
import { DesktopDownloadButton } from './DesktopDownloadButton'
import './AudioSettings.css'

interface AudioSettingsProps {
  onClose?: () => void
  isModal?: boolean
}

const PEER_STATS_POLL_MS = 1000
const PROFILE_APPLY_DEBOUNCE_MS = 200

/** 합주 중(엔진 사용 중)일 때만 피어 통계를 1초마다 읽는다(지터 버퍼 상태 표시용). */
function useLivePeerStats(enabled: boolean): PeerStats[] {
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

function LevelMeter({ value, kind }: { value: number; kind: 'input' | 'output' }) {
  const pct = Math.max(0, Math.min(100, value * 100))
  return (
    <div className="level-meter">
      <div className={`level-bar ${kind} ${pct > 90 ? 'clip' : ''}`} style={{ width: `${pct}%` }} />
    </div>
  )
}

export function AudioSettings({ onClose, isModal = false }: AudioSettingsProps) {
  const {
    isNativeMode,
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
  } = useAudioSettings()

  const livePeers = useLivePeerStats(isNativeMode && engineInUse)
  // 슬라이더 드래그 중에는 표시만 바꾸고, 엔진 적용(IPC)은 마지막 값으로 200ms 뒤 한 번만(PR #20 M3).
  const profileTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (profileTimer.current) clearTimeout(profileTimer.current)
  }, [])
  const scheduleProfileApply = (p: number) => {
    if (profileTimer.current) clearTimeout(profileTimer.current)
    profileTimer.current = setTimeout(() => {
      profileTimer.current = null
      void engineSetJitterProfile(p).catch(() => undefined)
    }, PROFILE_APPLY_DEBOUNCE_MS)
  }

  // 설정 화면을 닫으면 테스트 종료
  useEffect(() => () => {
    void stopTest()
  }, [stopTest])

  const wrap = (body: React.ReactNode) => (
    <div className={`audio-settings ${isModal ? 'modal' : ''}`}>
      {isModal && <div className="modal-backdrop" onClick={onClose} />}
      <div className="audio-settings-content">
        <div className="settings-header">
          <h2>오디오 설정</h2>
          {onClose && <button onClick={onClose} className="close-btn">×</button>}
        </div>
        {body}
      </div>
    </div>
  )

  // 웹: 연주는 앱 전용
  if (!isNativeMode) {
    return wrap(
      <>
        <div className="native-only-notice">
          <div className="notice-icon">🎵</div>
          <h3>데스크톱 앱 전용 기능</h3>
          <p>
            연주와 오디오 장치 설정은 데스크톱 앱에서만 할 수 있습니다.
            <br />
            웹에서는 합주를 관람(청취)할 수 있습니다.
          </p>
          <DesktopDownloadButton className="btn-primary" />
        </div>
        {onClose && (
          <div className="settings-actions">
            <button onClick={onClose} className="btn-secondary">닫기</button>
          </div>
        )}
      </>,
    )
  }

  if (isLoading) {
    return wrap(
      <div className="loading-state">
        <div className="loading-spinner" />
        <p>오디오 장치를 검색하는 중...</p>
      </div>,
    )
  }

  if (error) {
    return wrap(
      <div className="error-state">
        <div className="error-icon">⚠️</div>
        <h3>오디오 장치 검색 실패</h3>
        <p>{error}</p>
        <button onClick={() => void refreshDevices()} className="btn-primary">다시 시도</button>
      </div>,
    )
  }

  const payloadBytes = estimatePayloadBytes(config.codec, config.channels, config.frameMs, config.opusBitrate)
  const deviceChannels = Math.max(1, selectedInputDevice?.channels ?? 2)
  const channelOptions = Array.from({ length: deviceChannels }, (_, i) => i)
  const allowed = (patch: { codec?: CodecKind; channels?: 1 | 2; frameMs?: FrameMs }) =>
    isPayloadAllowed(patch.codec ?? config.codec, patch.channels ?? config.channels, patch.frameMs ?? config.frameMs, config.opusBitrate)

  const testLatency = testStats?.estLatencyMs

  return wrap(
    <>
      {migrated && (
        <div className="settings-notice">
          이전 버전의 오디오 설정을 새 엔진 형식으로 변환했습니다. 장치 선택을 확인해주세요.
        </div>
      )}

      {/* 프리셋 */}
      <section className="settings-section">
        <h3>프리셋</h3>
        <div className="preset-grid">
          {(Object.keys(ENGINE_PRESETS) as EnginePresetId[]).map((id) => (
            <button
              key={id}
              className={`preset-btn ${selectedPreset === id ? 'active' : ''}`}
              onClick={() => applyPreset(id)}
            >
              <span className="preset-label">{ENGINE_PRESETS[id].label}</span>
              <span className="preset-desc">{ENGINE_PRESETS[id].desc}</span>
            </button>
          ))}
          <div className={`preset-btn static ${selectedPreset === null ? 'active' : ''}`}>
            <span className="preset-label">사용자 정의</span>
            <span className="preset-desc">아래에서 직접 설정</span>
          </div>
        </div>
      </section>

      {/* 드라이버 + 장치 */}
      <section className="settings-section">
        <h3>입력</h3>
        <div className="settings-row">
          <label htmlFor="input-driver">드라이버</label>
          <select
            id="input-driver"
            value={config.inputDriver}
            onChange={(e) => updateConfig({ inputDriver: e.target.value as DriverKind })}
          >
            {DRIVER_OPTIONS.map((d) => (
              <option key={d.id} value={d.id}>{d.label}</option>
            ))}
          </select>
        </div>
        <p className="setting-hint">{DRIVER_OPTIONS.find((d) => d.id === config.inputDriver)?.desc}</p>
        <div className="settings-row">
          <label htmlFor="input-device">장치</label>
          <select
            id="input-device"
            className="device-select"
            value={config.inputDevice ?? ''}
            onChange={(e) => updateConfig({ inputDevice: e.target.value || null })}
          >
            <option value="">기본 장치</option>
            {inputDevices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name} ({d.channels}ch){d.isDefault ? ' · 기본' : ''}
              </option>
            ))}
          </select>
        </div>
        {inputDevices.length === 0 && <p className="setting-hint">이 드라이버로 찾은 입력 장치가 없습니다.</p>}
      </section>

      <section className="settings-section">
        <h3>출력</h3>
        <div className="settings-row">
          <label htmlFor="output-driver">드라이버</label>
          <select
            id="output-driver"
            value={config.outputDriver}
            onChange={(e) => updateConfig({ outputDriver: e.target.value as DriverKind })}
          >
            {DRIVER_OPTIONS.map((d) => (
              <option key={d.id} value={d.id}>{d.label}</option>
            ))}
          </select>
        </div>
        <div className="settings-row">
          <label htmlFor="output-device">장치</label>
          <select
            id="output-device"
            className="device-select"
            value={config.outputDevice ?? ''}
            onChange={(e) => updateConfig({ outputDevice: e.target.value || null })}
          >
            <option value="">기본 장치</option>
            {outputDevices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}{d.isDefault ? ' · 기본' : ''}
              </option>
            ))}
          </select>
        </div>
        {config.inputDriver === 'asio' && config.outputDriver === 'asio' && (
          <p className="setting-hint">ASIO는 입력과 출력을 같은 장치(드라이버)로 엽니다.</p>
        )}
        <button onClick={() => void refreshDevices()} className="refresh-btn">장치 새로고침</button>
      </section>

      {/* 엔진/네트워크 */}
      <section className="settings-section">
        <h3>엔진</h3>
        <div className="settings-row">
          <label htmlFor="buffer-size">장치 버퍼</label>
          <select
            id="buffer-size"
            value={config.bufferSize}
            onChange={(e) => updateConfig({ bufferSize: Number(e.target.value) })}
          >
            {BUFFER_OPTIONS.map((b) => (
              <option key={b} value={b}>{b} samples (~{bufferMs(b)}ms)</option>
            ))}
          </select>
        </div>
        <div className="settings-row">
          <label htmlFor="frame-ms">네트워크 프레임</label>
          <select
            id="frame-ms"
            value={config.frameMs}
            onChange={(e) => updateConfig({ frameMs: Number(e.target.value) as FrameMs })}
          >
            {FRAME_OPTIONS.map((f) => (
              <option key={f} value={f} disabled={!allowed({ frameMs: f })}>
                {f}ms{!allowed({ frameMs: f }) ? ' (이 코덱/채널에서 불가)' : ''}
              </option>
            ))}
          </select>
        </div>
        <div className="settings-row">
          <label htmlFor="codec">코덱</label>
          <select
            id="codec"
            value={config.codec}
            onChange={(e) => updateConfig({ codec: e.target.value as CodecKind })}
          >
            <option value="opus" disabled={!allowed({ codec: 'opus' })}>Opus (압축, 대역폭 적음)</option>
            <option value="pcm16" disabled={!allowed({ codec: 'pcm16' })}>
              PCM16 (비압축, 코덱 지연 없음){!allowed({ codec: 'pcm16' }) ? ' — 현재 프레임에서 불가' : ''}
            </option>
          </select>
        </div>
        <div className="settings-row">
          <label htmlFor="channels">채널</label>
          <select
            id="channels"
            value={config.channels}
            onChange={(e) => updateConfig({ channels: Number(e.target.value) === 2 ? 2 : 1 })}
          >
            <option value={1}>모노</option>
            <option value={2} disabled={!allowed({ channels: 2 })}>
              스테레오{!allowed({ channels: 2 }) ? ' — 현재 코덱/프레임에서 불가' : ''}
            </option>
          </select>
        </div>
        <p className={`setting-hint ${payloadBytes > MAX_PAYLOAD_BYTES ? 'error' : ''}`}>
          패킷 페이로드 약 {payloadBytes}B / 최대 {MAX_PAYLOAD_BYTES}B
          {config.codec === 'pcm16' && ' · PCM16 스테레오는 프레임 5ms 이하만 가능'}
        </p>

        {/* 입력 채널 선택 */}
        {config.channels === 1 ? (
          <div className="settings-row">
            <label htmlFor="input-ch">입력 채널</label>
            <select
              id="input-ch"
              value={config.inputChannels[0]}
              onChange={(e) => updateConfig({ inputChannels: [Number(e.target.value)] })}
            >
              {channelOptions.map((ch) => <option key={ch} value={ch}>Ch{ch + 1}</option>)}
            </select>
          </div>
        ) : (
          <div className="settings-row">
            <label>입력 채널 (L / R)</label>
            <select
              aria-label="왼쪽 입력 채널"
              value={config.inputChannels[0]}
              onChange={(e) => updateConfig({ inputChannels: [Number(e.target.value), config.inputChannels[1] ?? 1] })}
            >
              {channelOptions.map((ch) => <option key={ch} value={ch}>L: Ch{ch + 1}</option>)}
            </select>
            <select
              aria-label="오른쪽 입력 채널"
              value={config.inputChannels[1] ?? 1}
              onChange={(e) => updateConfig({ inputChannels: [config.inputChannels[0], Number(e.target.value)] })}
            >
              {channelOptions.map((ch) => <option key={ch} value={ch}>R: Ch{ch + 1}</option>)}
            </select>
          </div>
        )}
      </section>

      {/* 지터 버퍼 */}
      <section className="settings-section">
        <h3>지터 버퍼</h3>
        <div className="settings-row">
          <label htmlFor="jitter-profile">저지연 ↔ 안정</label>
          <input
            id="jitter-profile"
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={config.jitterProfile ?? JITTER_PROFILE_DEFAULT}
            onChange={(e) => {
              const p = clampJitterProfile(Number(e.target.value))
              updateConfig({ jitterProfile: p })
              // 합주 중이면 적용(디바운스). 다음 시작에는 저장된 설정을 쓴다
              if (engineInUse) scheduleProfileApply(p)
            }}
          />
          <span>{jitterProfileLabel(config.jitterProfile ?? JITTER_PROFILE_DEFAULT)}</span>
        </div>
        <p className="setting-hint">
          저지연: 버퍼를 최소로(가끔 끊김 감수) · 안정: 지터 스파이크에 여유를 더 오래 두고 반복되는 스파이크도 일부 덮음(회선에 따라 지연이 크게 늘 수 있음). 합주 중에도 바로 적용됩니다.
        </p>
        {engineInUse && (
          <div className="stats-grid" aria-label="피어별 지터 버퍼 상태">
            {livePeers.length === 0 && <p className="setting-hint">연결된 피어가 없습니다.</p>}
            {livePeers.map((ps) => (
              <div className="stat-item" key={ps.peerId}>
                <span className="stat-label">{ps.peerId.slice(0, 8)}</span>
                <span className="stat-value">{formatPeerJitter(ps)}</span>
              </div>
            ))}
          </div>
        )}
        <div className="settings-row">
          <label htmlFor="jitter-min">최소 (0 = 자동)</label>
          <input
            id="jitter-min"
            type="number"
            min={0}
            max={JITTER_MAX_LIMIT_MS}
            step={5}
            value={config.jitterMinMs ?? 0}
            onChange={(e) => updateConfig({ jitterMinMs: Math.max(0, Number(e.target.value) || 0) })}
          />
          <span>ms</span>
        </div>
        <div className="settings-row">
          <label htmlFor="jitter-max">최대</label>
          <input
            id="jitter-max"
            type="number"
            min={10}
            max={JITTER_MAX_LIMIT_MS}
            step={5}
            value={config.jitterMaxMs ?? 80}
            onChange={(e) => updateConfig({ jitterMaxMs: Math.max(0, Number(e.target.value) || 0) })}
          />
          <span>ms</span>
        </div>
        <p className="setting-hint">네트워크가 불안정하면 최대값을 올리고, 지연을 줄이려면 내리세요.</p>
      </section>

      {/* 테스트 모니터 */}
      <section className="settings-section">
        <h3>테스트 모니터</h3>
        <div className="settings-row">
          <label>내 입력 소리 듣기 (네트워크 없이)</label>
          <button
            className={`toggle-btn ${isTesting ? 'active' : ''}`}
            onClick={() => void (isTesting ? stopTest() : startTest())}
            disabled={engineInUse || (!isTesting && issues.length > 0)}
          >
            {isTesting ? '테스트 중 (끄기)' : '테스트 시작'}
          </button>
        </div>
        {engineInUse && <p className="setting-hint">합주 중에는 테스트할 수 없습니다.</p>}
        {testError && <p className="setting-hint error">{testError}</p>}
        <div className="stats-grid">
          <div className="stat-item">
            <span className="stat-label">입력 레벨</span>
            <LevelMeter value={isTesting ? testStats?.inputLevel ?? 0 : 0} kind="input" />
          </div>
          <div className="stat-item">
            <span className="stat-label">출력 레벨</span>
            <LevelMeter value={isTesting ? testStats?.outputLevel ?? 0 : 0} kind="output" />
          </div>
          {isTesting && testStats && (
            <>
              <div className="stat-item">
                <span className="stat-label">로컬 처리 지연</span>
                <span className="stat-value">{testLatency !== undefined ? `${testLatency.toFixed(1)}ms` : '--'}</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">장치</span>
                <span className="stat-value">{testStats.sampleRate / 1000}kHz · 버퍼 {testStats.deviceBuffer}</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">xrun</span>
                <span className={`stat-value ${testStats.xruns > 0 ? 'stopped' : 'running'}`}>{testStats.xruns}</span>
              </div>
            </>
          )}
        </div>
        <p className="setting-hint">※ 설정을 바꾸거나 이 화면을 닫으면 테스트가 종료됩니다.</p>
      </section>

      <LatencyMeasurePanel />

      {issues.length > 0 && (
        <section className="settings-section">
          <h3>설정 확인 필요</h3>
          <ul className="settings-issues">
            {issues.map((issue) => <li key={`${issue.field}-${issue.message}`}>{issue.message}</li>)}
          </ul>
        </section>
      )}

      <div className="settings-footer">
        <p className="auto-save-notice">{isSaving ? '저장 중...' : '설정은 자동으로 저장됩니다'}</p>
        <p className="restart-notice">※ 합주 중 바꾼 설정은 연주를 다시 시작하면 적용됩니다</p>
        {onClose && <button onClick={onClose} className="btn-primary">완료</button>}
      </div>
    </>,
  )
}
