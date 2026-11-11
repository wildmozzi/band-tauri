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
  engineRecordSetTrackNames: vi.fn(),
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
    api.engineRecordSetTrackNames.mockResolvedValue(undefined)
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
  it('시작 응답 직후(다음 통계 폴링 전)에도 녹음 중으로 보인다', async () => {
    render(<NativeRecordingControl peers={[]} />)
    fireEvent.click(screen.getByRole('button', { name: /멀티트랙 녹음/ }))
    const btn = await screen.findByRole('button', { name: /녹음 정지/ })
    expect((btn as HTMLButtonElement).disabled).toBe(false)
    expect(screen.getByText('🔴 1:05 · 37.0 MB · 3트랙')).toBeTruthy()
  })

  it('정지 뒤 늦게 온 폴링 응답(같은 세션)은 무시하고 정지 요약을 덮어쓰지 않는다', async () => {
    room.engineStats = { recording: live }
    api.engineRecordStop.mockResolvedValue({ ...live, channels: 2, startedAtMs: 0, tracks: [], error: null })
    const { rerender } = render(<NativeRecordingControl peers={[]} />)
    fireEvent.click(screen.getByRole('button', { name: /녹음 정지/ }))
    expect(await screen.findByText(/저장됨 · 0트랙/)).toBeTruthy()
    // 정지 전에 출발한 폴링이 정지 뒤 도착
    room.engineStats = { recording: { ...live, durationSec: 66 } }
    rerender(<NativeRecordingControl peers={[]} />)
    expect(screen.getByRole('button', { name: /멀티트랙 녹음/ })).toBeTruthy()
    room.engineStats = { recording: null }
    rerender(<NativeRecordingControl peers={[]} />)
    expect(screen.getByText(/저장됨 · 0트랙/)).toBeTruthy()
    expect(screen.queryByText(/엔진 정지/)).toBeNull()
  })

  it('녹음 중에 들어온 연주자 이름을 엔진에 더한다', async () => {
    room.engineStats = { recording: live }
    const { rerender } = render(<NativeRecordingControl peers={[{ peerId: 'u-1', name: '앨리스' }]} />)
    await vi.waitFor(() => expect(api.engineRecordSetTrackNames).toHaveBeenCalledWith({ 'u-1': '앨리스' }))
    rerender(<NativeRecordingControl peers={[{ peerId: 'u-1', name: '앨리스' }]} />)
    rerender(<NativeRecordingControl peers={[{ peerId: 'u-1', name: '앨리스' }, { peerId: 'u-2', name: '밥' }]} />)
    await vi.waitFor(() => expect(api.engineRecordSetTrackNames).toHaveBeenLastCalledWith({ 'u-1': '앨리스', 'u-2': '밥' }))
    expect(api.engineRecordSetTrackNames).toHaveBeenCalledTimes(2)
  })

  it('FLAC을 고르면 그 형식으로 시작하고 선택을 기억하며, 녹음 중에는 바꿀 수 없다(P2-4a)', async () => {
    localStorage.removeItem('band.recordFormat')
    const { unmount } = render(<NativeRecordingControl peers={[]} />)
    const select = screen.getByRole('combobox', { name: '녹음 파일 형식' }) as HTMLSelectElement
    expect(select.value).toBe('wav24')
    fireEvent.change(select, { target: { value: 'flac' } })
    fireEvent.click(screen.getByRole('button', { name: /멀티트랙 녹음/ }))
    await vi.waitFor(() => expect(api.engineRecordStart).toHaveBeenCalledTimes(1))
    expect(api.engineRecordStart.mock.calls[0][0]).toMatchObject({ format: 'flac' })
    unmount()
    room.engineStats = { recording: { ...live, format: 'flac' } }
    render(<NativeRecordingControl peers={[]} />)
    const again = screen.getByRole('combobox', { name: '녹음 파일 형식' }) as HTMLSelectElement
    expect(again.value).toBe('flac')
    expect(again.disabled).toBe(true)
    localStorage.removeItem('band.recordFormat')
  })

  it('정지 요약에 끊김 구간 수를 보인다(P2-4a)', async () => {
    room.engineStats = { recording: live }
    api.engineRecordStop.mockResolvedValue({ ...live, channels: 2, startedAtMs: 0, tracks: [], error: null, markers: 3, markersFile: 'markers.json' })
    render(<NativeRecordingControl peers={[]} />)
    fireEvent.click(screen.getByRole('button', { name: /녹음 정지/ }))
    expect(await screen.findByText(/끊김 구간 3곳\(markers\.json\)/)).toBeTruthy()
  })

  it('녹음 중이 아니면 이름을 보내지 않는다', async () => {
    render(<NativeRecordingControl peers={[{ peerId: 'u-1', name: '앨리스' }]} />)
    expect(await screen.findByText(/Band Recordings/)).toBeTruthy()
    expect(api.engineRecordSetTrackNames).not.toHaveBeenCalled()
  })
})
