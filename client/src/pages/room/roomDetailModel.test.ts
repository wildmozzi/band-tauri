import { describe, expect, it } from 'vitest'
import type { PeerStats } from '../../native/engineApi'
import {
  cpuTitle,
  formatCpu,
  formatDuration,
  formatMs,
  formatPan,
  instrumentInfo,
  instrumentUsage,
  levelClass,
  lossQualityIcon,
  normalizeDetailRoom,
  peerStatsTitle,
  performerName,
  recordingExtension,
  recordingFilename,
  recordingFormatLabel,
  recordingTimeLabel,
  remotePeerStatus,
} from './roomDetailModel'
import { bubblePositionFor } from './useBubblePositions'
import { startsWithoutApproval } from './usePerformControls'
import { settingsSaveErrorMessage } from './useRoomSettingsForm'

const stats = (over: Partial<PeerStats> = {}): PeerStats => ({
  state: 'connected', path: 'direct', rttMs: 12.4, jitterMs: 1.26, bufferMs: 10, lossPct: 0.5, late: 1, concealed: 2,
  ...over,
} as PeerStats)

describe('roomDetailModel', () => {
  it('formatCpu/cpuTitle: 첫 창 전·옛 엔진은 숨기고, 워커·콜백 사용률·p99를 보인다', () => {
    const cpu = {
      workerPct: 3.14,
      callbackLoadPct: 0.81,
      callbackP50Us: 20.2,
      callbackP99Us: 41.6,
      callbackMaxUs: 120,
      callbackP99PeriodPct: 1.6,
      inputCallbackMaxUs: 9,
      callbacks: 750,
      windowMs: 2000,
    }
    expect(formatCpu(undefined)).toBeNull()
    expect(formatCpu({ ...cpu, windowMs: 0 })).toBeNull()
    expect(formatCpu(cpu)).toBe('CPU 워커 3.1% · 콜백 0.8% (p99 42µs)')
    expect(formatCpu({ ...cpu, workerPct: null, callbacks: 0 })).toBe('CPU 워커 -- · 콜백 0.8%')
    expect(cpuTitle(cpu)).toContain('20/42/120µs')
    expect(cpuTitle({ ...cpu, workerPct: null })).toContain('측정 불가')
    expect(cpuTitle(cpu)).toContain('앱 전체 CPU가 아닙니다')
  })

  it('formatMs: 유한값만 ms로, 나머지는 --', () => {
    expect(formatMs(12.44)).toBe('12ms')
    expect(formatMs(1.26, 1)).toBe('1.3ms')
    expect(formatMs(null)).toBe('--')
    expect(formatMs(undefined)).toBe('--')
    expect(formatMs(Infinity)).toBe('--')
  })

  it('peerStatsTitle: 상태·경로·지연·손실을 줄마다', () => {
    const lines = peerStatsTitle(stats()).split('\n')
    expect(lines[0]).toBe('상태: 연결됨')
    expect(lines).toContain('RTT: 12ms')
    expect(lines).toContain('손실: 0.5%')
    expect(lines[lines.length - 1]).toBe('늦은 패킷: 1 · 은닉: 2')
  })

  it('remotePeerStatus: 연주 중이면 엔진 상태(통계 없으면 연결 중), 관람이면 배지 없이 활성', () => {
    expect(remotePeerStatus(true, undefined)).toMatchObject({ state: 'connecting', isActive: false })
    expect(remotePeerStatus(true, stats())).toMatchObject({ state: 'connected', isActive: true, stateInfo: { className: 'connected' } })
    expect(remotePeerStatus(true, stats({ state: 'failed' })).stateInfo?.label).toBe('연결 실패')
    expect(remotePeerStatus(false, stats())).toEqual({ state: null, stateInfo: null, isActive: true })
  })

  it('lossQualityIcon / levelClass / formatPan 경계값', () => {
    expect([0.99, 1, 4.99, 5].map(lossQualityIcon)).toEqual(['🟢', '🟡', '🟡', '🔴'])
    expect([50, 51, 80, 81].map(levelClass)).toEqual(['', 'mid', 'mid', 'high'])
    expect(formatPan(-0.5)).toBe('L50')
    expect(formatPan(0.3)).toBe('R30')
    expect(formatPan(0)).toBe('C')
  })

  it('녹음 표시: 시간, 형식, 확장자, 파일명', () => {
    expect(formatDuration(65)).toBe('1:05')
    expect(formatDuration(0)).toBe('0:00')
    const ts = new Date(2026, 0, 2, 3, 4).getTime()
    expect(recordingTimeLabel(ts)).toBe('03:04')
    expect(recordingFormatLabel('audio/webm;codecs=opus')).toBe('WebM')
    expect(recordingFormatLabel('audio/mp4')).toBe('M4A')
    expect(recordingFormatLabel('audio/wav')).toBe('Audio')
    expect(recordingExtension('audio/mpeg')).toBe('mp3')
    expect(recordingExtension('audio/wav')).toBe('wav')
    expect(recordingExtension('audio/webm')).toBe('webm')
    expect(recordingFilename(ts, 'audio/ogg')).toBe('recording_20260102_0304.ogg')
  })

  it('normalizeDetailRoom: 자유참여 기본 true, 악기 구성 없으면 기본 5슬롯, 있으면 유지', () => {
    const base = { id: 'r', title: 't', host_id: 'h' }
    const a = normalizeDetailRoom({ ...base, free_join: null, instrument_slots: null })
    expect(a.free_join).toBe(true)
    expect(a.instrument_slots).toHaveLength(5)
    const b = normalizeDetailRoom({ ...base, free_join: false, instrument_slots: [{ instrument: 'bass', count: 2 }] })
    expect(b.free_join).toBe(false)
    expect(b.instrument_slots).toEqual([{ instrument: 'bass', count: 2 }])
    // 기본 슬롯은 방마다 새 배열(공유 상수를 고치지 않게)
    expect(normalizeDetailRoom(base).instrument_slots).not.toBe(a.instrument_slots)
  })

  it('instrumentUsage / instrumentInfo / performerName', () => {
    const peers = { a: { instrument: 'guitar' }, b: { instrument: 'guitar' }, c: { instrument: 'drums' } }
    expect(instrumentUsage(peers, 'guitar')).toBe(2)
    expect(instrumentUsage(peers, 'vocal')).toBe(0)
    expect(instrumentInfo('drums')).toEqual({ icon: '🥁', name: '드럼' })
    expect(instrumentInfo('kazoo')).toEqual({ icon: '🎵', name: 'kazoo' })
    expect(performerName('', 'abcdef')).toBe('연주자 abcd')
    expect(performerName('닉', 'abcdef')).toBe('닉')
  })

  it('훅의 순수 부분: 말풍선 위치, 승인 필요 여부, 저장 실패 문구', () => {
    expect(bubblePositionFor({ top: 100, height: 40, right: 300 })).toEqual({ top: 120, left: 308 })
    expect(startsWithoutApproval(true, false)).toBe(true)
    expect(startsWithoutApproval(false, true)).toBe(true)
    expect(startsWithoutApproval(false, false)).toBe(false)
    expect(startsWithoutApproval(undefined, false)).toBe(false)
    expect(settingsSaveErrorMessage(new Error('x'))).toBe('설정 저장에 실패했습니다: x')
    expect(settingsSaveErrorMessage('?')).toBe('설정 저장에 실패했습니다.')
  })
})
