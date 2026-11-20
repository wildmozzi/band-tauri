// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { EngineStats, MetronomeStats } from '../../../native/engineApi'
import { OPEN_LATENCY_MEASURE_EVENT } from '../../../native/loopbackOffsetStore'

const room = vi.hoisted(() => ({
  metronome: null,
  setHostMetronome: vi.fn(),
  metronomeVolume: 0.7,
  metronomeMuted: false,
  setMetronomeVolume: vi.fn(),
  engineStats: null as Partial<EngineStats> | null,
  isPerforming: true,
}))
vi.mock('../state/roomHooks', () => ({ useRoomPerform: () => room, useRoomMetronome: () => room, useRoomEngineStats: (pick: (s: unknown) => unknown) => pick(room.engineStats) }))

import { MetronomePanel } from './MetronomePanel'

const metro = (over: Partial<MetronomeStats>) => ({ synced: true, outputLatencyMs: null, outputLatencySource: null, ...over }) as MetronomeStats

describe('MetronomePanel 출력 지연 보정 표시 (P2-3c)', () => {
  afterEach(() => cleanup())

  it('보정 없음이면 루프백 측정 안내와 측정 화면 열기', () => {
    room.engineStats = { metronome: metro({}) }
    const onOpen = vi.fn()
    window.addEventListener(OPEN_LATENCY_MEASURE_EVENT, onOpen)
    render(<MetronomePanel isHost={false} />)
    const line = screen.getByTestId('metronome-latency')
    expect(line.dataset.source).toBe('none')
    expect(line.textContent).toContain('루프백 측정을 하면 클릭이 더 정확해집니다')
    fireEvent.click(screen.getByRole('button', { name: '지연 측정 열기' }))
    expect(onOpen).toHaveBeenCalledTimes(1)
    window.removeEventListener(OPEN_LATENCY_MEASURE_EVENT, onOpen)
  })

  it('드라이버 보고·루프백 측정이면 출처와 값만', () => {
    room.engineStats = { metronome: metro({ outputLatencyMs: 4.2, outputLatencySource: 'loopback' }) }
    render(<MetronomePanel isHost={false} />)
    expect(screen.getByTestId('metronome-latency').textContent).toBe('출력 지연 보정: 루프백 측정 4.2ms')
    expect(screen.queryByRole('button', { name: '지연 측정 열기' })).toBeNull()
  })

  it('옛 엔진(필드 없음)·엔진 꺼짐이면 표시하지 않는다', () => {
    room.engineStats = { metronome: { synced: true } as MetronomeStats }
    render(<MetronomePanel isHost={false} />)
    expect(screen.queryByTestId('metronome-latency')).toBeNull()
    cleanup()
    room.engineStats = null
    render(<MetronomePanel isHost={false} />)
    expect(screen.queryByTestId('metronome-latency')).toBeNull()
  })
})

describe('MetronomePanel 동기 진단 줄 (TEST-SESSION 3.5절 5번 — Console 대신 화면)', () => {
  afterEach(() => cleanup())
  const counts = { lateClicks: 0, skippedClicks: 2, shiftedClicks: 1 }

  it('따르는 쪽: 시계 RTT·오프셋·늦은 클릭·뺀 박, 툴팁에 엔진 필드 이름과 기울기', () => {
    room.engineStats = {
      metronome: metro({ running: true, refPeerId: 'host', clockRttMs: 2.137, clockOffsetMs: 0.3449, driftPpm: -12.34, ...counts }),
    }
    render(<MetronomePanel isHost={false} />)
    const line = screen.getByTestId('metronome-sync')
    expect(line.textContent).toBe('시계 RTT 2.1ms · 오프셋 +0.34ms · 늦은 클릭 0 · 뺀 박 2')
    expect(line.className).not.toContain('warn')
    const title = line.getAttribute('title')!
    expect(title).toContain('동기(synced): 맞춤')
    expect(title).toContain('방장 시계와 왕복(clockRttMs): 2.1ms')
    expect(title).toContain('방장 시계 − 내 시계(clockOffsetMs): +0.34ms')
    expect(title).toContain('시계 기울기(driftPpm): -12.3ppm')
    expect(title).toContain('옮긴 클릭(shiftedClicks): 1')
  })

  it('방장: 기준 시계, 늦은 클릭이 있으면 경고 색', () => {
    room.engineStats = { metronome: metro({ running: true, refPeerId: null, clockRttMs: null, clockOffsetMs: 0, ...counts, lateClicks: 3 }) }
    render(<MetronomePanel isHost />)
    const line = screen.getByTestId('metronome-sync')
    expect(line.textContent).toBe('시계 기준(방장) · 늦은 클릭 3 · 뺀 박 2')
    expect(line.className).toContain('warn')
    expect(line.getAttribute('title')).toContain('내가 기준 시계(방장)')
  })

  it('옛 엔진 방장(시계 없음)이면 맞출 수 없음 + 경고, 시계 표본 전이면 —', () => {
    room.engineStats = { metronome: metro({ running: true, synced: false, refPeerId: 'host', refClockUnsupported: true, clockRttMs: null, clockOffsetMs: null, ...counts }) }
    render(<MetronomePanel isHost={false} />)
    expect(screen.getByTestId('metronome-sync').textContent).toBe('시계 맞출 수 없음(방장 앱이 옛 버전) · 늦은 클릭 0 · 뺀 박 2')
    expect(screen.getByTestId('metronome-sync').className).toContain('warn')
    cleanup()
    room.engineStats = { metronome: metro({ running: true, synced: false, refPeerId: 'host', refClockUnsupported: false, clockRttMs: null, clockOffsetMs: null, ...counts }) }
    render(<MetronomePanel isHost={false} />)
    expect(screen.getByTestId('metronome-sync').textContent).toBe('시계 RTT — · 오프셋 — · 늦은 클릭 0 · 뺀 박 2')
    expect(screen.getByTestId('metronome-sync').getAttribute('title')).toContain('동기(synced): 맞추는 중')
  })

  it('메트로놈이 꺼졌거나 엔진 통계가 없으면 줄이 없다', () => {
    room.engineStats = { metronome: metro({ running: false, ...counts }) }
    render(<MetronomePanel isHost={false} />)
    expect(screen.queryByTestId('metronome-sync')).toBeNull()
    cleanup()
    room.engineStats = null
    render(<MetronomePanel isHost={false} />)
    expect(screen.queryByTestId('metronome-sync')).toBeNull()
  })
})
