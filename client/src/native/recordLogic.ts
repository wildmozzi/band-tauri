/**
 * 합주 녹음(P2-4) 표시·옵션 로직. 엔진 계약은 설계 문서 1.5절, 래퍼는 `engineApi.ts`.
 */
import type { RecordOptions, RecordStats, RecordSummary } from './engineApi'

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
  const title = (roomTitle ?? '').trim().slice(0, 30)
  return title ? `${stamp} ${title}` : stamp
}

/** 엔진 녹음 옵션: 피어 트랙 이름은 engine peerId(= oderId) → 표시 이름 */
export function buildRecordOptions(params: {
  now: Date
  roomTitle?: string | null
  includeMix: boolean
  peers: { peerId: string; name: string }[]
}): RecordOptions {
  const trackNames: Record<string, string> = {}
  for (const p of params.peers) trackNames[p.peerId] = p.name
  return {
    format: 'wav24',
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

/** 정지 후 안내: "저장됨 · 3트랙 · 1:23 · 45.6 MB" */
export function recordSummaryLine(s: RecordSummary): string {
  return `저장됨 · ${s.tracks.length}트랙 · ${formatRecordTime(s.durationSec)} · ${formatBytes(s.bytes)}`
}
