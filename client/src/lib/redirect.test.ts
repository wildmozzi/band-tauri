import { describe, expect, it } from 'vitest'
import { safeRedirectPath } from './redirect'

describe('safeRedirectPath', () => {
  it('앱 안 경로는 그대로 (쿼리 포함)', () => {
    expect(safeRedirectPath('/payment?plan=standard')).toBe('/payment?plan=standard')
    expect(safeRedirectPath('/profile')).toBe('/profile')
  })

  it('없거나 외부 주소면 홈', () => {
    expect(safeRedirectPath(undefined)).toBe('/')
    expect(safeRedirectPath(42)).toBe('/')
    expect(safeRedirectPath('https://evil.example')).toBe('/')
    expect(safeRedirectPath('//evil.example')).toBe('/')
    expect(safeRedirectPath('/\\evil.example')).toBe('/')
    expect(safeRedirectPath('rooms')).toBe('/')
  })
})
