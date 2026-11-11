import { describe, expect, it } from 'vitest'
import type { EchoReport, LoopbackReport } from './engineApi'
import { echoBacklogLine, echoRows, echoText, fmtMs, loopbackBacklogLine, loopbackRows, loopbackText } from './measureFormat'

const loop: LoopbackReport = {
  verdict: 'ok',
  message: '왕복 10.16ms(반복 2회, 흩어짐 0.01ms)',
  runs: [
    { roundTripMs: 10.155, peakRatio: 250, correlation: 0.99, clipped: false, inverted: false, inputPeak: 0.3 },
    { roundTripMs: 10.165, peakRatio: 240, correlation: 0.98, clipped: false, inverted: true, inputPeak: 0.3 },
  ],
  detectedRuns: 2,
  roundTripMs: 10.16,
  minMs: 10.155,
  maxMs: 10.165,
  stdDevMs: 0.005,
  sampleRate: 48000,
  inputBufferFrames: 64,
  outputBufferFrames: 64,
  bufferRoundTripMs: 2.6667,
  driverInputLatencyMs: 2.5,
  driverOutputLatencyMs: 3.5,
  driverRoundTripMs: 6,
  estLatencyMs: 5.17,
  offsetMs: 7.49,
  driverErrorMs: 4.16,
  correctedEstLatencyMs: 12.66,
  backend: 'cpalAsio',
}

const echo: EchoReport = {
  verdict: 'ok',
  message: '네트워크 포함 왕복 24.6ms',
  peerId: 'bob',
  runs: [24.5, null, 24.7],
  detectedRuns: 2,
  usedRuns: 2,
  excludedRuns: 0,
  outlierIndex: null,
  roundTripMs: 24.6,
  minMs: 24.5,
  maxMs: 24.7,
  stdDevMs: 0.1,
  spreadMs: 0.2,
  peerEchoDelayMs: 0.1,
  oneWayMs: 12.25,
  networkRttMs: 11.9,
  jitterBufferMs: 6.9,
  frameMs: 2.5,
  localLatencyMs: 7.83,
  inputBufferMs: 2.67,
  outputBufferMs: 2.67,
  mouthToEarMs: 23.8,
  residualMs: 3.2,
  path: 'lan',
}

describe('measureFormat', () => {
  it('formats ms with fallback', () => {
    expect(fmtMs(1.234)).toBe('1.23ms')
    expect(fmtMs(null)).toBe('—')
    expect(fmtMs(Number.NaN)).toBe('—')
  })

  it('loopback rows: round trip, one-way, driver, est, offset', () => {
    const rows = loopbackRows(loop)
    const get = (l: string) => rows.find((r) => r.label.startsWith(l))?.value
    expect(get('실측 왕복')).toBe('10.16ms')
    expect(get('추정 편도')).toBe('5.08ms')
    expect(get('드라이버 보고 왕복')).toBe('6.00ms')
    expect(get('드라이버 보고 오차')).toBe('+4.16ms')
    expect(get('estLatencyMs')).toBe('5.17ms')
    expect(get('보정값')).toBe('+7.49ms')
    expect(get('판정')).toContain('정상')
    expect(rows.find((r) => r.label.startsWith('드라이버 보고 오차'))?.hint).toContain('ASIOGetLatencies')
    const w = loopbackRows({ ...loop, backend: 'wasapiExclusive' })
    expect(w.find((r) => r.label.startsWith('드라이버 보고 오차'))?.hint).toContain('GetStreamLatency')
  })

  it('loopback without driver report and no signal', () => {
    const r: LoopbackReport = { ...loop, verdict: 'noSignal', roundTripMs: null, offsetMs: null, driverRoundTripMs: null, driverErrorMs: null, detectedRuns: 0 }
    const rows = loopbackRows(r)
    expect(rows.find((x) => x.label.startsWith('드라이버 보고 왕복'))?.hint).toContain('보고하지 않음')
    expect(rows.find((x) => x.label.startsWith('실측 왕복'))?.value).toBe('—')
    expect(loopbackBacklogLine(r, { at: '2026-11-10T00:00:00Z' })).toContain('신호 미검출')
  })

  it('loopback text includes runs and backlog line', () => {
    const t = loopbackText(loop, { at: '2026-11-10T09:00:00Z', device: 'Scarlett Solo 4th Gen ASIO' })
    expect(t).toContain('측정: 2026-11-10T09:00:00Z')
    expect(t).toContain('1. 10.155ms')
    expect(t).toContain('극성 반전')
    expect(t).toContain('백로그 기록: [실측 2026-11-10] 루프백 Scarlett Solo 4th Gen ASIO 48000Hz/64: 왕복 10.16ms')
  })

  it('echo rows and text', () => {
    const rows = echoRows(echo)
    expect(rows.find((r) => r.label.startsWith('편도 추정'))?.value).toBe('12.3ms')
    expect(rows.find((r) => r.label.startsWith('망 RTT'))?.hint).toBe('경로 lan')
    expect(rows[0].value).toContain('2/3')
    const t = echoText(echo, { at: '2026-11-10T09:00:00Z', peerName: '드럼' })
    expect(t).toContain('상대: 드럼')
    expect(t).toContain('2. 미검출')
    expect(echoBacklogLine(echo, { at: '2026-11-10T09:00:00Z', peerName: '드럼' })).toBe(
      '[실측 2026-11-10] 에코 드럼(lan): 왕복 24.6ms, 편도 12.3ms, 입→귀 23.8ms, 핑 11.9ms, 지터 버퍼 6.9ms — 정상',
    )
    expect(rows[0].value).not.toContain('제외')
    expect(rows[0].hint).toContain('흩어짐 0.2ms')
  })

  it('echo with one excluded outlier marks the click', () => {
    const r: EchoReport = {
      ...echo,
      runs: [24.5, 24.7, 19.0, 24.6, 24.8],
      detectedRuns: 5,
      usedRuns: 4,
      excludedRuns: 1,
      outlierIndex: 2,
      spreadMs: 0.3,
    }
    const rows = echoRows(r)
    expect(rows[0].value).toBe('정상 (클릭 5/5개, 튄 값 1개 제외)')
    expect(rows[1].hint).toContain('사용 4개')
    const t = echoText(r, { at: '2026-11-10T09:00:00Z' })
    expect(t).toContain('3. 19.00ms (튄 값, 통계 제외)')
    expect(t).not.toContain('2. 24.70ms (튄')
  })
})
