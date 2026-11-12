/**
 * 음악재생 방(MusicRoom.tsx, 데모)의 계산 — UI·플레이어와 분리한 순수 함수 (P3-2 ③-4, vitest로 검증).
 */
import type { StemSeparation } from './musicApi'

// 스템 이름 한글 매핑
export const STEM_LABELS: Record<string, { icon: string; label: string }> = {
  vocals: { icon: '🎤', label: '보컬' },
  drums: { icon: '🥁', label: '드럼' },
  bass: { icon: '🎸', label: '베이스' },
  piano: { icon: '🎹', label: '피아노' },
  guitar: { icon: '🎸', label: '기타' },
  other: { icon: '🎼', label: '기타 악기' },
}

/** 스템 표시: 모르는 이름은 🎵 + 이름 그대로 */
export function stemInfo(stemName: string): { icon: string; label: string } {
  return STEM_LABELS[stemName] || { icon: '🎵', label: stemName }
}

const HANGUL = /[가-힣ᄀ-ᇿ㄰-㆏]/

/** DB에 깨진 이름(UTF-8 바이트를 Latin-1로 읽은 것)으로 저장된 기존 레코드 이름을 되살린다. 한글이 안 나오면 원래 이름 */
export function decodeLegacyFilename(name: string): string {
  if (HANGUL.test(name)) return name
  try {
    const bytes = new Uint8Array([...name].map(c => c.charCodeAt(0) & 0xFF))
    const decoded = new TextDecoder('utf-8').decode(bytes)
    if (HANGUL.test(decoded)) return decoded
  } catch { /* UTF-8 디코딩 실패 시 원래 이름 사용 */ }
  return name
}

/** 브라우저가 붙인 중복 번호 제거: "곡 (1).mp3" → "곡.mp3" */
export function stripCopySuffix(name: string): string {
  return name.replace(/ \(\d+\)(\.[^.]+)$/, '$1')
}

/** 만료되지 않은 같은 이름(중복 번호·깨진 이름 무시)의 분리 기록이 있는지 */
export function hasDuplicateSeparation(
  history: Pick<StemSeparation, 'expires_at' | 'original_filename'>[],
  fileName: string,
  now: Date,
): boolean {
  const uploadBaseName = stripCopySuffix(fileName)
  return history.some(h => {
    if (new Date(h.expires_at) <= now) return false
    const decoded = decodeLegacyFilename(h.original_filename)
    return stripCopySuffix(decoded) === uploadBaseName || decoded === fileName
  })
}

/** 진행률 바 위 마우스 x → 0~1 */
export function progressAt(clientX: number, rect: Pick<DOMRect, 'left' | 'width'>): number {
  return Math.max(0, Math.min(1, (clientX - rect.left) / rect.width))
}

/** 스템 목록 → 전부 켠 상태 */
export function allStemsEnabled(stems: string[]): Record<string, boolean> {
  const states: Record<string, boolean> = {}
  stems.forEach(stem => { states[stem] = true })
  return states
}

/** 빈 값(URL 없음) 스템을 뺀다 — 플레이어에 넘길 것만 */
export function nonEmptyStems(stems: object): Record<string, string> {
  const valid: Record<string, string> = {}
  for (const [key, value] of Object.entries(stems) as [string, string | undefined | null][]) {
    if (value) valid[key] = value
  }
  return valid
}

/** 시간 포맷 (초 → M:SS) */
export function formatPlaybackTime(seconds: number): string {
  const mins = Math.floor(seconds / 60)
  const secs = Math.floor(seconds % 60)
  return `${mins}:${secs.toString().padStart(2, '0')}`
}

/** 히스토리 날짜 표시 (예: 2026년 11월 12일) */
export function formatHistoryDate(dateString: string): string {
  const date = new Date(dateString)
  return date.toLocaleDateString('ko-KR', { year: 'numeric', month: 'short', day: 'numeric' })
}

/** base64 data URL → Blob (MIME 없으면 audio/wav) */
export function dataUrlToBlob(dataUrl: string): Blob {
  const [header, base64Data] = dataUrl.split(',')
  const mimeType = header.match(/data:(.*);/)?.[1] || 'audio/wav'
  const byteCharacters = atob(base64Data)
  const byteArray = new Uint8Array(byteCharacters.length)
  for (let i = 0; i < byteCharacters.length; i++) {
    byteArray[i] = byteCharacters.charCodeAt(i)
  }
  return new Blob([byteArray], { type: mimeType })
}

/** 서버 파일 경로의 마지막 이름(없으면 <스템>.wav) */
export function stemFileNameFromPath(path: string, stemName: string): string {
  return path.split(/[/\\]/).pop() || `${stemName}.wav`
}
