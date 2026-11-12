import { useRoom } from '../state/RoomContext'
import { INSTRUMENT_INFO, formatMs } from '../roomDetailModel'

/** 연주자 목록의 내 행: 처리 지연·xrun·입력 레벨, 악기 변경 요청, 입력 뮤트, 모니터링 */
export function MyPerformerItem({
  myInstrument,
  isHost,
  onOpenInstrumentChange,
}: {
  myInstrument: string
  isHost: boolean
  onOpenInstrumentChange: () => void
}) {
  const {
    nickname,
    isBroadcaster,
    broadcastViewerCount,
    engineStats,
    myInstrumentChangeStatus,
    myNewInstrument,
    cancelInstrumentChangeRequest,
    inputMuted,
    toggleInputMuted,
    isMonitoringEnabled,
    monitorGain,
    setMonitor,
  } = useRoom()

  return (
    <div className="performer-item me active">
      <div className="performer-avatar">
        {isHost && <span className="host-crown">👑</span>}
        <div className="avatar-circle">
          <span>{INSTRUMENT_INFO[myInstrument]?.icon || '🎵'}</span>
        </div>
        <span className="live-indicator" />
      </div>
      <div className="performer-info">
        <span className="performer-name">
          {nickname} {isHost && '(방장)'}
          {isBroadcaster && <span className="broadcast-badge" title={`관람자 ${broadcastViewerCount}명에게 믹스 송출 중`}>📡 방송</span>}
        </span>
        <span className="performer-instrument">{INSTRUMENT_INFO[myInstrument]?.name || myInstrument}</span>
        {/* 내 로컬 처리 지연 / xrun */}
        <span className="engine-local-stats" title="로컬 처리 지연 = 입력 버퍼 + 프레임 + 출력 버퍼 (네트워크 제외)">
          처리 지연 {formatMs(engineStats?.estLatencyMs, 1)}
          {' · '}
          <span className={(engineStats?.xruns ?? 0) > 0 ? 'xrun-warn' : ''}>xrun {engineStats?.xruns ?? 0}</span>
        </span>
        <div className="level-meter mini">
          <div className="level-bar">
            <div className="level-fill" style={{ width: `${Math.min(100, (engineStats?.inputLevel ?? 0) * 100)}%` }} />
          </div>
        </div>
        {/* 방장이 아닌 연주자: 악기 변경 요청 UI */}
        {!isHost && myInstrumentChangeStatus === 'none' && (
          <button
            onClick={onOpenInstrumentChange}
            className="change-instrument-btn"
            title="악기 변경 요청"
          >
            🔄 악기 변경
          </button>
        )}
        {!isHost && myInstrumentChangeStatus === 'pending' && (
          <div className="my-instrument-change-status pending">
            <span>⏳ 변경 요청 중: {INSTRUMENT_INFO[myNewInstrument || '']?.name || myNewInstrument}</span>
            <button onClick={cancelInstrumentChangeRequest} className="cancel-btn small">취소</button>
          </div>
        )}
        {!isHost && myInstrumentChangeStatus === 'approved' && (
          <div className="my-instrument-change-status approved">
            <span>✅ 변경 승인됨!</span>
          </div>
        )}
        {!isHost && myInstrumentChangeStatus === 'rejected' && (
          <div className="my-instrument-change-status rejected">
            <span>❌ 변경 거절됨</span>
          </div>
        )}
      </div>
      {/* 내 입력 뮤트 (engine_set_input_muted) */}
      <button
        onClick={toggleInputMuted}
        className={`mic-toggle ${inputMuted ? 'off' : 'on'}`}
        title={inputMuted ? '마이크 켜기' : '마이크 끄기 (상대·관람자에게 무음)'}
      >
        {inputMuted ? '🔇' : '🎤'}
      </button>
      {/* 모니터링 (내 소리 듣기) */}
      <div className="monitoring-controls">
        <button
          onClick={() => setMonitor(!isMonitoringEnabled, monitorGain)}
          className={`monitor-toggle ${isMonitoringEnabled ? 'on' : 'off'}`}
          title={isMonitoringEnabled ? '모니터링 끄기 (내 소리 안 들림)' : '모니터링 켜기 (내 소리 들림)'}
        >
          {isMonitoringEnabled ? '🎧' : '🔕'}
        </button>
        {isMonitoringEnabled && (
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            value={monitorGain}
            onChange={(e) => setMonitor(true, Number(e.target.value))}
            className="monitoring-volume-slider"
            title={`모니터링 볼륨: ${Math.round(monitorGain * 100)}%`}
          />
        )}
      </div>
    </div>
  )
}
