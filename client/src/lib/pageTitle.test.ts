import { describe, expect, it } from 'vitest'
import { pageTitleFor } from './pageTitle'

describe('pageTitleFor', () => {
  it('라우트별 제목에 서비스명을 붙인다', () => {
    expect(pageTitleFor('/')).toBe('IndigoMuse — 온라인 합주실')
    expect(pageTitleFor('/rooms')).toBe('IndigoMuse — 합주실')
    expect(pageTitleFor('/rooms/create')).toBe('IndigoMuse — 합주실 만들기')
    expect(pageTitleFor('/rooms/abc-123')).toBe('IndigoMuse — 합주실')
    expect(pageTitleFor('/community/edit/p1')).toBe('IndigoMuse — 글 수정')
    expect(pageTitleFor('/community/p1')).toBe('IndigoMuse — 커뮤니티')
    expect(pageTitleFor('/payment')).toBe('IndigoMuse — 결제')
    expect(pageTitleFor('/profile/')).toBe('IndigoMuse — 프로필')
  })

  it('없는 경로는 404 제목', () => {
    expect(pageTitleFor('/nope')).toBe('IndigoMuse — 페이지를 찾을 수 없음')
    expect(pageTitleFor('/rooms/a/b')).toBe('IndigoMuse — 페이지를 찾을 수 없음')
  })
})
