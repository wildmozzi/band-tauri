import { useState } from 'react'
import { EchoMeasure } from '../../settings'
import type { PeerInstrument } from '../state/roomHooks'
import type { EngineStats, PeerStats } from '../../../native/engineApi'
import { pathLabel, peerPathHint } from '../../../native/relayLogic'
import { earLatencyLevel, earLatencyTitleLines, estimateEarLatencyMs, formatEarLatency } from '../earLatency'
import {
  instrumentInfo,
  lossQualityIcon,
  peerStatsTitle,
  performerName,
  remotePeerStatus,
} from '../roomDetailModel'
import './RemotePerformerItem.css'

/** 연주자 목록의 상대 연주자 행: 피어 상태 배지, 경로 안내, 입→귀 추정 지연(RTT는 툴팁), 음소거, 에코 측정, (방장) 관람자로 내리기 */
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
  onDemote,
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
  /** 방장 화면에서만: 이 연주자를 관람자로 내린다(P2-9). 확인 단계를 거친 뒤 부른다 */
  onDemote?: () => void
  rowRef: (el: HTMLDivElement | null) => void
}) {
  const instInfo = instrumentInfo(peerInfo.instrument)
  // 내가 연주 중이면 엔진 피어 상태, 아니면(관람) 방송 담당 여부만 표시
  const { state, stateInfo, isActive } = remotePeerStatus(isPerformer, ps)
  // 'failed'(릴레이까지 실패) 안내 또는 릴레이 경로 지연 안내
  const pathHint = state ? peerPathHint(state, ps?.path, engineStats?.relay, relayError) : null
  // 상대 입력 → 내 귀 추정(P2-8). 엔진 통계만으로 계산. 상대 입력 버퍼·프레임(P2-8b)이 다 있으면 "≈" 없이, 하나라도 없거나 하한(포화·에코 측정 중)이면 ≈
  const ear = ps ? estimateEarLatencyMs(ps, engineStats) : null
  const [confirmDemote, setConfirmDemote] = useState(false)
  const name = performerName(peerInfo.nickname, oderId)

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
        {ps && ear && state !== 'failed' && (
          <div className="performer-latency" title={[...earLatencyTitleLines(ear), peerStatsTitle(ps)].join('\n')}>
            <span className={`latency-value ear-latency ${earLatencyLevel(ear.totalMs) ?? 'unknown'}`} data-testid="ear-latency">
              {formatEarLatency(ear)}
            </span>
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
          <EchoMeasure peerId={oderId} peerName={name} stats={ps} />
        )}
        {/* 방장: 관람자로 내리기 (P2-9). 실수로 누르지 않게 같은 행에서 한 번 더 확인 */}
        {onDemote && !confirmDemote && (
          <button className="performer-demote-btn" onClick={() => setConfirmDemote(true)} title={`${name}님을 관람자로 내리기`}>
            관람자로 내리기
          </button>
        )}
      </div>
      {onDemote && confirmDemote && (
        <div className="performer-demote-confirm" role="alertdialog" aria-label="관람자로 내리기 확인">
          <span>{name}님의 연주를 멈추고 관람자로 내릴까요?</span>
          <button
            className="performer-demote-yes"
            onClick={() => {
              setConfirmDemote(false)
              onDemote()
            }}
          >
            내리기
          </button>
          <button className="performer-demote-no" onClick={() => setConfirmDemote(false)}>
            취소
          </button>
        </div>
      )}
    </div>
  )
}
