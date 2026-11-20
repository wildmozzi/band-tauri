/**
 * useRoomEngineStats에 넘기는 엔진 통계 조각 선택자(모듈 수준 — 참조가 고정돼야 캐시가 맞는다).
 * 틱마다 바뀌는 값(박 번호·클릭 수 등)을 빼서, 쓰는 값이 그대로면 통계 틱(500ms)에 다시 그리지 않게 한다.
 */
import type { EngineStats, RecordStats } from '../../../native/engineApi'

/** 메트로놈 패널: 동기 여부와 출력 지연 보정값. latencyFields=false면 지연 보정 필드가 없는 옛 엔진 */
export const pickMetronomeSync = (stats: EngineStats | null) => {
  const m = stats?.metronome
  return {
    synced: m?.synced ?? false,
    latencyFields: !!m && 'outputLatencySource' in m,
    outputLatencyMs: m?.outputLatencyMs ?? null,
    outputLatencySource: m?.outputLatencySource ?? null,
  }
}

/** 녹음 컨트롤: 녹음 통계(녹음하지 않으면 null — 이때는 틱마다 다시 그리지 않는다) */
export const pickRecordStats = (stats: EngineStats | null): RecordStats | null => stats?.recording ?? null

/**
 * 메트로놈 동기 진단 줄(TEST-SESSION 3.5절 5번): 시계 왕복·기울기, 늦은·뺀·옮긴 클릭(+ 툴팁의 엔진 시작 시각 차 clockOffsetMs).
 * 메트로놈이 꺼졌거나 통계가 없으면 running=false라 틱마다 다시 그리지 않는다. 값은 표시 자릿수로 반올림해 같은 표시면 다시 그리지 않는다.
 */
export const pickMetronomeDiag = (stats: EngineStats | null) => {
  const m = stats?.metronome
  const round = (v: number | null | undefined, step: number) => (typeof v === 'number' && Number.isFinite(v) ? Math.round(v / step) * step : null)
  if (!m?.running) return { running: false as const }
  return {
    running: true as const,
    synced: m.synced,
    refPeer: m.refPeerId != null,
    refClockUnsupported: m.refClockUnsupported ?? false,
    clockRttMs: round(m.clockRttMs, 0.1),
    clockOffsetMs: round(m.clockOffsetMs, 0.01),
    driftPpm: round(m.driftPpm, 0.1),
    lateClicks: m.lateClicks ?? null,
    skippedClicks: m.skippedClicks ?? null,
    shiftedClicks: m.shiftedClicks ?? null,
  }
}

export type MetronomeDiag = ReturnType<typeof pickMetronomeDiag>
