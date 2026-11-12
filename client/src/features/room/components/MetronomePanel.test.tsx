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
vi.mock('../state/roomHooks', () => ({ useRoomPerform: () => room, useRoomMetronome: () => room, useRoomTelemetry: () => room }))

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
