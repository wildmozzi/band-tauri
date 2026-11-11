/**
 * 합주 녹음(P2-4) 표시·옵션 로직. 엔진 계약은 설계 문서 1.5절, 래퍼는 `engineApi.ts`.
 */
import type { RecordFormat, RecordOptions, RecordStats, RecordSummary } from './engineApi'

/** 형식 선택지(녹음 패널) */
export const RECORD_FORMATS: { value: RecordFormat; label: string; title: string }[] = [
  { value: 'wav24', label: 'WAV 24비트', title: '대부분의 DAW에서 바로 열립니다(48kHz 스테레오 1시간 약 1GB/트랙)' },
  { value: 'flac', label: 'FLAC', title: '무손실 압축(24비트, 보통 WAV의 절반 안팎)' },
  { value: 'wavF32', label: 'WAV 32비트 float', title: '클리핑 없이 원본 그대로(WAV 24보다 1/3 큼)' },
]

export function isRecordFormat(v: unknown): v is RecordFormat {
  return RECORD_FORMATS.some((f) => f.value === v)
}

/** 바이트 → "12.3 MB" (1000 단위, 디스크 표기와 같게) */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '0 B'
  if (n < 1000) return `${Math.round(n)} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1000
  let i = 0
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000
    i++
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`
}

/** 초 → "m:ss" 또는 "h:mm:ss" */
export function formatRecordTime(sec: number): string {
  const s = Math.max(0, Math.floor(Number.isFinite(sec) ? sec : 0))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const ss = String(s % 60).padStart(2, '0')
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`
}

const pad = (n: number) => String(n).padStart(2, '0')

/** 세션 폴더 이름: "2026-11-11 21.30.05 방 제목" (지역 시각, 엔진이 금지 문자를 다시 정리한다) */
export function recordSessionName(date: Date, roomTitle?: string | null): string {
  const stamp = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}.${pad(date.getMinutes())}.${pad(date.getSeconds())}`
  // 코드 포인트 단위로 자른다(이모지 서로게이트 쌍을 반으로 자르지 않게)
  const title = Array.from((roomTitle ?? '').trim()).slice(0, 30).join('')
  return title ? `${stamp} ${title}` : stamp
}

/** 피어 트랙 이름 맵: engine peerId(= oderId) → 표시 이름 */
export function trackNameMap(peers: { peerId: string; name: string }[]): Record<string, string> {
  const trackNames: Record<string, string> = {}
  for (const p of peers) trackNames[p.peerId] = p.name
  return trackNames
}

/** 엔진 녹음 옵션: 피어 트랙 이름은 engine peerId(= oderId) → 표시 이름 */
export function buildRecordOptions(params: {
  now: Date
  roomTitle?: string | null
  includeMix: boolean
  peers: { peerId: string; name: string }[]
  format?: RecordFormat
}): RecordOptions {
  const trackNames = trackNameMap(params.peers)
  return {
    format: params.format ?? 'wav24',
    includeMix: params.includeMix,
    name: recordSessionName(params.now, params.roomTitle),
    localName: '나',
    trackNames,
  }
}

/** 녹음 중 상태 줄: "1:23 · 45.6 MB · 3트랙" */
export function recordStatusLine(s: RecordStats): string {
  return `${formatRecordTime(s.durationSec)} · ${formatBytes(s.bytes)} · ${s.tracks}트랙`
}

/** 녹음 품질 경고(버림·쓰기 실패). 없으면 null */
export function recordWarning(s: Pick<RecordStats, 'droppedFrames' | 'sampleRate' | 'error'>): string | null {
  if (s.error) return `저장 실패: ${s.error}`
  if (s.droppedFrames > 0) {
    const ms = (s.droppedFrames / Math.max(1, s.sampleRate)) * 1000
    return `디스크가 느려 ${ms.toFixed(0)}ms를 무음으로 채웠습니다`
  }
  return null
}

/** 정지 후 안내: "저장됨 · 3트랙 · 1:23 · 45.6 MB" (+ 문제 구간이 있으면 " · 끊김 구간 4곳(markers.json)") */
export function recordSummaryLine(s: RecordSummary): string {
  const base = `저장됨 · ${s.tracks.length}트랙 · ${formatRecordTime(s.durationSec)} · ${formatBytes(s.bytes)}`
  const n = s.markers ?? 0
  return n > 0 ? `${base} · 끊김 구간 ${n}곳(${s.markersFile ?? 'markers.json'})` : base
}
