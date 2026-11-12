import type { MetronomeState } from './protocol'

/** 클라이언트가 보낸 메트로놈 계획 검증(엔진 계약과 같은 범위: bpm 20..300, 박자 1..16) */
export function parseMetronomeState(v: unknown): MetronomeState | null {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null
  const { running, bpm, beatsPerBar, startUs } = v as Record<string, unknown>
  if (typeof running !== 'boolean') return null
  if (typeof bpm !== 'number' || !Number.isFinite(bpm) || bpm < 20 || bpm > 300) return null
  if (typeof beatsPerBar !== 'number' || !Number.isInteger(beatsPerBar) || beatsPerBar < 1 || beatsPerBar > 16) return null
  if (typeof startUs !== 'number' || !Number.isFinite(startUs) || startUs < 0 || startUs > Number.MAX_SAFE_INTEGER) return null
  return { running, bpm, beatsPerBar, startUs }
}
