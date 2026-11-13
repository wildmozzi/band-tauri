import { useRoomTelemetry, useRoomMixer, useRoomRecording, useRoomConnection, useRoomPerform, type PeerInstrument } from '../state/roomHooks'
import { BroadcastMixerChannel, MyMixerChannel, PeerMixerChannel } from './MixerChannels'
import { DEFAULT_MIX, formatDuration, formatPan, levelClass, performerName } from '../roomDetailModel'
import { DiagnosticsExport } from '../../settings'
import { NativeRecordingControl } from './NativeRecordingControl'
import { isNativeApp } from '../../../native/engineApi'
import './MixerPanel.css'

/** 마스터 볼륨·음소거·패닝(관람만)·레벨 */
function MixerMaster({ isPerformer }: { isPerformer: boolean }) {
  const { masterLevel } = useRoomTelemetry('masterLevel')
  const { masterVolume, setMasterVolume, masterMuted, toggleMasterMute, masterPan, setMasterPan } = useRoomMixer()
  return (
    <div className={`mixer-master ${masterMuted ? 'muted' : ''}`}>
      <div className="master-header">
        <label>마스터</label>
        <button
          className={`master-mute-btn ${masterMuted ? 'active' : ''}`}
          onClick={toggleMasterMute}
          title={masterMuted ? '음소거 해제' : '음소거'}
        >
          {masterMuted ? '🔇' : '🔊'}
        </button>
      </div>
      <div className="master-control-row">
        <label>볼륨</label>
        <input
          type="range"
          aria-label="마스터 볼륨"
          min="0"
          max="1"
          step="0.01"
          value={masterVolume}
          onChange={(e) => setMasterVolume(parseFloat(e.target.value))}
        />
        <span>{masterMuted ? 'MUTE' : `${Math.round(masterVolume * 100)}%`}</span>
      </div>
      {/* 마스터 패닝은 웹 재생(관람)에서만. 엔진은 피어별 pan만 있다 */}
      {!isPerformer && (
        <div className="master-control-row">
          <label>패닝</label>
          <input
            type="range"
            aria-label="마스터 패닝"
            min="-1"
            max="1"
            step="0.01"
            value={masterPan}
            onChange={(e) => setMasterPan(parseFloat(e.target.value))}
          />
          <span>{formatPan(masterPan)}</span>
        </div>
      )}
      <div className="level-meter master-level">
        <div className="level-bar">
          <div
            className={`level-fill ${levelClass(masterLevel)}`}
            style={{ width: `${masterLevel}%` }}
          />
        </div>
      </div>
    </div>
  )
}

/** 녹음 시작/중지, 녹음 시간, 녹음 목록 버튼 */
function RecordingControl({ onOpenRecordings }: { onOpenRecordings: () => void }) {
  const { isRecording, recordings, recordingDuration, startRecording, stopRecording } = useRoomRecording()
  return (
    <div className="recording-section">
      <div className="recording-control">
        <button
          onClick={isRecording ? stopRecording : startRecording}
          className={`recording-btn ${isRecording ? 'recording' : ''}`}
          title={isRecording ? '녹음 중지' : '녹음 시작'}
        >
          {isRecording ? '⏹️' : '⏺️'}
          <span>{isRecording ? '녹음 중지' : '녹음'}</span>
        </button>
        {isRecording && (
          <span className="recording-time">
            🔴 {formatDuration(recordingDuration)}
          </span>
        )}
        {recordings.length > 0 && (
          <button
            onClick={onOpenRecordings}
            className="recordings-list-btn"
          >
            📁 녹음 목록 ({recordings.length})
          </button>
        )}
      </div>
    </div>
  )
}

/** 가운데: 개인 믹서 (연주 중이면 엔진 피어 채널, 관람 중이면 방송 믹스 채널) */
export function MixerPanel({
  isPerformer,
  remotePerformers,
  hasAnyPerformer,
  broadcasterName,
  onOpenRecordings,
  roomTitle,
}: {
  isPerformer: boolean
  remotePerformers: [string, PeerInstrument][]
  hasAnyPerformer: boolean
  broadcasterName: string | null
  onOpenRecordings: () => void
  /** 녹음 세션 폴더 이름에 붙인다 */
  roomTitle?: string | null
}) {
  const { clientId } = useRoomConnection()
  const { canPerform, myInstrument, relayError } = useRoomPerform()
  const { engineStats, peerEngineStats, peerNetworkStats, audioLevels, firstAudio } = useRoomTelemetry('engineStats', 'peerEngineStats', 'peerNetworkStats', 'audioLevels', 'firstAudio')
  const { rtcError, remoteAudioMap, mixSettingsMap, setMixVolume, setMixPan, setMixMuted } = useRoomMixer()
  // 관람자가 듣는 방송 믹스 스트림 (방송 담당자 하나)
  const broadcastStreams = Object.keys(remoteAudioMap)

  return (
    <main className="mixer-panel">
      <div className="panel-header">
        <h2>🎚️ 개인 믹서</h2>
        <span className="mixer-hint">내가 듣는 소리 조절</span>
        {/* 끊김·지연 진단 자료(P2-5): 데스크톱 앱에서만 */}
        <DiagnosticsExport compact />
      </div>

      <div className="mixer-content">
        {/* 마스터 컨트롤 */}
        <MixerMaster isPerformer={isPerformer} />

        <div className="mixer-channels">
          {isPerformer ? (
            <>
              {/* 내 채널 */}
              {myInstrument && (
                <MyMixerChannel
                  myInstrument={myInstrument}
                  engineStats={engineStats}
                  firstAudio={firstAudio}
                  level={audioLevels[clientId ?? ''] ?? 0}
                  rtcError={rtcError}
                />
              )}

              {/* 다른 연주자 채널 스트립 (엔진 피어별 gain/mute) */}
              {remotePerformers.map(([oderId, peerInfo]) => (
                <PeerMixerChannel
                  key={oderId}
                  oderId={oderId}
                  peerInfo={peerInfo}
                  mix={mixSettingsMap[oderId] || DEFAULT_MIX}
                  ps={peerEngineStats[oderId]}
                  engineStats={engineStats}
                  relayError={relayError}
                  level={audioLevels[oderId] || 0}
                  onMute={(muted) => setMixMuted(oderId, muted)}
                  onVolume={(v) => setMixVolume(oderId, v)}
                  onPan={(p) => setMixPan(oderId, p)}
                />
              ))}

              {remotePerformers.length === 0 && (
                <div className="mixer-empty">
                  <p>다른 연주자가 없습니다</p>
                  <small>연주자가 참여하면 여기서 볼륨을 조절할 수 있습니다</small>
                </div>
              )}
            </>
          ) : (
            <>
              {/* 관람: 방송 담당 연주자의 합주 믹스 */}
              {broadcastStreams.map((senderId) => (
                <BroadcastMixerChannel
                  key={senderId}
                  broadcasterName={broadcasterName}
                  mix={mixSettingsMap[senderId] || DEFAULT_MIX}
                  netStats={peerNetworkStats[senderId]}
                  level={audioLevels[senderId] || 0}
                  onMute={(muted) => setMixMuted(senderId, muted)}
                  onVolume={(v) => setMixVolume(senderId, v)}
                  onPan={(p) => setMixPan(senderId, p)}
                />
              ))}

              {broadcastStreams.length === 0 && (
                <div className="mixer-empty">
                  {hasAnyPerformer ? (
                    <>
                      <p>합주 방송에 연결 중...</p>
                      <small>{rtcError ?? '방송 담당 연주자의 믹스를 기다리는 중입니다'}</small>
                    </>
                  ) : (
                    <>
                      <p>연주자가 없습니다</p>
                      <small>연주가 시작되면 여기서 볼륨을 조절할 수 있습니다</small>
                    </>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {/* 녹음: 앱 연주자는 엔진 멀티트랙(P2-4), 관람자는 방송 믹스(MediaRecorder) */}
      {isPerformer && isNativeApp() ? (
        <NativeRecordingControl
          peers={remotePerformers.map(([oderId, p]) => ({ peerId: oderId, name: performerName(p.nickname, oderId) }))}
          roomTitle={roomTitle}
        />
      ) : (
        <RecordingControl onOpenRecordings={onOpenRecordings} />
      )}

      {/* 관람자 안내 */}
      {!isPerformer && (
        <div className="viewer-guide">
          <p>👀 관람 모드</p>
          <small>
            {canPerform
              ? '연주에 참여하려면 왼쪽에서 "연주 참여하기"를 클릭하세요'
              : '연주는 데스크톱 앱에서만 할 수 있습니다. 웹에서는 합주를 들을 수 있어요.'}
          </small>
        </div>
      )}
    </main>
  )
}
