import { EchoMeasure } from '../../settings'
import type { PeerInstrument } from '../state/roomHooks'
import type { EngineStats, PeerStats } from '../../../native/engineApi'
import { pathLabel, peerPathHint } from '../../../native/relayLogic'
import {
  formatMs,
  instrumentInfo,
  lossQualityIcon,
  peerStatsTitle,
  performerName,
  remotePeerStatus,
} from '../roomDetailModel'
import './RemotePerformerItem.css'

/** 연주자 목록의 상대 연주자 행: 피어 상태 배지, 경로 안내, RTT, 음소거, 에코 측정 */
export function RemotePerformerItem({
  oderId,
  peerInfo,
  isPerformer,
  isBroadcaster,
  ps,
  engineStats,
  relayError,
  muted,
  onToggleMute,
  rowRef,
}: {
  oderId: string
  peerInfo: PeerInstrument
  isPerformer: boolean
  /** 이 연주자가 관람자 방송 담당인가 */
  isBroadcaster: boolean
  /** 엔진 피어 통계 (내가 연주 중일 때만) */
  ps: PeerStats | undefined
  engineStats: EngineStats | null
  relayError: string | null
  muted: boolean
  onToggleMute: () => void
  rowRef: (el: HTMLDivElement | null) => void
}) {
  const instInfo = instrumentInfo(peerInfo.instrument)
  // 내가 연주 중이면 엔진 피어 상태, 아니면(관람) 방송 담당 여부만 표시
  const { state, stateInfo, isActive } = remotePeerStatus(isPerformer, ps)
  // 'failed'(릴레이까지 실패) 안내 또는 릴레이 경로 지연 안내
  const pathHint = state ? peerPathHint(state, ps?.path, engineStats?.relay, relayError) : null

  return (
    <div className="performer-item-wrapper" ref={rowRef}>
      <div className={`performer-item ${isActive ? 'active' : 'connecting'}`}>
        <div className="performer-avatar">
          {peerInfo.isHost && <span className="host-crown">👑</span>}
          <div className="avatar-circle">
            <span>{instInfo.icon}</span>
          </div>
          {isActive && <span className="live-indicator" />}
        </div>
        <div className="performer-info">
          <span className="performer-name">
            {performerName(peerInfo.nickname, oderId)} {peerInfo.isHost && '(방장)'}
            {isBroadcaster && <span className="broadcast-badge" title="관람자에게 믹스를 송출하는 연주자">📡 방송</span>}
          </span>
          <span className="performer-instrument">{instInfo.name}</span>
          {stateInfo && (
            <span className={`peer-state-badge ${stateInfo.className}`} title={ps ? peerStatsTitle(ps) : undefined}>
              {stateInfo.label}
              {ps?.path && <> · {pathLabel(ps.path)}</>}
            </span>
          )}
          {pathHint && <span className={state === 'failed' || engineStats?.relay?.overBudget ? 'peer-failed-hint' : 'peer-relay-hint'}>{pathHint}</span>}
        </div>
        {/* 네트워크 상태 (연주 중일 때만: 엔진 통계) */}
        {ps && state !== 'failed' && (
          <div className="performer-latency" title={peerStatsTitle(ps)}>
            <span className="latency-value">{formatMs(ps.rttMs)}</span>
            <span className="quality-indicator">{lossQualityIcon(ps.lossPct)}</span>
          </div>
        )}
        {/* 피어별 음소거 (엔진) */}
        {isPerformer && (
          <button
            className={`performer-mute-btn ${muted ? 'muted' : ''}`}
            onClick={onToggleMute}
            title={muted ? '음소거 해제' : '음소거'}
          >
            {muted ? '🔇' : '🔊'}
          </button>
        )}
        {/* 피어 메뉴: 에코 측정(네트워크 포함 왕복, 상대 동의 필요) */}
        {isPerformer && (
          <EchoMeasure peerId={oderId} peerName={performerName(peerInfo.nickname, oderId)} stats={ps} />
        )}
      </div>
    </div>
  )
}
