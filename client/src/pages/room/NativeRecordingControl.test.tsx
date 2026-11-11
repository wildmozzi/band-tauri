// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { EngineStats, RecordStats } from '../../native/engineApi'

const room = vi.hoisted(() => ({ engineStats: null as Partial<EngineStats> | null, isPerforming: true }))
const api = vi.hoisted(() => ({
  engineRecordFolder: vi.fn(),
  engineRecordPickFolder: vi.fn(),
  engineRecordStart: vi.fn(),
  engineRecordStop: vi.fn(),
}))

vi.mock('../../contexts/RoomContext', () => ({ useRoom: () => room }))
vi.mock('../../native/engineApi', () => ({
  ...api,
  engineErrorMessage: (e: unknown) => String(e),
}))

import { NativeRecordingControl } from './NativeRecordingControl'

const live: RecordStats = {
  folder: 'C:\\Music\\Band Recordings\\2026-11-11 21.00.00 방',
  format: 'wav24',
  sampleRate: 48000,
  durationSec: 65,
  bytes: 37_000_000,
  tracks: 3,
  droppedFrames: 0,
  error: null,
}

describe('NativeRecordingControl', () => {
  beforeEach(() => {
    room.engineStats = { recording: null }
    room.isPerforming = true
    api.engineRecordFolder.mockResolvedValue('C:\\Music\\Band Recordings')
    api.engineRecordStart.mockResolvedValue(live)
  })
  afterEach(() => {
    cleanup()
    vi.clearAllMocks()
  })

  it('시작하면 연주자 이름·믹스 옵션을 엔진에 넘긴다', async () => {
    render(<NativeRecordingControl peers={[{ peerId: 'u-1', name: '앨리스' }]} roomTitle="방" />)
    expect(await screen.findByText(/Band Recordings/)).toBeTruthy()
    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: /멀티트랙 녹음/ }))
    await vi.waitFor(() => expect(api.engineRecordStart).toHaveBeenCalledTimes(1))
    const opts = api.engineRecordStart.mock.calls[0][0]
    expect(opts).toMatchObject({ includeMix: true, localName: '나', trackNames: { 'u-1': '앨리스' }, format: 'wav24' })
    expect(opts.name).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2} 방$/)
  })

  it('녹음 중에는 시간·용량·트랙과 버림 경고를 보이고, 정지하면 요약을 보인다', async () => {
    room.engineStats = { recording: { ...live, droppedFrames: 2400 } }
    api.engineRecordStop.mockResolvedValue({ ...live, channels: 2, startedAtMs: 0, tracks: [], error: null })
    const { rerender } = render(<NativeRecordingControl peers={[]} />)
    expect(screen.getByText('🔴 1:05 · 37.0 MB · 3트랙')).toBeTruthy()
    expect(screen.getByRole('alert').textContent).toContain('50ms')
    expect((screen.getByRole('checkbox') as HTMLInputElement).disabled).toBe(true)
    fireEvent.click(screen.getByRole('button', { name: /녹음 정지/ }))
    await vi.waitFor(() => expect(api.engineRecordStop).toHaveBeenCalledTimes(1))
    room.engineStats = { recording: null }
    rerender(<NativeRecordingControl peers={[]} />)
    expect(await screen.findByText(/저장됨 · 0트랙 · 1:05/)).toBeTruthy()
  })

  it('엔진이 꺼져 있으면 시작할 수 없고, 엔진 정지로 끝난 녹음을 알린다', async () => {
    room.engineStats = { recording: live }
    const { rerender } = render(<NativeRecordingControl peers={[]} />)
    room.engineStats = { recording: null }
    room.isPerforming = false
    rerender(<NativeRecordingControl peers={[]} />)
    expect(await screen.findByText(/엔진 정지/)).toBeTruthy()
    expect((screen.getByRole('button', { name: /멀티트랙 녹음/ }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('시작 실패를 보여 준다', async () => {
    api.engineRecordStart.mockRejectedValue('이미 녹음 중입니다')
    render(<NativeRecordingControl peers={[]} />)
    fireEvent.click(screen.getByRole('button', { name: /멀티트랙 녹음/ }))
    expect((await screen.findByRole('alert')).textContent).toBe('녹음을 시작할 수 없습니다: 이미 녹음 중입니다')
  })
})
