import { describe, expect, it } from 'vitest'
import { initialRoomState, type RoomState } from './roomSessionStore'
import { MAX_BUSY_RETRIES, reduceRoomSignal, type SignalContext } from './signalReducer'
import type { PeerInstrument } from './roomState'

const ctx = (over: Partial<SignalContext> = {}): SignalContext => ({
  selfId: 'me',
  rejoining: false,
  busyRetryCount: 0,
  now: () => 1000,
  makeChatId: (sender, ts) => `${sender}-${ts}`,
  ...over,
})

const inRoom = (over: Partial<RoomState> = {}): RoomState => {
  const base = initialRoomState()
  return { ...base, connection: { ...base.connection, currentRoomId: 'r1', clientId: 'me', peers: ['a'] }, ...over }
}

const guitar: PeerInstrument = { peerId: 'a', instrument: 'guitar', nickname: 'A', isHost: false }

describe('reduceRoomSignal — 채팅', () => {
  it('서버가 붙인 oderId로 메시지를 붙인다', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'chat', oderId: 'a', message: 'hi', nickname: 'A', timestamp: 5 }, ctx())
    expect(r.handled).toBe(true)
    expect(r.patch?.chat).toEqual([{ id: 'a-5', peerId: 'a', nickname: 'A', message: 'hi', timestamp: 5 }])
  })

  it('형식이 틀린 채팅은 처리한 것으로 보고 아무것도 바꾸지 않는다', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'chat', message: 'no sender' }, ctx())
    expect(r).toEqual({ handled: true, patch: null, effects: [] })
  })
})

describe('reduceRoomSignal — 요청', () => {
  it('참여 승인: 요청 상태 갱신 + 엔진 자동 시작 효과(악기가 없으면 요청했던 악기)', () => {
    const s = inRoom()
    s.requests = { ...s.requests, perform: { ...s.requests.perform, myStatus: 'pending', myInstrument: 'bass', requestOutstanding: true } }
    const r = reduceRoomSignal(s, { type: 'perform-request-approved' }, ctx())
    expect(r.patch?.requests?.perform.myStatus).toBe('approved')
    expect(r.effects).toEqual([{ type: 'my-request-approved', instrument: 'bass' }])
  })

  it('악기 변경 승인: 엔진 악기 갱신 후 2초 뒤 초기화, 거절은 3초', () => {
    const ok = reduceRoomSignal(inRoom(), { type: 'instrument-change-approved', newInstrument: 'drums' }, ctx())
    expect(ok.patch?.requests?.myInstrumentChangeStatus).toBe('approved')
    expect(ok.effects).toEqual([
      { type: 'my-instrument-changed', instrument: 'drums' },
      { type: 'reset-instrument-change-after', ms: 2000 },
    ])
    const no = reduceRoomSignal(inRoom(), { type: 'instrument-change-rejected' }, ctx())
    expect(no.effects).toEqual([{ type: 'reset-instrument-change-after', ms: 3000 }])
  })

  it('다른 연주자의 악기 변경은 아는 연주자일 때만 반영', () => {
    const s = inRoom({ roster: { a: guitar } })
    expect(reduceRoomSignal(s, { type: 'participant-instrument-changed', oderId: 'a', instrument: 'bass' }, ctx()).patch?.roster?.a.instrument).toBe('bass')
    expect(reduceRoomSignal(s, { type: 'participant-instrument-changed', oderId: 'z', instrument: 'bass' }, ctx()).patch).toBeNull()
  })

  it('같은 사람의 악기 변경 요청은 한 번만 쌓인다', () => {
    const req = { oderId: 'a', nickname: 'A', currentInstrument: 'guitar', newInstrument: 'bass', timestamp: 1 }
    const first = reduceRoomSignal(inRoom(), { type: 'instrument-change-request-received', request: req }, ctx())
    const s = inRoom({ requests: first.patch!.requests! })
    expect(reduceRoomSignal(s, { type: 'instrument-change-request-received', request: req }, ctx()).patch).toBeNull()
  })
})

describe('reduceRoomSignal — 입장 오류', () => {
  it('비밀번호 틀림: 기억한 비밀번호를 지우고 오류 표시, 엔진은 그대로', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'password-invalid', roomId: 'r1' }, ctx())
    expect(r.effects).toEqual([
      { type: 'forget-password', roomId: 'r1' },
      { type: 'clear-password' },
      { type: 'set-rejoining', value: false },
    ])
    expect(r.patch?.connection?.joinError?.code).toBe('password-invalid')
    expect(r.patch?.connection?.roomJoined).toBe(false)
  })

  it('다른 방의 입장 오류는 무시', () => {
    expect(reduceRoomSignal(inRoom(), { type: 'error', code: 'room-full', roomId: 'other' }, ctx())).toEqual({ handled: true, patch: null, effects: [] })
  })

  it('정원 초과는 엔진·방송을 멈춘다. 재연결 중 거부는 afterReconnect로 표시', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-full' }, ctx({ rejoining: true }))
    expect(r.effects).toContainEqual({ type: 'stop-broadcast' })
    expect(r.effects).toContainEqual({ type: 'stop-session' })
    expect(r.patch?.connection?.joinError?.afterReconnect).toBe(true)
  })

  it('password-busy: 재시도 예약(0.5~10초), 상한에 닿으면 예약하지 않는다', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'password-busy', retryAfterMs: 50 }, ctx())
    expect(r.effects).toEqual([{ type: 'schedule-busy-retry', roomId: 'r1', delayMs: 500 }])
    const capped = reduceRoomSignal(inRoom(), { type: 'error', code: 'password-busy' }, ctx({ busyRetryCount: MAX_BUSY_RETRIES }))
    expect(capped.effects).toEqual([])
  })

  it('처리 순서: 입장 오류 코드가 아닌 error는 참가자 단계의 서버 에러로', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'rate-limited', message: 'x' }, ctx())
    expect(r.effects).toEqual([{ type: 'server-error', code: 'rate-limited', message: 'x' }])
  })
})

describe('reduceRoomSignal — 참가자·WebRTC', () => {
  it('room-state: 피어·연주자 목록을 통째로 바꾸고 입장 완료, 내 항목은 목록에서 뺀다', () => {
    const r = reduceRoomSignal(inRoom(), {
      type: 'room-state',
      isHost: true,
      peerIds: ['me', 'a', 'b'],
      participants: [
        { oderId: 'a', nickname: 'A', instrument: 'guitar', isPerforming: true },
        { oderId: 'b', isPerforming: false },
        { oderId: 'me', instrument: 'bass', isPerforming: true },
      ],
    }, ctx())
    expect(r.patch?.connection).toMatchObject({ peers: ['a', 'b'], isRoomHost: true, roomJoined: true, joinError: null, joinFeedback: '룸 입장 완료 · 동시 연결 3명' })
    expect(Object.keys(r.patch!.roster!)).toEqual(['a'])
    expect(r.effects).toEqual([{ type: 'set-rejoining', value: false }, { type: 'clear-busy-retry' }, { type: 'remember-password', roomId: 'r1' }])
  })

  it('participant-left: 피어·연주자·요청 정리 후 엔진 피어 제거와 관람 수신 닫기', () => {
    const s = inRoom({ roster: { a: guitar } })
    s.requests = { ...s.requests, perform: { ...s.requests.perform, pendingRequests: [{ oderId: 'a', nickname: 'A', instrument: 'bass', timestamp: 1 }] } }
    const r = reduceRoomSignal(s, { type: 'participant-left', oderId: 'a' }, ctx())
    expect(r.patch?.connection?.peers).toEqual([])
    expect(r.patch?.roster).toEqual({})
    expect(r.patch?.requests?.perform.pendingRequests).toEqual([])
    expect(r.effects).toEqual([{ type: 'peer-gone', peerId: 'a' }, { type: 'close-viewer', peerId: 'a' }])
  })

  it('participant-left: 대기 요청·연주 기록이 없던 참가자면 요청·연주자 조각을 건드리지 않는다 (#78 L4)', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'participant-left', oderId: 'a' }, ctx())
    expect(r.patch?.connection?.peers).toEqual([])
    expect(r.patch && 'requests' in r.patch).toBe(false)
    expect(r.patch && 'roster' in r.patch).toBe(false)
    const stranger = reduceRoomSignal(inRoom(), { type: 'participant-left', oderId: 'nobody' }, ctx())
    expect(stranger.patch).toEqual({})
  })

  it('performer-updated: 연주 시작이면 등록 + 엔진에 알림, 중단이면 제거', () => {
    const on = reduceRoomSignal(inRoom(), { type: 'performer-updated', oderId: 'c', isPerforming: true, instrument: 'keys' }, ctx())
    expect(on.patch?.connection?.peers).toEqual(['a', 'c'])
    expect(on.patch?.roster?.c).toEqual({ peerId: 'c', instrument: 'keys', nickname: 'User c', isHost: false })
    expect(on.effects).toEqual([{ type: 'performer-appeared', peerId: 'c' }])
    const off = reduceRoomSignal(inRoom({ roster: { a: guitar } }), { type: 'performer-updated', oderId: 'a', isPerforming: false }, ctx())
    expect(off.patch?.roster).toEqual({})
    expect(off.effects).toEqual([{ type: 'peer-gone', peerId: 'a' }, { type: 'close-viewer', peerId: 'a' }])
  })

  it('offer는 엔진이 쉬고 있을 때(관람자)만 받는다', () => {
    const offer = { type: 'offer', sdp: 'x' }
    expect(reduceRoomSignal(inRoom(), { type: 'offer', from: 'a', offer }, ctx()).effects).toEqual([{ type: 'viewer-offer', from: 'a', offer }])
    const performing = inRoom({ perform: { ...initialRoomState().perform, phase: 'running' } })
    expect(reduceRoomSignal(performing, { type: 'offer', from: 'a', offer }, ctx()).effects).toEqual([])
  })

  it('모르는 피어의 ICE는 피어로 등록하고 넘긴다 (H1)', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'ice-candidate', from: 'z', candidate: { candidate: 'c' } }, ctx())
    expect(r.patch?.connection?.peers).toEqual(['a', 'z'])
    expect(r.effects).toEqual([{ type: 'ice-candidate', from: 'z', candidate: { candidate: 'c' } }])
  })

  it('모르는 메시지는 처리하지 않음', () => {
    expect(reduceRoomSignal(inRoom(), { type: 'something-new' }, ctx()).handled).toBe(false)
  })
})
