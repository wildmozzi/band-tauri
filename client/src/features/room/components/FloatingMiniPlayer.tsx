import { useNavigate } from 'react-router-dom'
import { useRoomConnection, useRoomPerform, useRoomMixer, useMiniPlayer, useReconnectBanner, type ReconnectBannerKind } from '../state/roomHooks'
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

type ShownSignalKind = Exclude<ReconnectBannerKind, 'none'>
/** 상태 점 문구·툴팁. 미니 플레이어를 누르면 합주실로 돌아갈 뿐이고, 다시 입장은 합주실 상단 띠에서 한다 */
const MINI_SIGNAL_LABEL: Record<ShownSignalKind, string> = {
  reconnecting: '재연결 중',
  'gave-up': '연결 끊김',
  replaced: '다른 곳에서 입장',
}
const MINI_SIGNAL_TITLE: Record<ShownSignalKind, string> = {
  reconnecting: '서버에 다시 연결하는 중 — 눌러서 합주실로 돌아가기',
  'gave-up': '서버 연결이 끊겼습니다 — 눌러서 합주실로 돌아가기',
  replaced: '다른 창이나 기기에서 이 합주실에 들어왔습니다 — 눌러서 합주실로 돌아가기',
}

export function FloatingMiniPlayer() {
  const navigate = useNavigate()
  const { currentRoomId, peers, clientId } = useRoomConnection()
  const { peerInstruments, myInstrument, isPerforming, isMonitoringEnabled, monitorGain, setMonitor } = useRoomPerform()
  const { rtcStatus, masterMuted, toggleMasterMute } = useRoomMixer()
  const { isMiniPlayerMode, returnToRoom, exitMiniPlayerMode } = useMiniPlayer()
  // 시그널링 재연결 중·포기·밀려남 (P1-15b). 클릭하면 합주실로 돌아가 띠에서 자세한 안내와 "다시 입장"을 본다
  const { kind: signalBanner } = useReconnectBanner()

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
        {signalBanner !== 'none' && (
          <span
            className={`mini-player-signal ${signalBanner}`}
            role={signalBanner === 'reconnecting' ? 'status' : 'alert'}
            title={MINI_SIGNAL_TITLE[signalBanner]}
          >
            <span className="mini-player-signal-dot" aria-hidden="true" />
            {MINI_SIGNAL_LABEL[signalBanner]}
          </span>
        )}
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
        {/* 모니터링(내 소리 듣기) 토글 (앱 연주자인 경우만 표시) */}
        {isPerforming && (
          <button
            className={`control-btn mic ${isMonitoringEnabled ? '' : 'muted'}`}
            onClick={(e) => {
              e.stopPropagation()
              setMonitor(!isMonitoringEnabled, monitorGain)
            }}
            title={isMonitoringEnabled ? '모니터링 끄기' : '모니터링 켜기'}
          >
            {isMonitoringEnabled ? '🎧' : '🔕'}
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
