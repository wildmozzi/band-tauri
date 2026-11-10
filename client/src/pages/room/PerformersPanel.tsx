import type { MutableRefObject } from 'react'
import { useRoom, type PeerInstrument } from '../../contexts/RoomContext'
import { MyPerformerItem } from './MyPerformerItem'
import { ParticipationToggle } from './ParticipationToggle'
import { RemotePerformerItem } from './RemotePerformerItem'

/** 왼쪽: 연주자 패널 (내 행, 상대 연주자, 참여 요청 알림, 참여/관람 전환) */
export function PerformersPanel({
  isPerformer,
  isHost,
  freeJoin,
  performerCount,
  remotePerformers,
  roomFull,
  performBusy,
  performerRefs,
  performersListRef,
  onOpenInstrumentChange,
  onOpenPendingRequests,
  onBecomePerformer,
  onBecomeViewer,
  onStartAfterApproval,
}: {
  isPerformer: boolean
  isHost: boolean
  freeJoin: boolean
  performerCount: number
  remotePerformers: [string, PeerInstrument][]
  roomFull: boolean
  performBusy: boolean
  performerRefs: MutableRefObject<Record<string, HTMLDivElement | null>>
  performersListRef: MutableRefObject<HTMLDivElement | null>
  onOpenInstrumentChange: () => void
  onOpenPendingRequests: () => void
  onBecomePerformer: () => void
  onBecomeViewer: () => void
  onStartAfterApproval: () => void
}) {
  const {
    myInstrument,
    canPerform,
    broadcasterId,
    peerEngineStats,
    engineStats,
    relayError,
    mixSettingsMap,
    setMixMuted,
    pendingRequests,
  } = useRoom()

  return (
    <aside className="performers-panel">
      <div className="panel-header">
        <h2>🎸 연주자</h2>
        <span className="performer-count">{performerCount}명</span>
      </div>

      <div className="performers-list" ref={performersListRef}>
        {/* 내 채널 (연주자일 경우) */}
        {isPerformer && myInstrument && (
          <MyPerformerItem myInstrument={myInstrument} isHost={isHost} onOpenInstrumentChange={onOpenInstrumentChange} />
        )}

        {/* 다른 연주자들 (peerInstruments 기반) */}
        {remotePerformers.map(([oderId, peerInfo]) => {
          const muted = mixSettingsMap[oderId]?.muted ?? false
          return (
            <RemotePerformerItem
              key={oderId}
              oderId={oderId}
              peerInfo={peerInfo}
              isPerformer={isPerformer}
              isBroadcaster={broadcasterId === oderId}
              ps={isPerformer ? peerEngineStats[oderId] : undefined}
              engineStats={engineStats}
              relayError={relayError}
              muted={muted}
              onToggleMute={() => setMixMuted(oderId, !muted)}
              rowRef={(el) => { performerRefs.current[oderId] = el }}
            />
          )
        })}

        {performerCount === 0 && (
          <div className="no-performers">
            <p>아직 연주자가 없습니다</p>
            <small>{canPerform ? '첫 번째 연주자가 되어보세요!' : '연주가 시작되면 여기에 표시됩니다'}</small>
          </div>
        )}
      </div>

      {/* 방장: 승인 요청 알림 (참여 버튼 위에 표시) */}
      {isHost && pendingRequests.length > 0 && (
        <div className="pending-requests-alert" onClick={onOpenPendingRequests}>
          <span className="alert-badge">{pendingRequests.length}</span>
          <span>연주 참여 요청</span>
        </div>
      )}

      {/* 참여/관람 전환 버튼 */}
      <ParticipationToggle
        isPerformer={isPerformer}
        isHost={isHost}
        freeJoin={freeJoin}
        roomFull={roomFull}
        performBusy={performBusy}
        onBecomePerformer={onBecomePerformer}
        onBecomeViewer={onBecomeViewer}
        onStartAfterApproval={onStartAfterApproval}
      />
    </aside>
  )
}
