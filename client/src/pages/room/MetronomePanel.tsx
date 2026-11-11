import { useEffect, useState } from 'react'
import { useRoom } from '../../contexts/RoomContext'
import {
  DEFAULT_METRONOME_BEATS,
  DEFAULT_METRONOME_BPM,
  METRONOME_MAX_BEATS,
  METRONOME_MAX_BPM,
  METRONOME_MIN_BPM,
  clampBeats,
  clampBpm,
} from '../../native/metronomeLogic'

/**
 * 공유 메트로놈(P2-3). 방장은 BPM·박자·시작/정지, 모든 연주자는 내 클릭 볼륨·뮤트.
 * 클릭은 각자 엔진이 지연을 보정해 로컬에서 만든다(네트워크로 보내지 않음, 방송·녹음에도 없음).
 */
export function MetronomePanel({ isHost }: { isHost: boolean }) {
  const { metronome, setHostMetronome, metronomeVolume, metronomeMuted, setMetronomeVolume, engineStats } = useRoom()
  const [bpm, setBpm] = useState(DEFAULT_METRONOME_BPM)
  const [beats, setBeats] = useState(DEFAULT_METRONOME_BEATS)
  const [busy, setBusy] = useState(false)
  const running = metronome?.running ?? false
  const following = running && metronome?.leaderId != null
  const synced = engineStats?.metronome?.synced ?? false

  // 따르는 쪽: 방장 설정을 그대로 보여 준다
  useEffect(() => {
    if (metronome) {
      setBpm(metronome.bpm)
      setBeats(metronome.beatsPerBar)
    }
  }, [metronome])

  const apply = async (on: boolean, nextBpm = bpm, nextBeats = beats) => {
    setBusy(true)
    try {
      await setHostMetronome(on, clampBpm(nextBpm), clampBeats(nextBeats))
    } finally {
      setBusy(false)
    }
  }

  const status = !running
    ? '꺼짐'
    : following && !synced
      ? '방장 시계 맞추는 중…'
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
              value={bpm}
              onChange={(e) => setBpm(Number(e.target.value))}
              onBlur={() => setBpm((v) => clampBpm(v))}
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
            <button className="metronome-btn" disabled={busy} onClick={() => void apply(true)}>적용</button>
          )}
          <button
            className={`metronome-btn ${running ? 'stop' : 'start'}`}
            disabled={busy}
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
    </section>
  )
}
