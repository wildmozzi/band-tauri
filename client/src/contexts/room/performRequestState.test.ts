import { describe, expect, it } from 'vitest'
import {
  initialPerformRequestState as init,
  isPerformRequestSignal,
  performRequestReducer as reduce,
  type PerformRequest,
  type PerformRequestAction,
  type PerformRequestState,
} from './performRequestState'

const sig = (payload: { type: string; [k: string]: unknown }): PerformRequestAction => ({ type: 'signal', payload })
const req = (id: string, instrument = 'guitar'): PerformRequest => ({ oderId: id, nickname: id, instrument, timestamp: 1 })
const run = (...actions: PerformRequestAction[]): PerformRequestState => actions.reduce(reduce, init)

describe('요청자 흐름', () => {
  it('요청 → 대기(방장 접속 여부) → 승인', () => {
    let s = run({ type: 'requested', instrument: 'drums' })
    expect(s.myStatus).toBe('none')
    expect(s.myInstrument).toBe('drums')
    s = reduce(s, sig({ type: 'perform-request-sent', instrument: 'drums', hostOnline: false }))
    expect(s).toMatchObject({ myStatus: 'pending', hostOnline: false })
    s = reduce(s, sig({ type: 'perform-request-approved', instrument: 'drums' }))
    expect(s).toMatchObject({ myStatus: 'approved', myInstrument: 'drums', myRejectReason: null })
    s = reduce(s, { type: 'started' })
    expect(s).toMatchObject({ myStatus: 'none', myInstrument: null, hostOnline: null })
  })

  it('거절 사유를 보관하고, 다시 요청하면 지운다', () => {
    let s = run(
      { type: 'requested', instrument: 'bass' },
      sig({ type: 'perform-request-sent', instrument: 'bass', hostOnline: true }),
      sig({ type: 'perform-request-rejected', reason: '  자리가 찼습니다 ' }),
    )
    expect(s).toMatchObject({ myStatus: 'rejected', myRejectReason: '자리가 찼습니다' })
    s = reduce(s, { type: 'cancelled' })
    expect(s).toMatchObject({ myStatus: 'none', myRejectReason: null })
    s = reduce(reduce(s, { type: 'requested', instrument: 'keyboard' }), sig({ type: 'perform-request-sent' }))
    expect(s).toMatchObject({ myStatus: 'pending', myInstrument: 'keyboard', myRejectReason: null, hostOnline: null })
  })

  it('취소 후 늦게 온 perform-request-sent는 무시 (리뷰 L6)', () => {
    const s = run({ type: 'requested', instrument: 'guitar' }, { type: 'cancelled' }, sig({ type: 'perform-request-sent', instrument: 'guitar' }))
    expect(s.myStatus).toBe('none')
    // 요청하지 않았는데 온 sent도 무시
    expect(run(sig({ type: 'perform-request-sent' })).myStatus).toBe('none')
  })

  it('빈 거절 사유는 null', () => {
    expect(run(sig({ type: 'perform-request-rejected', reason: '   ' })).myRejectReason).toBeNull()
    expect(run(sig({ type: 'perform-request-rejected' })).myRejectReason).toBeNull()
  })

  it('서버가 요청을 거부하면(로그인 필요) 기본 상태로', () => {
    const s = run({ type: 'requested', instrument: 'guitar' }, sig({ type: 'request-perform-error', code: 'login-required' }))
    expect(s).toMatchObject({ myStatus: 'none', myInstrument: null })
  })

  it('승인 메시지에 악기가 없으면 요청했던 악기를 유지', () => {
    const s = run({ type: 'requested', instrument: 'vocal' }, sig({ type: 'perform-request-approved' }))
    expect(s.myInstrument).toBe('vocal')
  })
})

describe('방장 흐름', () => {
  it('새 요청은 목록과 알림에 들어가고, 같은 요청은 중복되지 않는다', () => {
    let s = run(sig({ type: 'perform-request-received', request: req('a') }))
    expect(s.pendingRequests).toHaveLength(1)
    expect(s.notice?.oderId).toBe('a')
    s = reduce(s, sig({ type: 'perform-request-received', request: req('a') }))
    expect(s.pendingRequests).toHaveLength(1)
    s = reduce(s, sig({ type: 'perform-request-received', request: req('b', 'drums') }))
    expect(s.pendingRequests.map((r) => r.oderId)).toEqual(['a', 'b'])
    expect(s.notice?.oderId).toBe('b')
  })

  it('잘못된 요청 형식은 무시', () => {
    expect(run(sig({ type: 'perform-request-received', request: { nickname: 'x' } }))).toBe(init)
  })

  it('승인/거절·취소·퇴장 시 목록과 알림에서 뺀다', () => {
    const base = run(
      sig({ type: 'perform-request-received', request: req('a') }),
      sig({ type: 'perform-request-received', request: req('b') }),
      sig({ type: 'perform-request-received', request: req('c') }),
    )
    let s = reduce(base, { type: 'resolved', targetId: 'c' })
    expect(s.notice).toBeNull()
    expect(s.pendingRequests.map((r) => r.oderId)).toEqual(['a', 'b'])
    s = reduce(s, sig({ type: 'perform-request-cancelled', oderId: 'a' }))
    s = reduce(s, { type: 'peer-left', peerId: 'b' })
    expect(s.pendingRequests).toEqual([])
    // 해당 없는 제거는 상태를 바꾸지 않는다
    expect(reduce(s, { type: 'peer-left', peerId: 'zzz' })).toBe(s)
  })

  it('알림 닫기는 목록을 유지한다', () => {
    const s = reduce(run(sig({ type: 'perform-request-received', request: req('a') })), { type: 'dismiss-notice' })
    expect(s.notice).toBeNull()
    expect(s.pendingRequests).toHaveLength(1)
  })
})

describe('공통', () => {
  it('reset은 모두 초기화', () => {
    const s = run(sig({ type: 'perform-request-received', request: req('a') }), { type: 'requested', instrument: 'bass' }, sig({ type: 'perform-request-sent', hostOnline: true }))
    expect(reduce(s, { type: 'reset' })).toEqual(init)
  })

  it('isPerformRequestSignal: 연주 참여 요청 메시지만', () => {
    expect(isPerformRequestSignal('perform-request-approved')).toBe(true)
    expect(isPerformRequestSignal('request-perform-error')).toBe(true)
    expect(isPerformRequestSignal('instrument-change-approved')).toBe(false)
    expect(isPerformRequestSignal('error')).toBe(false)
  })
})
