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
