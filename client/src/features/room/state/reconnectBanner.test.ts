import { describe, expect, it } from 'vitest'
import { canReenter, reconnectBannerKind, selectReconnectBanner } from './reconnectBanner'
import { IDLE_PERFORM, initialRoomState, RoomSessionStore } from './roomSessionStore'
import type { SignalStatus } from './useSignaling'

const snap = (signalStatus: SignalStatus, currentRoomId: string | null, phase: 'idle' | 'running' = 'idle') => {
  const base = initialRoomState()
  return { connection: { ...base.connection, signalStatus, currentRoomId }, perform: { ...IDLE_PERFORM, phase } }
}

describe('reconnectBannerKind', () => {
  it.each<[SignalStatus, string | null, string]>([
    // 방 밖: 첫 연결 안내는 화면이 전체 화면으로 (띠 없음)
    ['idle', null, 'none'],
    ['connecting', null, 'none'],
    ['error', null, 'none'],
    ['connected', null, 'none'],
    ['gave-up', null, 'none'],
    ['replaced', null, 'none'],
    // 방 안
    ['connected', 'r1', 'none'],
    ['idle', 'r1', 'reconnecting'],
    ['connecting', 'r1', 'reconnecting'],
    ['error', 'r1', 'reconnecting'],
    ['gave-up', 'r1', 'gave-up'],
    ['replaced', 'r1', 'replaced'],
  ])('%s, 방 %s → %s', (status, roomId, expected) => {
    expect(reconnectBannerKind({ signalStatus: status, currentRoomId: roomId })).toBe(expected)
  })
})

describe('canReenter', () => {
  it('스스로 다시 연결하지 않는 끊김(포기·밀려남)에서만 다시 입장할 수 있다', () => {
    const all: SignalStatus[] = ['idle', 'connecting', 'connected', 'error', 'gave-up', 'replaced']
    expect(all.filter(canReenter)).toEqual(['gave-up', 'replaced'])
  })
})

describe('selectReconnectBanner', () => {
  it('재연결 중 엔진이 돌면 오디오 유지, 아니면(웹 관람·엔진 정지) 오디오 유지라고 하지 않는다', () => {
    expect(selectReconnectBanner(snap('connecting', 'r1', 'running'))).toEqual({ kind: 'reconnecting', audioLive: true })
    expect(selectReconnectBanner(snap('connecting', 'r1', 'idle'))).toEqual({ kind: 'reconnecting', audioLive: false })
    expect(selectReconnectBanner(snap('gave-up', 'r1', 'running'))).toEqual({ kind: 'gave-up', audioLive: false })
    expect(selectReconnectBanner(snap('replaced', 'r1', 'running'))).toEqual({ kind: 'replaced', audioLive: false })
    expect(selectReconnectBanner(snap('connected', 'r1', 'running'))).toEqual({ kind: 'none', audioLive: false })
  })

  it('엔진 통계 틱(perform.stats만 바뀜)에는 결과가 얕은 비교로 같다', () => {
    const store = new RoomSessionStore(initialRoomState({
      connection: { ...initialRoomState().connection, signalStatus: 'connecting', currentRoomId: 'r1' },
      perform: { ...IDLE_PERFORM, phase: 'running' },
    }))
    const a = selectReconnectBanner(store.getSnapshot())
    store.update((s) => ({ perform: { ...s.perform, stats: { peers: [], inputLevel: 0.5, outputLevel: 0.1 } as never } }))
    const b = selectReconnectBanner(store.getSnapshot())
    expect(b).toEqual(a)
  })
})
