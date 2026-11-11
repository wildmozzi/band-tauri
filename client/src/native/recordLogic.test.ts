import { describe, expect, it } from 'vitest'
import type { RecordStats, RecordSummary } from './engineApi'
import {
  buildRecordOptions,
  formatBytes,
  formatRecordTime,
  recordSessionName,
  recordStatusLine,
  recordSummaryLine,
  recordWarning,
} from './recordLogic'

const stats: RecordStats = {
  folder: 'C:\\Music\\Band Recordings\\s',
  format: 'wav24',
  sampleRate: 48000,
  durationSec: 83.4,
  bytes: 45_600_000,
  tracks: 3,
  droppedFrames: 0,
  error: null,
}

describe('recordLogic', () => {
  it('formatBytes', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(999)).toBe('999 B')
    expect(formatBytes(1500)).toBe('1.5 KB')
    expect(formatBytes(45_600_000)).toBe('45.6 MB')
    expect(formatBytes(123_400_000)).toBe('123 MB')
    expect(formatBytes(1_040_000_000)).toBe('1.0 GB')
    expect(formatBytes(-1)).toBe('0 B')
    expect(formatBytes(NaN)).toBe('0 B')
  })

  it('formatRecordTime', () => {
    expect(formatRecordTime(0)).toBe('0:00')
    expect(formatRecordTime(83.9)).toBe('1:23')
    expect(formatRecordTime(3600 + 62)).toBe('1:01:02')
    expect(formatRecordTime(-5)).toBe('0:00')
  })

  it('recordSessionName은 지역 시각 + 방 제목', () => {
    const d = new Date(2026, 10, 11, 21, 5, 9)
    expect(recordSessionName(d)).toBe('2026-11-11 21.05.09')
    expect(recordSessionName(d, '  금요 합주 ')).toBe('2026-11-11 21.05.09 금요 합주')
    expect(recordSessionName(d, 'x'.repeat(50)).length).toBe(19 + 1 + 30)
    // 이모지(서로게이트 쌍)를 반으로 자르지 않는다
    const t = recordSessionName(d, 'a'.repeat(29) + '🎸🎸')
    expect(t.endsWith('a🎸')).toBe(true)
    expect(t).not.toMatch(/[\uD800-\uDBFF]$/)
  })

  it('buildRecordOptions는 peerId → 이름', () => {
    const o = buildRecordOptions({
      now: new Date(2026, 0, 2, 3, 4, 5),
      roomTitle: '방',
      includeMix: true,
      peers: [{ peerId: 'u-1', name: '앨리스' }, { peerId: 'u-2', name: '밥' }],
    })
    expect(o).toEqual({
      format: 'wav24',
      includeMix: true,
      name: '2026-01-02 03.04.05 방',
      localName: '나',
      trackNames: { 'u-1': '앨리스', 'u-2': '밥' },
    })
  })

  it('상태 줄과 경고', () => {
    expect(recordStatusLine(stats)).toBe('1:23 · 45.6 MB · 3트랙')
    expect(recordWarning(stats)).toBeNull()
    expect(recordWarning({ ...stats, droppedFrames: 4800 })).toBe('디스크가 느려 100ms를 무음으로 채웠습니다')
    expect(recordWarning({ ...stats, droppedFrames: 4800, error: '디스크 가득' })).toBe('저장 실패: 디스크 가득')
  })

  it('정지 요약', () => {
    const s: RecordSummary = {
      folder: stats.folder,
      format: 'wav24',
      sampleRate: 48000,
      channels: 2,
      durationSec: 61,
      bytes: 2_000_000,
      droppedFrames: 0,
      startedAtMs: 0,
      tracks: [
        { kind: 'local', name: '나', file: '01-나.wav', peerId: null },
        { kind: 'peer', name: '앨리스', file: '02-앨리스.wav', peerId: 'u-1' },
      ],
      error: null,
    }
    expect(recordSummaryLine(s)).toBe('저장됨 · 2트랙 · 1:01 · 2.0 MB')
  })
})
