// @vitest-environment jsdom
// 관람 수신(useViewerAudio)의 경합: 협상 중 새 offer·퇴장, 통계 측정 중 닫힘
import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { RoomSessionStore } from './roomSessionStore'
import { useViewerAudio } from './useViewerAudio'

interface Deferred {
  resolve: () => void
  reject: (e: unknown) => void
}

/** setRemoteDescription·getStats를 테스트가 끝내 주는 가짜 RTCPeerConnection */
class FakePc {
  static all: FakePc[] = []
  connectionState: RTCPeerConnectionState = 'new'
  remoteDescription: unknown = null
  closed = false
  remoteSdp: Deferred | null = null
  stats: { resolve: (r: Map<string, unknown>) => void } | null = null
  ontrack: unknown = null
  onicecandidate: unknown = null
  onconnectionstatechange: (() => void) | null = null
  constructor() {
    FakePc.all.push(this)
  }
  setRemoteDescription(d: unknown) {
    return new Promise<void>((resolve, reject) => {
      this.remoteSdp = {
        resolve: () => {
          this.remoteDescription = d
          resolve()
        },
        reject,
      }
    })
  }
  async addIceCandidate() {}
  async createAnswer() {
    return { type: 'answer', sdp: 'v=0\r\n' }
  }
  async setLocalDescription() {}
  getStats() {
    return new Promise<Map<string, unknown>>((resolve) => {
      this.stats = { resolve }
    })
  }
  close() {
    this.closed = true
  }
}

const offer = { type: 'offer', sdp: 'v=0\r\n' } as RTCSessionDescriptionInit

function setup() {
  const store = new RoomSessionStore()
  const send = vi.fn()
  const hook = renderHook(() => useViewerAudio({ send, isAllowedSender: () => true, store }))
  return { store, send, hook, viewer: () => hook.result.current }
}

beforeEach(() => {
  FakePc.all = []
  vi.stubGlobal('RTCPeerConnection', FakePc)
})
afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe('useViewerAudio', () => {
  it('같은 방송 담당의 새 offer가 먼저 끝나면, 늦게 실패한 옛 협상이 새 연결을 닫거나 실패 안내를 띄우지 않는다', async () => {
    const { store, send, viewer } = setup()
    let first!: Promise<void>
    let second!: Promise<void>
    act(() => {
      first = viewer().handleOffer('a', offer)
    })
    // 재협상(방송 담당 엔진 재시작 등): 옛 연결을 닫고 새로 받는다
    act(() => {
      second = viewer().handleOffer('a', offer)
    })
    const [oldPc, newPc] = FakePc.all
    expect(oldPc.closed).toBe(true)
    await act(async () => {
      newPc.remoteSdp!.resolve()
      await second
    })
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: 'answer', to: 'a' }))
    // 닫힌 옛 연결의 setRemoteDescription이 늦게 실패한다
    await act(async () => {
      oldPc.remoteSdp!.reject(new Error('InvalidStateError: closed'))
      await first
    })
    expect(newPc.closed).toBe(false)
    expect(viewer().hasPeer('a')).toBe(true)
    expect(store.getSnapshot().viewer.error).toBeNull()
    expect(store.getSnapshot().viewer.status).not.toBe('error')
    expect(send.mock.calls.filter(([p]) => p.type === 'answer')).toHaveLength(1)
  })

  it('협상 중 퇴장(closeAll)하면 옛 협상은 answer를 보내지 않는다', async () => {
    const { send, viewer } = setup()
    let first!: Promise<void>
    act(() => {
      first = viewer().handleOffer('a', offer)
    })
    act(() => viewer().closeAll())
    await act(async () => {
      FakePc.all[0].remoteSdp!.resolve()
      await first
    })
    expect(send).not.toHaveBeenCalledWith(expect.objectContaining({ type: 'answer' }))
  })

  it('통계를 기다리는 사이 닫힌 연결의 회선 통계를 다시 넣지 않는다', async () => {
    vi.useFakeTimers()
    const { store, viewer } = setup()
    let p!: Promise<void>
    act(() => {
      p = viewer().handleOffer('a', offer)
    })
    await act(async () => {
      FakePc.all[0].remoteSdp!.resolve()
      await p
    })
    FakePc.all[0].connectionState = 'connected'
    act(() => {
      vi.advanceTimersByTime(2000)
    })
    expect(FakePc.all[0].stats).not.toBeNull()
    act(() => viewer().closeFrom('a'))
    await act(async () => {
      FakePc.all[0].stats!.resolve(new Map())
    })
    expect(store.getSnapshot().viewer.networkStats).toEqual({})
  })
})
