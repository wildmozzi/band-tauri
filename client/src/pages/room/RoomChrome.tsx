// 합주실 화면의 머리글, 비밀번호 입장 화면, 악기 변경 요청 말풍선.
import { Link } from 'react-router-dom'
import type { InstrumentChangeRequest, JoinError, RtcStatus } from '../../contexts/RoomContext'
import { describeJoinError } from '../../contexts/room/roomAccess'
import { INSTRUMENT_INFO, RTC_STATUS_TEXT, type DetailRoom } from './roomDetailModel'
import type { BubblePositions } from './useBubblePositions'
import { Badge } from '../../components/ui/Badge'
import { Button } from '../../components/ui/Button'

/** 상단 헤더: 제목, 상태 배지, 인원, 방장, 정보/설정/나가기 */
export function LiveHeader({
  room,
  displayStatus,
  participantCount,
  hostNickname,
  isHost,
  onOpenInfo,
  onOpenSettings,
  onLeave,
}: {
  room: DetailRoom
  displayStatus: RtcStatus
  participantCount: number
  hostNickname: string | null
  isHost: boolean
  onOpenInfo: () => void
  onOpenSettings: () => void
  onLeave: () => void
}) {
  return (
    <header className="live-header">
      <div className="live-header-left">
        <Link to="/rooms" className="back-link">←</Link>
        <div className="live-title-area">
          <h1>{room.title}</h1>
          <div className="live-meta">
            <Badge kind="live-badge" tone={displayStatus}>
              {displayStatus === 'live' ? '● LIVE' : RTC_STATUS_TEXT[displayStatus]}
            </Badge>
            <Badge kind="genre-badge">{room.genre || '기타'}</Badge>
            <span className="viewer-count">👁 {participantCount}명</span>
            <span className="host-info">👑 {hostNickname || '...'}</span>
          </div>
        </div>
      </div>
      <div className="live-header-right">
        <button onClick={onOpenInfo} className="header-btn">
          ℹ️ 정보
        </button>
        {isHost && (
          <button onClick={onOpenSettings} className="header-btn">
            ⚙️ 방 설정
          </button>
        )}
        <button onClick={onLeave} className="header-btn leave">
          나가기
        </button>
      </div>
    </header>
  )
}

/** 비밀번호 방 입장 (방장은 서버가 묻지 않는다). 정원 초과(room-full)는 안내와 다시 시도·목록 버튼만 */
export function RoomPasswordGate({
  title,
  joinError,
  passwordInput,
  onPasswordInput,
  onSubmit,
  onRetry,
  onLeave,
}: {
  title: string
  joinError: JoinError
  passwordInput: string
  onPasswordInput: (v: string) => void
  onSubmit: (e: React.FormEvent) => void
  onRetry: () => void
  onLeave: () => void
}) {
  const locked = joinError.code === 'password-rate-limited'
  if (joinError.code === 'room-full') {
    return (
      <div className="room-password-gate">
        <div className="room-password-card">
          <h2>{title}</h2>
          <p className="room-password-message error" role="alert">{describeJoinError(joinError)}</p>
          <div className="room-password-actions">
            <button type="button" className="btn-secondary" onClick={onLeave}>목록으로</button>
            <button type="button" className="btn-primary" onClick={onRetry}>다시 시도</button>
          </div>
        </div>
      </div>
    )
  }
  return (
    <div className="room-password-gate">
      <form className="room-password-card" onSubmit={onSubmit}>
        <h2>🔒 {title}</h2>
        <p className={`room-password-message ${joinError.code === 'password-required' ? '' : 'error'}`}>
          {describeJoinError(joinError)}
        </p>
        <input
          type="password"
          autoFocus
          autoComplete="off"
          placeholder="합주실 비밀번호"
          value={passwordInput}
          onChange={(e) => onPasswordInput(e.target.value)}
          maxLength={64}
          disabled={locked}
        />
        <div className="room-password-actions">
          <Button variant="secondary" onClick={onLeave}>목록으로</Button>
          <Button type="submit" disabled={locked || !passwordInput}>입장</Button>
        </div>
      </form>
    </div>
  )
}

/** 방장: 악기 변경 요청 말풍선들 (fixed position으로 믹서 위에 표시) */
export function InstrumentChangeBubbles({
  requests,
  positions,
  onApprove,
  onReject,
}: {
  requests: InstrumentChangeRequest[]
  positions: BubblePositions
  onApprove: (oderId: string) => void
  onReject: (oderId: string) => void
}) {
  return (
    <>
      {requests.map(request => {
        const pos = positions[request.oderId]
        if (!pos) return null

        return (
          <div
            key={request.oderId}
            className="instrument-change-bubble fixed"
            style={{
              top: pos.top,
              left: pos.left
            }}
          >
            <div className="bubble-arrow" />
            <div className="bubble-content">
              <span className="bubble-text">
                🔄 {INSTRUMENT_INFO[request.newInstrument]?.icon} {INSTRUMENT_INFO[request.newInstrument]?.name || request.newInstrument}
              </span>
              <div className="bubble-actions">
                <button
                  onClick={() => onApprove(request.oderId)}
                  className="bubble-btn approve"
                  title="승인"
                >
                  ✓
                </button>
                <button
                  onClick={() => onReject(request.oderId)}
                  className="bubble-btn reject"
                  title="거절"
                >
                  ✕
                </button>
              </div>
            </div>
          </div>
        )
      })}
    </>
  )
}
