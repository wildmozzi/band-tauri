// @vitest-environment jsdom
// MusicRoom에서 분리한 플레이어 훅: 재생 상태 전이, 스템 로드·리셋, 진행률 콜백, 볼륨·템포
import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const players = vi.hoisted(() => [] as FakePlayer[])
interface FakePlayer {
  progressCb: ((p: number, t: number) => void) | null
  enabled: Record<string, boolean>
  play: ReturnType<typeof vi.fn>
  pause: ReturnType<typeof vi.fn>
  stop: ReturnType<typeof vi.fn>
  dispose: ReturnType<typeof vi.fn>
  loadStems: ReturnType<typeof vi.fn>
  setMasterVolume: ReturnType<typeof vi.fn>
  setTempo: ReturnType<typeof vi.fn>
}
vi.mock('./MultiTrackPlayer', () => ({
  MultiTrackPlayer: class {
    progressCb: ((p: number, t: number) => void) | null = null
    enabled: Record<string, boolean> = {}
    play = vi.fn(async () => {})
    pause = vi.fn()
    stop = vi.fn()
    dispose = vi.fn()
    loadStems = vi.fn(async () => {})
    setMasterVolume = vi.fn()
    setStemVolume = vi.fn()
    setTempo = vi.fn()
    setPitch = vi.fn()
    seekToProgress = vi.fn()
    getState = () => ({ duration: 42 })
    onProgress(cb: (p: number, t: number) => void) { this.progressCb = cb }
    toggleStem(name: string) { this.enabled[name] = this.enabled[name] === false }
    isStemEnabled(name: string) { return this.enabled[name] !== false }
    constructor() { players.push(this as unknown as FakePlayer) }
  },
}))

import { useStemPlayer } from './useStemPlayer'

const change = (value: string) => ({ target: { value } }) as unknown as React.ChangeEvent<HTMLInputElement>

beforeEach(() => {
  vi.spyOn(window, 'alert').mockImplementation(() => {})
})
afterEach(() => {
  vi.restoreAllMocks()
  players.length = 0
})

describe('useStemPlayer', () => {
  it('스템이 없으면 재생 대신 안내', async () => {
    const { result } = renderHook(() => useStemPlayer(false))
    await act(() => result.current.handlePlay())
    expect(window.alert).toHaveBeenCalledWith('먼저 MP3 파일을 업로드하고 분리해주세요.')
    expect(players[0].play).not.toHaveBeenCalled()
  })

  it('재생 → 일시정지 → 정지 상태 전이, 진행률 콜백 반영', async () => {
    const { result } = renderHook(() => useStemPlayer(true))
    await act(() => result.current.handlePlay())
    expect(result.current.isPlaying).toBe(true)
    act(() => players[0].progressCb!(0.5, 21))
    expect(result.current.progress).toBe(0.5)
    expect(result.current.currentTime).toBe(21)
    act(() => result.current.handlePause())
    expect(result.current).toMatchObject({ isPlaying: false, isPaused: true })
    act(() => result.current.handleStop())
    expect(result.current).toMatchObject({ isPlaying: false, isPaused: false, progress: 0, currentTime: 0 })
  })

  it('loadStems는 길이를 읽고, resetPlayer는 새 플레이어로 바꾸고 상태를 비운다', async () => {
    const { result } = renderHook(() => useStemPlayer(true))
    await act(() => result.current.loadStems({ vocals: 'u' }))
    expect(players[0].loadStems).toHaveBeenCalledWith({ vocals: 'u' })
    expect(result.current.duration).toBe(42)
    act(() => result.current.setStemStates({ vocals: true }))
    act(() => result.current.resetPlayer())
    expect(players[0].stop).toHaveBeenCalled()
    expect(players[0].dispose).toHaveBeenCalled()
    expect(players).toHaveLength(2)
    expect(result.current).toMatchObject({ duration: 0, stemStates: {}, isPlaying: false })
    // 새 플레이어의 진행률이 반영된다
    act(() => players[1].progressCb!(0.25, 3))
    expect(result.current.progress).toBe(0.25)
  })

  it('스템 토글·마스터 볼륨·템포를 플레이어에 전달', () => {
    const { result } = renderHook(() => useStemPlayer(true))
    act(() => result.current.handleStemToggle('bass'))
    expect(result.current.stemStates).toEqual({ bass: false })
    act(() => result.current.handleMasterVolumeChange(change('40')))
    expect(result.current.masterVolume).toBe(40)
    expect(players[0].setMasterVolume).toHaveBeenCalledWith(0.4)
    act(() => result.current.handleTempoChange(change('1.25')))
    expect(players[0].setTempo).toHaveBeenCalledWith(1.25)
  })

  it('언마운트하면 플레이어를 정리한다', () => {
    const { unmount } = renderHook(() => useStemPlayer(true))
    unmount()
    expect(players[0].dispose).toHaveBeenCalled()
  })
})
