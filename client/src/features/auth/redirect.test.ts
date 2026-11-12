import { describe, expect, it } from 'vitest'
import { safeRedirectPath } from './redirect'

const ORIGIN = 'https://app.example'
const safe = (from: unknown) => safeRedirectPath(from, '/rooms', ORIGIN)

describe('safeRedirectPath', () => {
  it('앱 안 경로는 그대로 (쿼리·해시 포함)', () => {
    expect(safe('/payment?plan=standard')).toBe('/payment?plan=standard')
    expect(safe('/profile')).toBe('/profile')
    expect(safe('/community/p1#comments')).toBe('/community/p1#comments')
  })

  it('없으면 기본 복귀 경로(/rooms)', () => {
    expect(safe(undefined)).toBe('/rooms')
    expect(safe(42)).toBe('/rooms')
    expect(safe('')).toBe('/rooms')
  })

  it('외부 주소·프로토콜 상대·스킴은 거부', () => {
    expect(safe('https://evil.example')).toBe('/rooms')
    expect(safe('//evil.example')).toBe('/rooms')
    expect(safe('javascript:alert(1)')).toBe('/rooms')
    expect(safe('rooms')).toBe('/rooms')
  })

  it('백슬래시·제어 문자 우회를 거부 (브라우저 URL 파서가 지우거나 /로 바꾸는 문자)', () => {
    expect(safe('/\\evil.example')).toBe('/rooms')
    expect(safe('/\\/evil.example')).toBe('/rooms')
    expect(safe('/\t/evil.example')).toBe('/rooms')
    expect(safe('/\n/evil')).toBe('/rooms')
    expect(safe('/\r/evil')).toBe('/rooms')
    expect(safe('/\u0000/evil')).toBe('/rooms')
    expect(safe('/ /evil')).toBe('/rooms')
    // 참고: 검사 없이 파싱하면 정말 외부로 풀린다
    expect(new URL('/\t/evil.example', ORIGIN).origin).toBe('https://evil.example')
  })

  it('퍼센트 인코딩된 경로는 같은 출처 안에 머문다', () => {
    expect(safe('/%2F%2Fevil.example')).toBe('/%2F%2Fevil.example')
  })
})
