import { useEffect, useState } from 'react'
import { useRoom } from '../state/RoomContext'
import {
  DEFAULT_METRONOME_BEATS,
  DEFAULT_METRONOME_BPM,
  METRONOME_MAX_BEATS,
  METRONOME_MAX_BPM,
  METRONOME_MIN_BPM,
  clampBeats,
  clampBpm,
} from '../../../native/metronomeLogic'
import { OPEN_LATENCY_MEASURE_EVENT, outputLatencyView } from '../../../native/loopbackOffsetStore'

/**
 * 공유 메트로놈(P2-3). 방장은 BPM·박자·시작/정지, 모든 연주자는 내 클릭 볼륨·뮤트.
 * 클릭은 각자 엔진이 지연을 보정해 로컬에서 만든다(네트워크로 보내지 않음, 방송·녹음에도 없음).
 */
/** 따르기 시작 후 이 시간 안에 시계가 안 맞으면 실패 안내 */
const SYNC_TIMEOUT_MS = 5000

export function MetronomePanel({ isHost }: { isHost: boolean }) {
  const { metronome, setHostMetronome, metronomeVolume, metronomeMuted, setMetronomeVolume, engineStats, isPerforming } = useRoom()
  // 입력 중에는 문자열로 두고(빈 칸 = 0이 상태로 들어가지 않게), 적용·blur 때 범위로 자른다(PR #36 L4)
  const [bpmText, setBpmText] = useState(String(DEFAULT_METRONOME_BPM))
  const bpmValid = bpmText.trim() !== '' && Number.isFinite(Number(bpmText))
  const bpm = bpmValid ? clampBpm(Number(bpmText)) : (metronome?.bpm ?? DEFAULT_METRONOME_BPM)
  const [beats, setBeats] = useState(DEFAULT_METRONOME_BEATS)
  const [busy, setBusy] = useState(false)
  const running = metronome?.running ?? false
  const following = running && metronome?.leaderId != null
  const synced = engineStats?.metronome?.synced ?? false
  // 출력 지연 보정 상태(P2-3c): 드라이버 보고 / 루프백 측정 / 없음. 옛 엔진(필드 없음)이면 표시하지 않는다
  const latency = engineStats?.metronome && 'outputLatencySource' in engineStats.metronome ? outputLatencyView(engineStats.metronome) : null
  // 따르기 시작한 뒤 한참 지나도 시계 표본이 없으면(옛 엔진 방장·연결 문제) 안내를 바꾼다(PR #36 L2)
  const [syncTimedOut, setSyncTimedOut] = useState(false)
  const waitingSync = following && !synced
  useEffect(() => {
    setSyncTimedOut(false)
    if (!waitingSync) return
    const t = setTimeout(() => setSyncTimedOut(true), SYNC_TIMEOUT_MS)
    return () => clearTimeout(t)
  }, [waitingSync])

  // 따르는 쪽: 방장 설정을 그대로 보여 준다
  useEffect(() => {
    if (metronome) {
      setBpmText(String(metronome.bpm))
      setBeats(metronome.beatsPerBar)
    }
  }, [metronome])

  const apply = async (on: boolean) => {
    setBusy(true)
    setBpmText(String(bpm))
    try {
      await setHostMetronome(on, bpm, clampBeats(beats))
    } finally {
      setBusy(false)
    }
  }

  const status = !running
    ? '꺼짐'
    : waitingSync
      ? syncTimedOut
        ? '방장과 시계를 맞추지 못했습니다(연결·앱 버전 확인)'
        : '방장 시계 맞추는 중…'
      : `${metronome?.bpm ?? bpm} BPM · ${metronome?.beatsPerBar ?? beats}박`

  return (
    <section className="metronome-panel" aria-label="공유 메트로놈">
      <div className="metronome-header">
        <span className={`metronome-dot ${running ? 'on' : ''}`} aria-hidden="true" />
        <span className="metronome-title">메트로놈</span>
        <span className="metronome-status" title="각자 지연을 보정한 클릭을 로컬에서 들려줍니다. 관람 방송·녹음에는 들어가지 않습니다.">
          {status}
        </span>
      </div>

      {isHost ? (
        <div className="metronome-row">
          <label className="metronome-field">
            BPM
            <input
              type="number"
              min={METRONOME_MIN_BPM}
              max={METRONOME_MAX_BPM}
              value={bpmText}
              onChange={(e) => setBpmText(e.target.value)}
              onBlur={() => setBpmText(String(bpm))}
            />
          </label>
          <label className="metronome-field">
            박자
            <select value={beats} onChange={(e) => setBeats(Number(e.target.value))}>
              {Array.from({ length: METRONOME_MAX_BEATS }, (_, i) => i + 1).map((n) => (
                <option key={n} value={n}>{n}/4</option>
              ))}
            </select>
          </label>
          {running && (bpm !== metronome?.bpm || beats !== metronome?.beatsPerBar) && (
            <button className="metronome-btn" disabled={busy || !isPerforming} onClick={() => void apply(true)}>적용</button>
          )}
          <button
            className={`metronome-btn ${running ? 'stop' : 'start'}`}
            disabled={busy || !isPerforming}
            title={isPerforming ? undefined : '엔진이 켜진 뒤에 조작할 수 있습니다'}
            onClick={() => void apply(!running)}
          >
            {running ? '정지' : '시작'}
          </button>
        </div>
      ) : (
        !running && <p className="metronome-hint">방장이 메트로놈을 켜면 여기서 들립니다.</p>
      )}

      <div className="metronome-row">
        <button
          className={`metronome-mute ${metronomeMuted ? 'off' : 'on'}`}
          onClick={() => setMetronomeVolume(metronomeVolume, !metronomeMuted)}
          title={metronomeMuted ? '클릭 소리 켜기' : '클릭 소리 끄기 (나만)'}
        >
          {metronomeMuted ? '🔇' : '🔔'}
        </button>
        <input
          type="range"
          min="0"
          max="1"
          step="0.01"
          value={metronomeVolume}
          onChange={(e) => setMetronomeVolume(Number(e.target.value), metronomeMuted)}
          className="monitoring-volume-slider metronome-volume"
          aria-label="클릭 볼륨"
          title={`클릭 볼륨: ${Math.round(metronomeVolume * 100)}%`}
        />
      </div>

      {latency && (
        <p className="metronome-hint metronome-latency" data-testid="metronome-latency" data-source={latency.source ?? 'none'}>
          {latency.text}
          {latency.needsMeasure && (
            <>
              {' — '}루프백 측정을 하면 클릭이 더 정확해집니다(연주를 멈춘 뒤).{' '}
              <button
                type="button"
                className="metronome-link"
                onClick={() => window.dispatchEvent(new CustomEvent(OPEN_LATENCY_MEASURE_EVENT))}
              >
                지연 측정 열기
              </button>
            </>
          )}
        </p>
      )}
    </section>
  )
}
