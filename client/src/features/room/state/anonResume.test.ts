import { describe, expect, it } from 'vitest'
import { anonResumeAuth } from './anonResume'

describe('익명 관람자 재개 토큰 (P2-18c)', () => {
  it('이전 clientId로 받은 토큰과 지금 방이 있으면 재개 auth를 만든다', () => {
    expect(anonResumeAuth('c1', 'room-1', { clientId: 'c1', token: 't1' }))
      .toEqual({ type: 'auth', resumeClientId: 'c1', resumeToken: 't1', resumeRoomId: 'room-1' })
  })

  it('다른 clientId의 토큰·방 없음·clientId 없음·토큰 없음이면 만들지 않는다', () => {
    expect(anonResumeAuth('c2', 'room-1', { clientId: 'c1', token: 't1' })).toBeNull()
    expect(anonResumeAuth('c1', null, { clientId: 'c1', token: 't1' })).toBeNull()
    expect(anonResumeAuth(null, 'room-1', { clientId: 'c1', token: 't1' })).toBeNull()
    expect(anonResumeAuth('c1', 'room-1', null)).toBeNull()
  })
})
