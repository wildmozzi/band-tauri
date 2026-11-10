import { useRoom } from '../../contexts/RoomContext'
import { MAX_PERFORMERS } from '../../native/engineApi'
import { DESKTOP_DOWNLOAD_URL, INSTRUMENT_INFO } from './roomDetailModel'

/** 참여/관람 전환 버튼과 연주 요청 상태 (웹은 데스크톱 앱 안내) */
export function ParticipationToggle({
  isPerformer,
  isHost,
  freeJoin,
  roomFull,
  performBusy,
  onBecomePerformer,
  onBecomeViewer,
  onStartAfterApproval,
}: {
  isPerformer: boolean
  isHost: boolean
  freeJoin: boolean
  roomFull: boolean
  performBusy: boolean
  onBecomePerformer: () => void
  onBecomeViewer: () => void
  onStartAfterApproval: () => void
}) {
  const {
    canPerform,
    performStarting,
    rtcError,
    myRequestStatus,
    myRequestInstrument,
    myRejectReason,
    hostOnline,
    cancelRequest,
    dismissRejection,
  } = useRoom()

  return (
    <div className="participation-toggle">
      {isPerformer ? (
        <button onClick={onBecomeViewer} className="toggle-btn viewer">
          👀 관람자로 전환
        </button>
      ) : !canPerform ? (
        /* 웹: 연주는 데스크톱 앱 전용 */
        <div className="web-notice">
          <p>🖥️ 연주는 데스크톱 앱에서</p>
          <small>웹에서는 관람만 할 수 있습니다. 연주에 참여하려면 Band 데스크톱 앱을 설치해주세요.</small>
          {DESKTOP_DOWNLOAD_URL ? (
            <a href={DESKTOP_DOWNLOAD_URL} className="download-link" target="_blank" rel="noopener noreferrer">📥 데스크톱 앱 다운로드</a>
          ) : (
            <span className="download-link disabled">📥 다운로드 준비 중</span>
          )}
        </div>
      ) : performBusy || performStarting ? (
        <div className="request-status pending">
          <p>⏳ 오디오 엔진 시작 중...</p>
        </div>
      ) : myRequestStatus === 'pending' ? (
        /* 승인 대기 중 */
        <div className="request-status pending">
          <p>⏳ 방장 승인 대기 중...</p>
          <small>{INSTRUMENT_INFO[myRequestInstrument || '']?.name || myRequestInstrument} 연주 요청</small>
          <small className="request-hint">
            {hostOnline === false
              ? '방장이 지금 접속해 있지 않습니다. 방장이 들어오면 요청이 전달됩니다.'
              : '승인되면 오디오 엔진이 자동으로 시작됩니다.'}
          </small>
          <button onClick={cancelRequest} className="cancel-request-btn">
            요청 취소
          </button>
        </div>
      ) : myRequestStatus === 'approved' ? (
        /* 승인됨 - 연주 시작 가능 */
        <div className="request-status approved">
          <p>✅ 승인되었습니다!</p>
          {rtcError && <small>엔진을 자동으로 시작하지 못했습니다. 설정을 확인한 뒤 다시 시도하세요.</small>}
          <button onClick={onStartAfterApproval} className="toggle-btn performer">
            🎤 연주 시작하기
          </button>
        </div>
      ) : myRequestStatus === 'rejected' ? (
        /* 거절됨 */
        <div className="request-status rejected">
          <p>❌ 요청이 거절되었습니다</p>
          {myRejectReason && <small className="reject-reason">사유: {myRejectReason}</small>}
          <button onClick={dismissRejection} className="toggle-btn performer">
            🎤 다시 요청하기
          </button>
        </div>
      ) : (
        /* 기본 상태 (데스크톱 앱) */
        <button
          onClick={onBecomePerformer}
          className="toggle-btn performer"
          disabled={roomFull}
          title={roomFull ? `연주자는 최대 ${MAX_PERFORMERS}명입니다` : undefined}
        >
          🎤 {roomFull ? `연주자 가득 참 (${MAX_PERFORMERS}명)` : '연주 참여하기'}
        </button>
      )}
      {canPerform && !isPerformer && rtcError && (
        <p className="perform-error">{rtcError}</p>
      )}
      {canPerform && !freeJoin && !isHost && !isPerformer && myRequestStatus === 'none' && (
        <p className="approval-notice">이 방은 방장 승인이 필요합니다</p>
      )}
    </div>
  )
}
