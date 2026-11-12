import { describe, expect, it } from 'vitest'
import type { PeerStats } from '../../native/engineApi'
import {
  EAR_LATENCY_THRESHOLDS,
  earLatencyLevel,
  earLatencyTitleLines,
  estimateEarLatencyMs,
  formatEarLatency,
} from './earLatency'

const peer = (over: Partial<PeerStats> = {}): PeerStats => ({
  peerId: 'p1',
  state: 'connected',
  path: 'lan',
  rttMs: 10,
  jitterMs: 0.5,
  bufferMs: 6,
  targetMs: 6,
  concealPct: 0,
  corrTimePct: 0,
  maxCorrCents: 0,
  lossPct: 0,
  late: 0,
  concealed: 0,
  level: 0,
  ...over,
})

describe('estimateEarLatencyMs (P2-8)', () => {
  it('RTT/2 + playoutMs. 상대 프레임을 모르면 근사(≈)', () => {
    const est = estimateEarLatencyMs(peer({ rttMs: 10, playoutMs: 12 }), { devicePeriodMs: 3 })
    expect(est.totalMs).toBe(17)
    expect(est.approx).toBe(true)
    expect(est.oneWayMs).toBe(5)
    expect(est.remoteFrameMs).toBeNull()
    // playoutMs에 출력 장치 버퍼가 이미 들어 있어 devicePeriodMs를 다시 더하지 않는다
    expect(est.outputMs).toBeNull()
    expect(formatEarLatency(est)).toBe('귀까지 ≈ 17 ms')
  })

  it('RTT null이면 추정 불가 → "—"', () => {
    const est = estimateEarLatencyMs(peer({ rttMs: null, playoutMs: 12 }), null)
    expect(est.totalMs).toBeNull()
    expect(earLatencyLevel(est.totalMs)).toBeNull()
    expect(formatEarLatency(est)).toBe('귀까지 —')
    expect(earLatencyTitleLines(est)[0]).toContain('RTT 없음')
  })

  it('재생 전(playoutMs = 0)이면 RTT/2만으로 과소 표시하지 않고 "—"', () => {
    const est = estimateEarLatencyMs(peer({ rttMs: 10, playoutMs: 0 }), { devicePeriodMs: 3 })
    expect(est.playing).toBe(false)
    expect(est.totalMs).toBeNull()
    expect(formatEarLatency(est)).toBe('귀까지 —')
    expect(earLatencyTitleLines(est)[0]).toContain('재생 전')
  })

  it('옛 엔진(playoutMs 없음): bufferMs + 장치 출력 주기', () => {
    const est = estimateEarLatencyMs(peer({ rttMs: 8, bufferMs: 6 }), { devicePeriodMs: 2.7 })
    expect(est.playoutMs).toBe(6)
    expect(est.outputMs).toBe(2.7)
    expect(est.totalMs).toBeCloseTo(12.7)
  })

  it('필드 없음(playoutMs·devicePeriodMs 모두 없음)이면 bufferMs만, 근사', () => {
    const est = estimateEarLatencyMs(peer({ rttMs: 8, bufferMs: 6 }), undefined)
    expect(est.totalMs).toBe(10)
    expect(est.outputMs).toBeNull()
    expect(est.approx).toBe(true)
  })

  it.each([
    ['PCM16 2.5ms 프레임', 2.5, 19.5],
    ['Opus 5ms 프레임', 5, 22],
    ['Opus 10ms 프레임', 10, 27],
  ])('상대 프레임을 알면 더하고 근사 표시를 뗀다: %s', (_name, frame, total) => {
    const est = estimateEarLatencyMs(peer({ rttMs: 10, playoutMs: 12 }), null, frame)
    expect(est.remoteFrameMs).toBe(frame)
    expect(est.totalMs).toBeCloseTo(total)
    expect(est.approx).toBe(false)
    expect(formatEarLatency(est)).toBe(`귀까지 ${Math.round(total)} ms`)
  })

  it('잘못된 상대 프레임(0·NaN)은 무시', () => {
    expect(estimateEarLatencyMs(peer({ playoutMs: 12 }), null, 0).remoteFrameMs).toBeNull()
    expect(estimateEarLatencyMs(peer({ playoutMs: 12 }), null, Number.NaN).approx).toBe(true)
  })

  it('색 구간은 GOAL 1절 수치(25 / 40ms) 경계 포함', () => {
    expect(EAR_LATENCY_THRESHOLDS).toEqual({ goodMs: 25, fairMs: 40 })
    expect(earLatencyLevel(25)).toBe('good')
    expect(earLatencyLevel(25.1)).toBe('fair')
    expect(earLatencyLevel(40)).toBe('fair')
    expect(earLatencyLevel(40.1)).toBe('poor')
  })
})
