// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import type { EngineStats, PeerStats } from '../../../native/engineApi'

// 에코 측정 메뉴는 이 테스트 대상이 아니다
vi.mock('../../settings', () => ({ EchoMeasure: () => null }))

import { RemotePerformerItem } from './RemotePerformerItem'

const peer = (over: Partial<PeerStats> = {}): PeerStats => ({
  peerId: 'bob',
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

function renderItem(ps: PeerStats | undefined, engineStats: Partial<EngineStats> | null = { devicePeriodMs: 2.7, peers: [] }) {
  return render(
    <RemotePerformerItem
      oderId="bob"
      peerInfo={{ peerId: 'bob', instrument: 'bass', nickname: '밥', isHost: false }}
      isPerformer
      isBroadcaster={false}
      ps={ps}
      engineStats={engineStats as EngineStats | null}
      relayError={null}
      muted={false}
      onToggleMute={() => {}}
      rowRef={() => {}}
    />,
  )
}

describe('RemotePerformerItem 입→귀 추정 지연 (P2-8)', () => {
  afterEach(() => cleanup())

  it('"귀까지 ≈ NN ms"와 색 구간, RTT는 진단 툴팁에', () => {
    renderItem(peer({ rttMs: 10, playoutMs: 12 }))
    const ear = screen.getByTestId('ear-latency')
    expect(ear.textContent).toBe('귀까지 ≈ 17 ms')
    expect(ear.className).toContain('good')
    const title = ear.parentElement?.getAttribute('title') ?? ''
    expect(title).toContain('입→귀 추정: ≈ 17.0ms')
    expect(title).toContain('RTT: 10ms')
  })

  it('40ms 넘으면 빨강, 25~40 노랑', () => {
    renderItem(peer({ rttMs: 40, playoutMs: 25 }))
    expect(screen.getByTestId('ear-latency').className).toContain('poor')
    cleanup()
    renderItem(peer({ rttMs: 20, playoutMs: 20 }))
    expect(screen.getByTestId('ear-latency').className).toContain('fair')
  })

  it('RTT 없으면 "—"', () => {
    renderItem(peer({ rttMs: null, playoutMs: 12 }))
    const ear = screen.getByTestId('ear-latency')
    expect(ear.textContent).toBe('귀까지 —')
    expect(ear.className).toContain('unknown')
  })

  it('피어 통계가 없으면(관람·연결 전) 표시하지 않는다', () => {
    renderItem(undefined)
    expect(screen.queryByTestId('ear-latency')).toBeNull()
  })
})
