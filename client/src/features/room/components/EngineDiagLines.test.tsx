// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import type { EngineStats } from '../../../native/engineApi'
import { EngineDiagLines } from './EngineDiagLines'

const stats = (over: Partial<EngineStats> = {}): EngineStats =>
  ({ running: true, sampleRate: 48_000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, peers: [], ...over }) as EngineStats

afterEach(cleanup)

/** TEST-SESSION 3.2·4.2: 송신 큐·릴레이 상태를 개발자 도구 없이 진단 줄에서 읽는다 */
describe('EngineDiagLines — 송신 큐·릴레이', () => {
  it('송신 큐 p99와 릴레이 상태·RTT를 보이고, 마우스를 올리면 자세한 값', () => {
    render(
      <EngineDiagLines
        stats={stats({
          sendQueue: { p50Ms: 0.05, p99Ms: 0.31, maxMs: 0.9, packets: 400 },
          relay: { state: 'bound', addr: '203.0.113.5:5000', rttMs: 18.2, txKbps: 900, overBudget: false, autoOpus: false },
        })}
      />,
    )
    const sq = screen.getByText('송신 큐 p99 0.31ms')
    expect(sq.getAttribute('title')).toContain('1ms 미만이면 정상')
    expect(sq.className).not.toContain('diag-warn')
    const relay = screen.getByText('릴레이 bound(등록됨) · RTT 18ms')
    expect(relay.getAttribute('title')).toContain('relay.autoOpus')
  })

  it('릴레이 미설정·송신 없음이면 줄이 없고, 릴레이 응답 없음·송신 큐 1ms 이상은 경고 색', () => {
    const { container, rerender } = render(<EngineDiagLines stats={stats({ sendQueue: { p50Ms: 0, p99Ms: 0, maxMs: 0, packets: 0 }, relay: null })} />)
    expect(container.textContent).not.toContain('송신 큐')
    expect(container.textContent).not.toContain('릴레이')
    rerender(
      <EngineDiagLines
        stats={stats({
          sendQueue: { p50Ms: 0.5, p99Ms: 1.4, maxMs: 3, packets: 400 },
          relay: { state: 'unreachable', addr: '203.0.113.5:5000', rttMs: null, txKbps: 0, overBudget: false },
        })}
      />,
    )
    expect(screen.getByText('송신 큐 p99 1.40ms').className).toContain('diag-warn')
    expect(screen.getByText('릴레이 unreachable(응답 없음)').className).toContain('diag-warn')
  })
})
