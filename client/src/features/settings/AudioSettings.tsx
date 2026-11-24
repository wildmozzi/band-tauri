/**
 * 네이티브 엔진 오디오 설정 화면 (EngineConfig / DeviceInfo, 설계 4절).
 * 상태와 저장은 AudioSettingsContext가 맡고, 이 컴포넌트는 표시와 입력만 한다.
 */
import { useEffect } from 'react'
import { useAudioSettings } from './AudioSettingsContext'
import { MAX_PAYLOAD_BYTES, type CodecKind, type DriverKind, type FrameMs, type PeerStats, type ResampleQuality } from '../../native/engineApi'
import {
  BUFFER_OPTIONS,
  DRIVER_OPTIONS,
  ENGINE_PRESETS,
  FRAME_OPTIONS,
  JITTER_MAX_LIMIT_MS,
  JITTER_PROFILE_DEFAULT,
  RESAMPLE_QUALITY_OPTIONS,
  bufferMs,
  clampJitterProfile,
  estimatePayloadBytes,
  formatPeerJitter,
  jitterProfileLabel,
  resolveResampleQuality,
  type EnginePresetId,
  DEFAULT_THREAD_POLICY,
} from '../../native/engineConfig'
import { driverChoices, exclusiveModeHint } from './driverSupport'
import { deviceNameFromId } from '../../native/deviceFallback'
import { LatencyMeasurePanel } from './LatencyMeasurePanel'
import { DesktopDownloadButton } from '../../shared/ui/DesktopDownloadButton'
import { Modal } from '../../shared/ui'
import { DiagnosticsExport } from './DiagnosticsExport'
import { DevServerSettingsSection } from './DevServerSettings'
import { useLiveEngineStats } from './hooks/useLivePeerStats'
import { useDebouncedJitterProfile } from './hooks/useDebouncedJitterProfile'
import {
  bufferFrameHint,
  formatTestDevice,
  formatTestLatency,
  formatTestResampler,
  inputChannelOptions,
  isClipping,
  levelPercent,
  parseChannels,
  parseJitterMs,
  payloadAllowed,
  peerLabel,
  isWindowsUserAgent,
  resampleQualityHint,
  threadPolicyStatus,
  THREAD_POLICY_OPTIONS,
  withLeftInputChannel,
  withRightInputChannel,
  type PayloadPatch,
} from './audioSettingsView'
import './AudioSettings.css'
import './audioSettingsPanel.css'

interface AudioSettingsProps {
  onClose?: () => void
  isModal?: boolean
}

const NO_PEERS: PeerStats[] = []
/** 이 PC가 Windows인지(스레드 정책 결과 안내용). 앱 수명 동안 바뀌지 않는다 */
const IS_WINDOWS = isWindowsUserAgent(typeof navigator === 'undefined' ? undefined : navigator.userAgent)

function LevelMeter({ value, kind }: { value: number; kind: 'input' | 'output' }) {
  const pct = levelPercent(value)
  return (
    <div className="level-meter">
      <div className={`level-bar ${kind} ${isClipping(pct) ? 'clip' : ''}`} style={{ width: `${pct}%` }} />
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
    supportedDrivers,
    fallbackNotice,
    missingDevices,
    engineThreadPolicy,
    isTesting,
    testStats,
    testError,
    startTest,
    stopTest,
    engineInUse,
  } = useAudioSettings()

  const liveStats = useLiveEngineStats(isNativeMode && engineInUse)
  const livePeers = liveStats?.peers ?? NO_PEERS
  // 슬라이더 드래그 중에는 표시만 바꾸고, 엔진 적용(IPC)은 마지막 값으로 200ms 뒤 한 번만(PR #20 M3).
  const scheduleProfileApply = useDebouncedJitterProfile()

  // 설정 화면을 닫으면 테스트 종료
  useEffect(() => () => {
    void stopTest()
  }, [stopTest])

  const wrap = (body: React.ReactNode) =>
    isModal && onClose ? (
      // 모달: 공용 Modal(포커스 트랩·Esc·포커스 복귀). 클래스는 기존 오디오 설정 CSS 그대로
      <Modal
        className="audio-settings modal"
        title="오디오 설정"
        onClose={onClose}
        classNames={{ content: 'audio-settings-content', header: 'settings-header', body: 'audio-settings-body' }}
      >
        {body}
      </Modal>
    ) : (
      <div className="audio-settings">
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
  const channelOptions = inputChannelOptions(selectedInputDevice?.channels)
  const allowed = (patch: PayloadPatch) => payloadAllowed(config, patch)
  // 이 배포본에 없는 드라이버(예: ASIO 없이 빌드)는 고를 수 없게 하고 이유를 보인다(P2-7)
  const drivers = driverChoices(supportedDrivers)
  const driverOptions = drivers.map((d) => (
    <option key={d.id} value={d.id} disabled={d.disabled}>
      {d.label}{d.disabled ? ' (이 배포본에 없음)' : ''}
    </option>
  ))
  const unsupportedReasons = drivers.flatMap((d) => (d.reason ? [d.reason] : []))
  // 기본값은 공유지만 합주에는 독점을 권한다(P1-19) — 공유일 때 눈에 띄게
  const exclusiveHint = exclusiveModeHint(config, supportedDrivers)
  const frameHint = bufferFrameHint(config, selectedInputDevice?.sampleRates)
  // 출력 장치: 고른 것, 없으면(기본 장치) 기본 표시된 것
  const selectedOutputDevice = outputDevices.find((d) => (config.outputDevice ? d.id === config.outputDevice : d.isDefault))
  // 테스트 모니터가 돌면 엔진이 실제로 보고한 품질(같은 설정으로 시작 — 설정을 바꾸면 테스트가 끝난다), 아니면 같은 식으로 예상
  const resampleHint = resampleQualityHint(
    (isTesting ? testStats?.resampler?.quality : undefined) ?? resolveResampleQuality(config),
    (config.resampleQuality ?? 'auto') === 'auto',
    selectedInputDevice?.sampleRates,
    selectedOutputDevice?.sampleRates,
    config.frameMs,
  )

  return wrap(
    <>
      {migrated && (
        <div className="settings-notice">
          이전 버전의 오디오 설정을 새 엔진 형식으로 변환했습니다. 장치 선택을 확인해주세요.
        </div>
      )}

      {/* 저장된 드라이버·장치를 이 PC에서 쓸 수 없음 → 이번 실행만 기본 장치(P2-27). 저장값은 그대로 */}
      {fallbackNotice && <div className="settings-notice" data-testid="driver-notice" role="status">{fallbackNotice}</div>}

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

      {exclusiveHint && (
        <div
          className={`exclusive-hint ${exclusiveHint.emphasized ? 'emphasized' : ''}`}
          role={exclusiveHint.emphasized ? 'note' : undefined}
          data-testid="exclusive-hint"
        >
          <p>{exclusiveHint.text}</p>
          {exclusiveHint.canSwitch && (
            <button
              className="exclusive-switch-btn"
              onClick={() => updateConfig({ inputDriver: 'wasapiExclusive', outputDriver: 'wasapiExclusive' })}
            >
              WASAPI 독점으로 바꾸기
            </button>
          )}
        </div>
      )}

      {/* 드라이버 + 장치. 장치 쪽 값은 PC마다 달라 이 PC에만 저장한다(P2-27 후속) — 고급(스레드 정책) 표기와 같은 말로 */}
      <p className="setting-hint" data-testid="device-local-note">
        드라이버·장치·입력 채널·장치 버퍼는 PC마다 달라 계정으로 다른 PC에 따라가지 않고 이 PC에만 저장됩니다(코덱·프레임·모노/스테레오·지터 버퍼는 계정에 저장). 그래서 프리셋 표시는 PC마다 다를 수 있습니다.
      </p>
      <section className="settings-section">
        <h3>입력</h3>
        <div className="settings-row">
          <label htmlFor="input-driver">드라이버</label>
          <select
            id="input-driver"
            value={config.inputDriver}
            onChange={(e) => updateConfig({ inputDriver: e.target.value as DriverKind })}
          >
            {driverOptions}
          </select>
        </div>
        <p className="setting-hint">{DRIVER_OPTIONS.find((d) => d.id === config.inputDriver)?.desc}</p>
        {unsupportedReasons.map((r) => <p key={r} className="setting-hint" data-testid="driver-unsupported">{r}</p>)}
        <div className="settings-row">
          <label htmlFor="input-device">장치</label>
          <select
            id="input-device"
            className="device-select"
            value={config.inputDevice ?? ''}
            onChange={(e) => updateConfig({ inputDevice: e.target.value || null })}
          >
            <option value="">기본 장치</option>
            {missingDevices.input && (
              <option value={missingDevices.input}>(이 PC에 없음) {deviceNameFromId(missingDevices.input)} — 기본 장치 사용</option>
            )}
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
            {driverOptions}
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
            {missingDevices.output && (
              <option value={missingDevices.output}>(이 PC에 없음) {deviceNameFromId(missingDevices.output)} — 기본 장치 사용</option>
            )}
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
        {frameHint && <p className="setting-hint" data-testid="buffer-frame-hint">{frameHint}</p>}
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
            onChange={(e) => updateConfig({ channels: parseChannels(e.target.value) })}
          >
            <option value={1}>모노</option>
            <option value={2} disabled={!allowed({ channels: 2 })}>
              스테레오{!allowed({ channels: 2 }) ? ' — 현재 코덱/프레임에서 불가' : ''}
            </option>
          </select>
        </div>
        <div className="settings-row">
          <label htmlFor="resample-quality">리샘플러 품질</label>
          <select
            id="resample-quality"
            value={config.resampleQuality ?? 'auto'}
            onChange={(e) => updateConfig({ resampleQuality: e.target.value as ResampleQuality })}
          >
            {RESAMPLE_QUALITY_OPTIONS.map((o) => (
              <option key={o.id} value={o.id}>{o.label}</option>
            ))}
          </select>
        </div>
        <p className="setting-hint" data-testid="resample-quality-hint">{resampleHint}</p>
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
              onChange={(e) => updateConfig({ inputChannels: withLeftInputChannel(config.inputChannels, Number(e.target.value)) })}
            >
              {channelOptions.map((ch) => <option key={ch} value={ch}>L: Ch{ch + 1}</option>)}
            </select>
            <select
              aria-label="오른쪽 입력 채널"
              value={config.inputChannels[1] ?? 1}
              onChange={(e) => updateConfig({ inputChannels: withRightInputChannel(config.inputChannels, Number(e.target.value)) })}
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
                <span className="stat-label">{peerLabel(ps)}</span>
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
            onChange={(e) => updateConfig({ jitterMinMs: parseJitterMs(e.target.value) })}
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
            onChange={(e) => updateConfig({ jitterMaxMs: parseJitterMs(e.target.value) })}
          />
          <span>ms</span>
        </div>
        <p className="setting-hint">네트워크가 불안정하면 최대값을 올리고, 지연을 줄이려면 내리세요.</p>
      </section>

      {/* 고급: 엔진 스레드 코어 배치 정책(P1-21 — 절전 끄기는 기본 켬, P 코어 묶기는 실험적·기본 꺼짐) */}
      <section className="settings-section" data-testid="thread-policy-section">
        <h3>고급</h3>
        <p className="setting-hint">
          Windows 전용입니다. 바꾼 뒤 연주(또는 테스트 모니터)를 다시 시작해야 적용됩니다.
          CPU 구성에 따른 설정이라 계정으로 다른 PC에 따라가지 않고 이 PC에만 저장됩니다.
        </p>
        {THREAD_POLICY_OPTIONS.map((o) => {
          const policy = config.threadPolicy ?? DEFAULT_THREAD_POLICY
          const id = `thread-policy-${o.key}`
          // 엔진이 돌고 있으면 그 엔진의 결과(테스트 모니터 또는 합주), 아니면 null
          const live = isTesting || engineInUse ? { stats: (isTesting ? testStats : liveStats) ?? null } : null
          // 돌고 있는 엔진을 시작할 때 넘긴 값 — 환경변수 BAND_THREAD_POLICY가 덮었는지 판단(#249 리뷰 Low-1)
          const startedWith = live && engineThreadPolicy ? engineThreadPolicy[o.key] : null
          const status = threadPolicyStatus(o.key, policy[o.key], IS_WINDOWS, live, startedWith)
          return (
            <div key={o.key}>
              <div className="settings-row">
                <label htmlFor={id}>
                  {o.label}
                  {o.experimental && <span className="experimental-badge">실험적</span>}
                </label>
                <input
                  id={id}
                  type="checkbox"
                  checked={policy[o.key]}
                  aria-describedby={`${id}-status ${id}-effect ${id}-caution`}
                  onChange={(e) => updateConfig({ threadPolicy: { ...policy, [o.key]: e.target.checked } })}
                />
              </div>
              <p
                id={`${id}-status`}
                className={`setting-hint thread-policy-status ${status?.tone ?? ''}`}
                data-testid={`${id}-status`}
                aria-live="polite"
              >
                {status?.text ?? ''}
              </p>
              <p id={`${id}-effect`} className="setting-hint">{o.effect}</p>
              <p id={`${id}-caution`} className="setting-hint">{o.caution}</p>
            </div>
          )
        })}
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
                <span className="stat-value">{formatTestLatency(testStats.estLatencyMs)}</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">장치</span>
                <span className="stat-value">{formatTestDevice(testStats.sampleRate, testStats.deviceBuffer)}</span>
              </div>
              <div className="stat-item">
                <span className="stat-label">리샘플러</span>
                <span className="stat-value">{formatTestResampler(testStats.resampler)}</span>
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

      <DiagnosticsExport />

      {/* 개발 실행 전용(P1-12d): 🎛️ 모달에서 바로 개발 서버를 바꾼다(/settings/audio 페이지는 자체 섹션을 그린다).
          운영 빌드에서는 import.meta.env.DEV가 false로 접혀 컴포넌트째 빠진다(PR #56 L1) */}
      {import.meta.env.DEV && isModal && <DevServerSettingsSection />}

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
