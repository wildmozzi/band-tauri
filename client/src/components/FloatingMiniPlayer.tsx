import { useNavigate } from 'react-router-dom'
import { useRoom } from '../contexts/RoomContext'
import './FloatingMiniPlayer.css'

// 악기 정보
const INSTRUMENT_INFO: Record<string, { icon: string; name: string }> = {
  vocal: { icon: '🎤', name: '보컬' },
  guitar: { icon: '🎸', name: '기타' },
  bass: { icon: '🎸', name: '베이스' },
  keyboard: { icon: '🎹', name: '건반' },
  drums: { icon: '🥁', name: '드럼' },
  other: { icon: '🎵', name: '기타 악기' },
}

export function FloatingMiniPlayer() {
  const navigate = useNavigate()
  const {
    isMiniPlayerMode,
    currentRoomId,
    returnToRoom,
    exitMiniPlayerMode,
    peers,
    peerInstruments,
    clientId,
    myInstrument,
    rtcStatus,
    masterMuted,
    toggleMasterMute,
    localMuted,
    toggleLocalMute,
    localStream,
  } = useRoom()

  // 미니 플레이어 모드가 아니면 렌더링하지 않음
  if (!isMiniPlayerMode || !currentRoomId) {
    return null
  }

  // 연주자 수 계산
  const remotePerformers = Object.entries(peerInstruments).filter(([peerId]) => peerId !== clientId)
  const performerCount = (myInstrument ? 1 : 0) + remotePerformers.length

  // 방으로 돌아가기
  const handleReturnToRoom = () => {
    const roomId = returnToRoom()
    if (roomId) {
      navigate(`/rooms/${roomId}`)
    }
  }

  // 방 나가기 (완전 종료)
  const handleExitRoom = (e: React.MouseEvent) => {
    e.stopPropagation()
    exitMiniPlayerMode()
  }

  return (
    <div className="floating-mini-player" onClick={handleReturnToRoom}>
      {/* 상태 표시 */}
      <div className="mini-player-status">
        <span className={`status-indicator ${rtcStatus}`}>
          {rtcStatus === 'live' ? '● LIVE' : rtcStatus === 'connecting' ? '◌ 연결 중' : '○ 대기'}
        </span>
      </div>

      {/* 연주자 정보 */}
      <div className="mini-player-info">
        <div className="performers-preview">
          {/* 내 악기 */}
          {myInstrument && (
            <span className="performer-icon" title={`나 - ${INSTRUMENT_INFO[myInstrument]?.name || myInstrument}`}>
              {INSTRUMENT_INFO[myInstrument]?.icon || '🎵'}
            </span>
          )}
          {/* 다른 연주자들 (최대 3명까지 표시) */}
          {remotePerformers.slice(0, 3).map(([peerId, info]) => (
            <span
              key={peerId}
              className="performer-icon"
              title={`${info.nickname} - ${INSTRUMENT_INFO[info.instrument]?.name || info.instrument}`}
            >
              {INSTRUMENT_INFO[info.instrument]?.icon || '🎵'}
            </span>
          ))}
          {remotePerformers.length > 3 && (
            <span className="more-performers">+{remotePerformers.length - 3}</span>
          )}
        </div>
        <div className="room-stats">
          <span className="stat-item">🎸 {performerCount}</span>
          <span className="stat-item">👥 {peers.length + 1}</span>
        </div>
      </div>

      {/* 컨트롤 버튼들 */}
      <div className="mini-player-controls">
        {/* 마이크 음소거 토글 (연주자인 경우만 표시) */}
        {localStream && (
          <button
            className={`control-btn mic ${localMuted ? 'muted' : ''}`}
            onClick={(e) => {
              e.stopPropagation()
              toggleLocalMute()
            }}
            title={localMuted ? '마이크 켜기' : '마이크 끄기'}
          >
            {localMuted ? '🎤' : '🎤'}
          </button>
        )}

        {/* 스피커 음소거 토글 */}
        <button
          className={`control-btn mute ${masterMuted ? 'muted' : ''}`}
          onClick={(e) => {
            e.stopPropagation()
            toggleMasterMute()
          }}
          title={masterMuted ? '음소거 해제' : '음소거'}
        >
          {masterMuted ? '🔇' : '🔊'}
        </button>

        {/* 나가기 버튼 */}
        <button
          className="control-btn exit"
          onClick={handleExitRoom}
          title="합주실 나가기"
        >
          ✕
        </button>
      </div>

      {/* 클릭 힌트 */}
      <div className="mini-player-hint">
        클릭하여 돌아가기
      </div>
    </div>
  )
}
