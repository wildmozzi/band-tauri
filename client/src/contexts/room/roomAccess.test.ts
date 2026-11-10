import { describe, expect, it } from 'vitest'
import {
  describeJoinError,
  forgetRoomPassword,
  inviteLink,
  joinErrorFromSignal,
  parseRoomCode,
  recallRoomPassword,
  rememberRoomPassword,
  validateRoomPasswordInput,
} from './roomAccess'

const ID = '3f2a9c1e-8b7d-4e6f-9a0b-1c2d3e4f5a6b'

describe('parseRoomCode', () => {
  it('방 코드(UUID)를 그대로 받는다 (대소문자·공백 무시)', () => {
    expect(parseRoomCode(ID)).toBe(ID)
    expect(parseRoomCode(`  ${ID.toUpperCase()}  `)).toBe(ID)
  })
  it('초대 링크에서 방 id를 꺼낸다', () => {
    expect(parseRoomCode(`https://band.example/rooms/${ID}`)).toBe(ID)
    expect(parseRoomCode(`https://band.example/rooms/${ID}?from=chat#x`)).toBe(ID)
  })
  it('알아볼 수 없으면 null', () => {
    expect(parseRoomCode('')).toBeNull()
    expect(parseRoomCode('hello')).toBeNull()
    expect(parseRoomCode(`https://band.example/posts/${ID}x`)).toBeNull()
    expect(parseRoomCode(`${ID}-extra`)).toBeNull()
  })
})

describe('inviteLink', () => {
  it('웹에서는 현재 origin을 쓴다', () => {
    expect(inviteLink(ID, undefined, 'https://band.example')).toBe(`https://band.example/rooms/${ID}`)
  })
  it('설정된 웹 주소가 우선 (데스크톱 앱 tauri:// 대응)', () => {
    expect(inviteLink(ID, 'https://band.example/', 'tauri://localhost')).toBe(`https://band.example/rooms/${ID}`)
  })
  it('웹 주소를 모르면 방 코드만', () => {
    expect(inviteLink(ID, undefined, 'tauri://localhost')).toBe(ID)
  })
})

describe('validateRoomPasswordInput', () => {
  it('6~64자 (NFC 기준)', () => {
    expect(validateRoomPasswordInput('abcde')).not.toBeNull()
    expect(validateRoomPasswordInput('abcdef')).toBeNull()
    // 한글 NFD(자모 분리)도 NFC 글자 수로 센다
    expect(validateRoomPasswordInput('합주실비번이야'.normalize('NFD'))).toBeNull()
    expect(validateRoomPasswordInput('합주실비번'.normalize('NFD'))).not.toBeNull()
    expect(validateRoomPasswordInput('x'.repeat(64))).toBeNull()
    expect(validateRoomPasswordInput('x'.repeat(65))).not.toBeNull()
  })
})

describe('joinErrorFromSignal / describeJoinError', () => {
  it('비밀번호 관련 서버 에러만 해석한다', () => {
    expect(joinErrorFromSignal({ type: 'error', code: 'perform-not-approved' })).toBeNull()
    expect(joinErrorFromSignal({ type: 'chat', code: 'password-invalid' })).toBeNull()
    expect(joinErrorFromSignal({ type: 'error', code: 'password-invalid', roomId: ID, remainingAttempts: 2 })).toEqual({
      code: 'password-invalid', roomId: ID, remainingAttempts: 2, retryAfterMs: null,
    })
  })
  it('남은 시도 횟수와 대기 시간을 보여준다', () => {
    expect(describeJoinError({ code: 'password-invalid', roomId: ID, remainingAttempts: 2, retryAfterMs: null })).toContain('2회')
    expect(describeJoinError({ code: 'password-rate-limited', roomId: ID, remainingAttempts: null, retryAfterMs: 125_000 })).toContain('3분')
    expect(describeJoinError({ code: 'password-rate-limited', roomId: ID, remainingAttempts: null, retryAfterMs: 1 })).toContain('1분')
    expect(describeJoinError({ code: 'password-required', roomId: null, remainingAttempts: null, retryAfterMs: null })).toContain('비밀번호')
    expect(describeJoinError({ code: 'password-busy', roomId: null, remainingAttempts: null, retryAfterMs: 1000 })).toContain('다시 시도')
    expect(joinErrorFromSignal({ type: 'error', code: 'password-busy', retryAfterMs: 2000 })?.retryAfterMs).toBe(2000)
  })
})

describe('세션 비밀번호 기억', () => {
  const mem = () => {
    const m = new Map<string, string>()
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) }
  }
  it('방별로 저장/조회/삭제', () => {
    const s = mem()
    rememberRoomPassword('a', 'pw-a', s)
    rememberRoomPassword('b', 'pw-b', s)
    expect(recallRoomPassword('a', s)).toBe('pw-a')
    forgetRoomPassword('a', s)
    expect(recallRoomPassword('a', s)).toBeNull()
    expect(recallRoomPassword('b', s)).toBe('pw-b')
  })
  it('저장소가 없거나 예외를 던져도 앱이 멈추지 않는다', () => {
    const broken = { getItem: () => { throw new Error('denied') }, setItem: () => { throw new Error('denied') }, removeItem: () => { throw new Error('denied') } }
    expect(() => rememberRoomPassword('a', 'x', broken)).not.toThrow()
    expect(recallRoomPassword('a', broken)).toBeNull()
    expect(() => forgetRoomPassword('a', broken)).not.toThrow()
    expect(recallRoomPassword('a', null)).toBeNull()
  })
})
