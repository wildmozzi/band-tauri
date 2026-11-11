/**
 * 공유 메트로놈(P2-3) 시그널링 규칙 — React 비의존, 테스트 대상.
 *
 * - 방장: engine_set_metronome(refPeerId 없음) → 엔진이 정한 startUs(방장 엔진 시계)를 `metronome`으로 방에 알린다.
 * - 다른 연주자: `metronome { from, state }`(from = 서버가 붙인 방장 clientId = 방장 엔진 peerId)를 받으면
 *   engine_set_metronome({ ...state, refPeerId: from }). 엔진이 방장 시계 오프셋으로 클릭을 맞춘다.
 * - 서버는 방장 여부를 DB로 확인하고, 엔진을 (다시) 켠 연주자에게 현재 상태를 다시 보낸다.
 */
import type { MetronomeConfig } from './engineApi'

export const METRONOME_MIN_BPM = 20
export const METRONOME_MAX_BPM = 300
export const METRONOME_MAX_BEATS = 16
export const DEFAULT_METRONOME_BPM = 120
export const DEFAULT_METRONOME_BEATS = 4
/** 엔진 클릭 볼륨 기본값(engine_set_metronome_gain) */
export const DEFAULT_METRONOME_VOLUME = 0.8

/** 방에 알리는 메트로놈 계획. startUs = 방장 엔진 시계(µs) 기준 박 0 */
export interface MetronomeSignalState {
  running: boolean
  bpm: number
  beatsPerBar: number
  startUs: number
}

/** UI가 보는 메트로놈 상태. leaderId = null이면 내가 기준(방장) */
export interface MetronomeView {
  running: boolean
  bpm: number
  beatsPerBar: number
  leaderId: string | null
}

export function clampBpm(bpm: number): number {
  if (!Number.isFinite(bpm)) return DEFAULT_METRONOME_BPM
  return Math.min(METRONOME_MAX_BPM, Math.max(METRONOME_MIN_BPM, Math.round(bpm)))
}

export function clampBeats(beats: number): number {
  if (!Number.isFinite(beats)) return DEFAULT_METRONOME_BEATS
  return Math.min(METRONOME_MAX_BEATS, Math.max(1, Math.round(beats)))
}

/** 서버가 중계한 `metronome` 메시지 검증 */
export function parseMetronomeSignal(p: Record<string, unknown>): { from: string; state: MetronomeSignalState } | null {
  if (p.type !== 'metronome' || typeof p.from !== 'string' || !p.from) return null
  const s = p.state
  if (!s || typeof s !== 'object' || Array.isArray(s)) return null
  const { running, bpm, beatsPerBar, startUs } = s as Record<string, unknown>
  if (typeof running !== 'boolean') return null
  if (typeof bpm !== 'number' || !Number.isFinite(bpm) || bpm < METRONOME_MIN_BPM || bpm > METRONOME_MAX_BPM) return null
  if (typeof beatsPerBar !== 'number' || !Number.isInteger(beatsPerBar) || beatsPerBar < 1 || beatsPerBar > METRONOME_MAX_BEATS) return null
  if (typeof startUs !== 'number' || !Number.isFinite(startUs) || startUs < 0) return null
  return { from: p.from, state: { running, bpm, beatsPerBar, startUs } }
}

/** 방장 계획을 따르는 엔진 설정 */
export function followConfig(from: string, s: MetronomeSignalState): MetronomeConfig {
  return { running: s.running, bpm: s.bpm, beatsPerBar: s.beatsPerBar, startUs: s.startUs, refPeerId: from }
}

/** 메트로놈 끄기(기준 무관) */
export function stoppedConfig(bpm = DEFAULT_METRONOME_BPM, beatsPerBar = DEFAULT_METRONOME_BEATS): MetronomeConfig {
  return { running: false, bpm: clampBpm(bpm), beatsPerBar: clampBeats(beatsPerBar) }
}

/** 메트로놈 관련 서버 에러 → 사용자 메시지 */
export function metronomeErrorFromCode(code: unknown, serverMessage?: unknown): string | null {
  const msg = typeof serverMessage === 'string' && serverMessage ? serverMessage : null
  if (code === 'metronome-host-only') return msg ?? '메트로놈은 방장만 조작할 수 있습니다.'
  if (code === 'metronome-invalid') return msg ?? '메트로놈 설정이 올바르지 않습니다.'
  return null
}
