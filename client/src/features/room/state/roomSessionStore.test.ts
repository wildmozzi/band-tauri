import { describe, expect, it, vi } from 'vitest'
import type { EngineStats } from '../../../native/engineApi'
import { initialRoomState, RoomSessionStore } from './roomSessionStore'

const stats = (over: Partial<EngineStats> = {}) => ({ peers: [], inputLevel: 0.5, outputLevel: 0.25, ...over }) as unknown as EngineStats

describe('RoomSessionStore', () => {
  it('바뀐 것이 없으면 알리지도 스냅샷을 바꾸지도 않는다', () => {
    const store = new RoomSessionStore()
    const listener = vi.fn()
    store.subscribe(listener)
    const before = store.getSnapshot()
    store.patch('connection', { peers: before.connection.peers, joinFeedback: '' })
    store.update(null)
    expect(listener).not.toHaveBeenCalled()
    expect(store.getSnapshot()).toBe(before)
  })

  it('바뀐 슬라이스만 새 참조가 되고, 입력이 같은 파생 값은 같은 참조를 유지한다', () => {
    const store = new RoomSessionStore()
    const a = store.getSnapshot()
    store.patch('levels', { audioLevels: { x: 1 }, masterLevel: 3 })
    const b = store.getSnapshot()
    expect(b.levels).not.toBe(a.levels)
    expect(b.connection).toBe(a.connection)
    expect(b.derived.peerInstruments).toBe(a.derived.peerInstruments)
    expect(b.derived.viewers).toBe(a.derived.viewers)
    expect(b.derived.audioLevels).toEqual({ x: 1 })
  })

  it('연주 중이면 내 항목을 연주자 목록에 합치고 방송 담당·관람자를 계산한다', () => {
    const base = initialRoomState()
    const store = new RoomSessionStore({ ...base, connection: { ...base.connection, clientId: 'me', peers: ['v1', 'p1'], nickname: '나', isRoomHost: true } })
    store.update({ roster: { p1: { peerId: 'p1', instrument: 'bass', nickname: 'P', isHost: false } } })
    store.update({ perform: { ...base.perform, phase: 'running', instrument: 'guitar', stats: stats() } })
    const d = store.getSnapshot().derived
    expect(Object.keys(d.peerInstruments).sort()).toEqual(['me', 'p1'])
    expect(d.broadcasterId).toBe('me') // 방장 연주자가 방송 담당
    expect(d.isBroadcaster).toBe(true)
    expect(d.viewers).toEqual(['v1'])
    expect(d.rtcStatus).toBe('live')
    expect(d.audioLevels).toEqual({ me: 50 })
    expect(d.masterLevel).toBe(25)
  })

  it('엔진 통계만 바뀌면 연주자 목록 참조는 그대로', () => {
    const base = initialRoomState()
    const store = new RoomSessionStore({ ...base, connection: { ...base.connection, clientId: 'me' } })
    store.update({ perform: { ...base.perform, phase: 'running', instrument: 'guitar', stats: stats() } })
    const before = store.getSnapshot().derived
    store.update({ perform: { ...store.getSnapshot().perform, stats: stats({ inputLevel: 0.9 }) } })
    const after = store.getSnapshot().derived
    expect(after.peerInstruments).toBe(before.peerInstruments)
    expect(after.viewers).toBe(before.viewers)
    expect(after.peerEngineStats).not.toBe(before.peerEngineStats)
  })

  it('표시용 연결 상태: 시작 중 → connecting, 안내 오류 → error, 아니면 관람 수신 상태', () => {
    const base = initialRoomState()
    const store = new RoomSessionStore()
    store.patch('viewer', { status: 'live' })
    expect(store.getSnapshot().derived.rtcStatus).toBe('live')
    store.patch('local', { localError: '실패' })
    expect(store.getSnapshot().derived).toMatchObject({ rtcStatus: 'error', rtcError: '실패' })
    store.update({ perform: { ...base.perform, phase: 'starting' } })
    expect(store.getSnapshot().derived.rtcStatus).toBe('connecting')
  })

  it('fieldSetter는 setState처럼 이전 값 함수도 받는다', () => {
    const store = new RoomSessionStore()
    const setRecordings = store.fieldSetter('recording', 'recordingDuration')
    setRecordings(3)
    setRecordings((d) => d + 1)
    expect(store.getSnapshot().recording.recordingDuration).toBe(4)
  })
})
