import { describe, expect, it } from 'vitest'
import type { PeerStats } from './engineApi'
import {
  firstAudioOverGoal,
  formatFirstAudio,
  newFirstAudioTrack,
  noteAddPeer,
  noteStats,
  summarizeFirstAudio,
} from './firstAudio'

const peer = (peerId: string, firstAudioMs: number | null | undefined): PeerStats =>
  ({ peerId, state: 'connected', path: 'lan', firstAudioMs }) as PeerStats

describe('firstAudio (P1-20)', () => {
  it('시작 때 있던 연주자는 연주 시작부터: 서버 왕복(시작 → add_peer) + 엔진 firstAudioMs', () => {
    let t = newFirstAudioTrack(1000, ['A', 'B'])
    t = noteAddPeer(t, 'A', 1300)
    t = noteAddPeer(t, 'B', 1400)
    t = noteStats(t, [peer('A', 200), peer('B', null)])
    expect(t.heard.get('A')).toBe(500)
    expect(summarizeFirstAudio(t, [peer('A', 200), peer('B', null)])).toEqual({ firstMs: 500, lastMs: 500, heard: 1, total: 2 })
    t = noteStats(t, [peer('A', 200), peer('B', 900)])
    expect(summarizeFirstAudio(t, [peer('A', 200), peer('B', 900)])).toEqual({ firstMs: 500, lastMs: 1300, heard: 2, total: 2 })
  })

  it('시작 뒤에 들어온 연주자는 처음 추가한 시각(그 사람의 입장)부터', () => {
    let t = newFirstAudioTrack(0, [])
    t = noteAddPeer(t, 'C', 60_000)
    t = noteStats(t, [peer('C', 250)])
    expect(t.heard.get('C')).toBe(250)
  })

  it('한 번 들린 값은 고정: 재추가(엔진 firstAudioMs 리셋)·같은 통계 반복에도 그대로', () => {
    let t = newFirstAudioTrack(0, ['A'])
    t = noteAddPeer(t, 'A', 100)
    t = noteStats(t, [peer('A', 400)])
    t = noteAddPeer(t, 'A', 30_000)
    const same = noteStats(t, [peer('A', 50)])
    expect(same).toBe(t)
    expect(t.heard.get('A')).toBe(500)
  })

  it('재추가 전에 들리지 않았으면 기준은 그대로, add 시각만 새 호출로', () => {
    let t = newFirstAudioTrack(0, ['A'])
    t = noteAddPeer(t, 'A', 100)
    t = noteAddPeer(t, 'A', 2000) // 실패 후 재시도
    t = noteStats(t, [peer('A', 300)])
    expect(t.heard.get('A')).toBe(2300)
  })

  it('옛 엔진(firstAudioMs 없음)·추가 기록 없는 피어는 대기로 남는다', () => {
    let t = newFirstAudioTrack(0, ['A'])
    t = noteStats(t, [peer('A', 100), peer('X', undefined)])
    expect(t.heard.size).toBe(0)
    expect(summarizeFirstAudio(t, [peer('A', 100)])).toEqual({ firstMs: null, lastMs: null, heard: 0, total: 1 })
  })

  it('피어가 없거나 추적이 없으면 요약 없음', () => {
    expect(summarizeFirstAudio(null, [peer('A', 1)])).toBeNull()
    expect(summarizeFirstAudio(newFirstAudioTrack(0, []), [])).toBeNull()
  })

  it('문구와 목표(3초) 초과 표시', () => {
    expect(formatFirstAudio(null)).toBeNull()
    expect(formatFirstAudio({ firstMs: null, lastMs: null, heard: 0, total: 2 })).toBe('첫 소리 대기 (0/2명)')
    expect(formatFirstAudio({ firstMs: 820, lastMs: 820, heard: 1, total: 1 })).toBe('첫 소리 0.82초')
    expect(formatFirstAudio({ firstMs: 820, lastMs: 1400, heard: 2, total: 3 })).toBe('첫 소리 0.82초~1.40초 (2/3명)')
    expect(firstAudioOverGoal({ firstMs: 820, lastMs: 3001, heard: 2, total: 2 })).toBe(true)
    expect(firstAudioOverGoal({ firstMs: 820, lastMs: 3000, heard: 2, total: 2 })).toBe(false)
    expect(firstAudioOverGoal(null)).toBe(false)
  })
})
