import { describe, expect, it } from 'vitest'
import { initialRoomState, type RoomState } from './roomSessionStore'
import { DEMOTE_FAILED_MESSAGE, DEMOTED_MESSAGE, LOST_PERFORMER_MESSAGE, MAX_BUSY_RETRIES, reduceRoomSignal, type SignalContext } from './signalReducer'
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

  it('관람자로 내려짐(P2-9): 내 요청·악기 변경 상태를 비우고 방송 정지 + 알림 없는 엔진 정지·안내', () => {
    const s = inRoom({ perform: { ...initialRoomState().perform, phase: 'running' } })
    s.requests = {
      ...s.requests,
      perform: { ...s.requests.perform, myStatus: 'approved', myInstrument: 'bass', requestOutstanding: true },
      myInstrumentChangeStatus: 'pending',
      myNewInstrument: 'keys',
    }
    const r = reduceRoomSignal(s, { type: 'demoted-to-viewer' }, ctx())
    expect(r.handled).toBe(true)
    expect(r.patch?.requests?.perform).toMatchObject({ myStatus: 'none', myInstrument: null, requestOutstanding: false })
    expect(r.patch?.requests?.myInstrumentChangeStatus).toBe('none')
    expect(r.patch?.requests?.myNewInstrument).toBeNull()
    expect(r.effects).toEqual([{ type: 'stop-broadcast' }, { type: 'demoted', message: DEMOTED_MESSAGE }])
  })

  it('관람자로 내려짐: 엔진이 이미 멈춰 있으면(중복·늦은 알림) 새로 보낸 참여 요청을 지우지 않고 아무것도 하지 않는다', () => {
    const s = inRoom()
    s.requests = { ...s.requests, perform: { ...s.requests.perform, myStatus: 'pending', myInstrument: 'bass', requestOutstanding: true } }
    expect(reduceRoomSignal(s, { type: 'demoted-to-viewer' }, ctx())).toEqual({ handled: true, patch: null, effects: [] })
  })

  it('방장: demote-not-performer면 패널 안내를 남긴다 (#180 L-4)', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'demote-not-performer', message: 'x' }, ctx())
    expect(r.patch?.requests?.hostActionError).toBe(DEMOTE_FAILED_MESSAGE)
    expect(reduceRoomSignal(inRoom(), { type: 'error', code: 'login-required', message: 'x' }, ctx()).patch).toBeNull()
  })

  it('다른 사람이 내려지면 performer-updated(isPerforming=false)로 연주자 목록에서 빠지고 엔진 피어도 정리', () => {
    const r = reduceRoomSignal(inRoom({ roster: { a: guitar } }), { type: 'performer-updated', oderId: 'a', from: 'a', nickname: 'A', isPerforming: false }, ctx())
    expect(r.patch?.roster).toEqual({})
    expect(r.effects).toEqual([{ type: 'peer-gone', peerId: 'a' }, { type: 'close-viewer', peerId: 'a' }])
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

  it('방 조회 실패(room-unavailable, #189 리뷰 H-1): 재연결 중이면 화면·엔진을 그대로 두고 조용히 재시도한다', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-unavailable', roomId: 'r1', retryAfterMs: 2000 }, ctx({ rejoining: true, random: () => 0.5 }))
    expect(r.patch).toBeNull()
    expect(r.effects).toEqual([{ type: 'schedule-busy-retry', roomId: 'r1', delayMs: 2000 }])
  })

  it('room-unavailable: 첫 입장이면 안내 + 재시도, 재시도를 다 쓰면 안내만. 어느 경우도 엔진을 멈추지 않는다', () => {
    const first = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-unavailable', roomId: 'r1', retryAfterMs: 2000 }, ctx({ random: () => 0.5 }))
    expect(first.patch?.connection?.joinError?.code).toBe('room-unavailable')
    expect(first.effects).toContainEqual({ type: 'schedule-busy-retry', roomId: 'r1', delayMs: 2000 })
    const spent = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-unavailable', roomId: 'r1' }, ctx({ rejoining: true, busyRetryCount: MAX_BUSY_RETRIES }))
    expect(spent.patch?.connection?.joinError?.code).toBe('room-unavailable')
    for (const r of [first, spent]) {
      expect(r.effects).not.toContainEqual({ type: 'stop-session' })
      expect(r.effects).not.toContainEqual({ type: 'stop-broadcast' })
      expect(r.effects.some((e) => e.type === 'keep-viewers')).toBe(false) // 일시 오류: 유지한 관람 수신은 그대로
    }
  })

  it('room-unavailable 재시도 간격은 ±50% 흩는다(재배포 직후 동시 재입장 몰림 방지, #189 리뷰 L-5). password-busy는 그대로', () => {
    const delay = (code: string, random: number) => {
      const r = reduceRoomSignal(inRoom(), { type: 'error', code, roomId: 'r1', retryAfterMs: 2000 }, ctx({ random: () => random }))
      return (r.effects.find((e) => e.type === 'schedule-busy-retry') as { delayMs: number }).delayMs
    }
    expect(delay('room-unavailable', 0)).toBe(1000)
    expect(delay('room-unavailable', 0.5)).toBe(2000)
    expect(delay('room-unavailable', 0.999)).toBe(2998)
    expect(delay('password-busy', 0)).toBe(2000)
  })

  it('자동 재시도를 다 쓰면 joinError.autoRetry=false — 수동 재시도 안내 (#189 리뷰 L-6)', () => {
    const retrying = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-unavailable', roomId: 'r1' }, ctx({ random: () => 0.5 }))
    expect(retrying.patch?.connection?.joinError?.autoRetry).toBe(true)
    const spent = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-unavailable', roomId: 'r1' }, ctx({ busyRetryCount: MAX_BUSY_RETRIES }))
    expect(spent.patch?.connection?.joinError?.autoRetry).toBe(false)
    const notRetryable = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-full', roomId: 'r1' }, ctx())
    expect(notRetryable.patch?.connection?.joinError).not.toHaveProperty('autoRetry')
  })

  it('방 없음(room-not-found)은 재연결 중이어도 엔진·방송을 멈춘다(보던 방이 종료됨)', () => {
    const r = reduceRoomSignal(inRoom(), { type: 'error', code: 'room-not-found', message: 'x' }, ctx({ rejoining: true }))
    expect(r.effects).toContainEqual({ type: 'stop-session' })
    // 재개로 유지한 관람 수신도 닫는다 (#192 리뷰 M-1)
    expect(r.effects).toContainEqual({ type: 'keep-viewers', peerIds: [] })
    expect(r.patch?.connection?.joinError?.code).toBe('room-not-found')
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
    expect(r.effects).toEqual([{ type: 'set-rejoining', value: false }, { type: 'clear-busy-retry' }, { type: 'keep-viewers', peerIds: ['a'] }, { type: 'remember-password', roomId: 'r1' }])
  })

  it('room-state: 관람 수신은 지금 연주 중인 발신자 것만 남긴다(재개로 유지한 연결의 연주자가 그새 내려간 경우)', () => {
    const r = reduceRoomSignal(inRoom({ roster: { a: guitar } }), { type: 'room-state', peerIds: ['a'], participants: [{ oderId: 'a', isPerforming: false }] }, ctx())
    expect(r.effects).toContainEqual({ type: 'keep-viewers', peerIds: [] })
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

describe('reduceRoomSignal — 재개 뒤 내 연주 상태 맞추기 (P2-9, #180 M-1)', () => {
  const running = () => {
    const s = inRoom({ perform: { ...initialRoomState().perform, phase: 'running' } })
    s.requests = { ...s.requests, perform: { ...s.requests.perform, myStatus: 'approved', myInstrument: 'bass' } }
    return s
  }
  const roomState = (isPerforming: boolean) => ({ type: 'room-state', peerIds: ['a'], participants: [], isHost: false, isPerforming })

  it('내리기 알림이 유실된 채 재개: room-state.isPerforming=false면 방송·엔진을 멈추고 안내, 내 요청 상태도 비운다', () => {
    const r = reduceRoomSignal(running(), roomState(false), ctx({ verifySelfPerforming: true }))
    expect(r.effects).toContainEqual({ type: 'stop-broadcast' })
    expect(r.effects).toContainEqual({ type: 'demoted', message: LOST_PERFORMER_MESSAGE })
    expect(r.patch?.requests?.perform.myStatus).toBe('none')
  })

  it('서버도 연주 중이라고 하면 그대로', () => {
    const r = reduceRoomSignal(running(), roomState(true), ctx({ verifySelfPerforming: true }))
    expect(r.effects.some((e) => e.type === 'demoted')).toBe(false)
    expect(r.patch?.requests).toBeUndefined()
  })

  it('재개가 아닌 입장의 room-state는 보지 않는다(막 다시 시작한 엔진의 start-performing보다 먼저 처리됐을 수 있다)', () => {
    const r = reduceRoomSignal(running(), roomState(false), ctx())
    expect(r.effects.some((e) => e.type === 'demoted')).toBe(false)
  })

  it('isPerforming이 없는 옛 서버 room-state는 보지 않는다', () => {
    const { isPerforming: _omit, ...old } = roomState(false)
    void _omit
    const r = reduceRoomSignal(running(), old, ctx({ verifySelfPerforming: true }))
    expect(r.effects.some((e) => e.type === 'demoted')).toBe(false)
  })
})
