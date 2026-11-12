import type { MixSettings, PeerInstrument, PeerNetworkStats } from '../state/RoomContext'
import type { EngineStats, PeerState, PeerStats } from '../../../native/engineApi'
import { pathLabel, peerPathHint } from '../../../native/relayLogic'
import { EngineDiagLines } from './EngineDiagLines'
import {
  INSTRUMENT_INFO,
  PEER_STATE_INFO,
  QUALITY_ICONS,
  cpuTitle,
  formatCpu,
  formatMs,
  formatPan,
  instrumentInfo,
  levelClass,
  peerStatsTitle,
  performerName,
} from '../roomDetailModel'

function ChannelLevel({ level }: { level: number }) {
  return (
    <div className="channel-level">
      <div className="level-meter">
        <div className="level-bar">
          <div
            className={`level-fill ${levelClass(level)}`}
            style={{ width: `${level}%` }}
          />
        </div>
      </div>
    </div>
  )
}

function MuteButton({ muted, onToggle }: { muted: boolean; onToggle: () => void }) {
  return (
    <button
      className={`mute-btn ${muted ? 'active' : ''}`}
      onClick={onToggle}
      title={muted ? '음소거 해제' : '음소거'}
    >
      {muted ? '🔇' : '🔊'}
    </button>
  )
}

/** 채널 볼륨·패닝 슬라이더 (음소거면 비활성) */
function ChannelControls({
  mix,
  onVolume,
  onPan,
}: {
  mix: MixSettings
  onVolume: (volume: number) => void
  onPan: (pan: number) => void
}) {
  return (
    <div className="channel-controls">
      <div className="control-row">
        <label>볼륨</label>
        <input
          type="range"
          min="0"
          max="1"
          step="0.01"
          value={mix.volume}
          onChange={(e) => onVolume(parseFloat(e.target.value))}
          disabled={mix.muted}
        />
        <span>{Math.round(mix.volume * 100)}%</span>
      </div>
      <div className="control-row">
        <label>패닝</label>
        <input
          type="range"
          min="-1"
          max="1"
          step="0.01"
          value={mix.pan}
          onChange={(e) => onPan(parseFloat(e.target.value))}
          disabled={mix.muted}
        />
        <span>{formatPan(mix.pan)}</span>
      </div>
    </div>
  )
}

/** 내 채널 (모니터): 엔진 샘플레이트·버퍼·처리 지연 */
export function MyMixerChannel({
  myInstrument,
  engineStats,
  level,
  rtcError,
}: {
  myInstrument: string
  engineStats: EngineStats | null
  level: number
  rtcError: string | null
}) {
  return (
    <div className="mixer-channel me">
      <div className="channel-header">
        <span className="channel-icon">{INSTRUMENT_INFO[myInstrument]?.icon || '🎵'}</span>
        <span className="channel-name">나 (모니터)</span>
      </div>
      <div className="channel-info">
        <span className="audio-info">
          {engineStats ? `${engineStats.sampleRate / 1000}kHz · 버퍼 ${engineStats.deviceBuffer}` : '엔진 상태 확인 중'}
        </span>
        {/* 백엔드·스레드 우선순위·QoS(P1-12b): 실장비 테스트에서 개발자 도구 없이 읽는다 */}
        {engineStats && <EngineDiagLines stats={engineStats} />}
        <span className="audio-info">처리 지연 {formatMs(engineStats?.estLatencyMs, 1)} · xrun {engineStats?.xruns ?? 0}</span>
        {engineStats?.cpu && formatCpu(engineStats.cpu) && (
          <span className="audio-info" title={cpuTitle(engineStats.cpu)}>{formatCpu(engineStats.cpu)}</span>
        )}
      </div>
      <div className="channel-level">
        <div className="level-meter">
          <div className="level-bar">
            <div className="level-fill" style={{ width: `${level}%` }} />
          </div>
        </div>
      </div>
      {rtcError && <div className="channel-error">{rtcError}</div>}
    </div>
  )
}

/** 상대 연주자 채널 스트립 (엔진 피어별 gain/pan/mute + 피어 상태) */
export function PeerMixerChannel({
  oderId,
  peerInfo,
  mix,
  ps,
  engineStats,
  relayError,
  level,
  onMute,
  onVolume,
  onPan,
}: {
  oderId: string
  peerInfo: PeerInstrument
  mix: MixSettings
  ps: PeerStats | undefined
  engineStats: EngineStats | null
  relayError: string | null
  level: number
  onMute: (muted: boolean) => void
  onVolume: (volume: number) => void
  onPan: (pan: number) => void
}) {
  const instInfo = instrumentInfo(peerInfo.instrument)
  const state: PeerState = ps?.state ?? 'connecting'
  return (
    <div className={`mixer-channel ${mix.muted ? 'muted' : ''}`}>
      <div className="channel-header">
        <span className="channel-icon">{instInfo.icon}</span>
        <span className="channel-name">{performerName(peerInfo.nickname, oderId)}</span>
        <MuteButton muted={mix.muted} onToggle={() => onMute(!mix.muted)} />
      </div>
      <div className="channel-latency">
        <span className={`peer-state-badge ${PEER_STATE_INFO[state].className}`}>{PEER_STATE_INFO[state].label}</span>
        {state === 'failed' ? (
          <span className="peer-failed-hint">{peerPathHint(state, ps?.path, engineStats?.relay, relayError)}</span>
        ) : ps ? (
          <span className="latency-text engine-peer-stats" title={peerStatsTitle(ps)}>
            RTT {formatMs(ps.rttMs)} · 지터 {formatMs(ps.jitterMs, 1)} · 버퍼 {formatMs(ps.bufferMs)} · 손실 {ps.lossPct.toFixed(1)}%
            {' · '}{pathLabel(ps.path) ?? '경로 탐색 중'}
            {ps.path === 'relay' && (
              <span className={engineStats?.relay?.overBudget ? 'peer-failed-hint' : 'peer-relay-hint'}> ({peerPathHint(state, ps.path, engineStats?.relay)})</span>
            )}
          </span>
        ) : (
          <span className="latency-text">연결 정보 교환 중</span>
        )}
      </div>
      <ChannelLevel level={level} />
      <ChannelControls mix={mix} onVolume={onVolume} onPan={onPan} />
    </div>
  )
}

/** 관람: 방송 담당 연주자의 합주 믹스 채널 */
export function BroadcastMixerChannel({
  broadcasterName,
  mix,
  netStats,
  level,
  onMute,
  onVolume,
  onPan,
}: {
  broadcasterName: string | null
  mix: MixSettings
  netStats: PeerNetworkStats | undefined
  level: number
  onMute: (muted: boolean) => void
  onVolume: (volume: number) => void
  onPan: (pan: number) => void
}) {
  const qualityInfo = QUALITY_ICONS[netStats?.quality || 'unknown']
  return (
    <div className={`mixer-channel ${mix.muted ? 'muted' : ''}`}>
      <div className="channel-header">
        <span className="channel-icon">📡</span>
        <span className="channel-name">합주 믹스 {broadcasterName && `(${broadcasterName})`}</span>
        <MuteButton muted={mix.muted} onToggle={() => onMute(!mix.muted)} />
      </div>
      <div className="channel-latency">
        <span className="quality-dot" style={{ background: qualityInfo.color }}></span>
        <span className="latency-text">
          {netStats?.audioLatency != null ? `${netStats.audioLatency}ms` : '측정 중'}
          {netStats?.jitter != null && <small> (지터: {netStats.jitter}ms)</small>}
        </span>
      </div>
      <ChannelLevel level={level} />
      <ChannelControls mix={mix} onVolume={onVolume} onPan={onPan} />
    </div>
  )
}
