import { describe, expect, it } from 'vitest'
import { pageTitleFor } from './pageTitle'
import { ROUTE_TABLE } from '../routeTable'

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

  it('라우터와 같은 우선순위: 정적 세그먼트가 파라미터보다 먼저 (표 순서와 무관)', () => {
    expect(pageTitleFor('/community/create')).toBe('IndigoMuse — 글쓰기')
    expect(pageTitleFor('/payment/success')).toBe('IndigoMuse — 결제 완료')
    expect(pageTitleFor('/settings/audio')).toBe('IndigoMuse — 오디오 설정')
  })

  it('라우트 표의 모든 고정 경로에 제목이 있다', () => {
    for (const r of ROUTE_TABLE) {
      if (r.path === '*') continue
      const sample = r.path.replace(/:\w+/g, 'x1')
      const expected = `IndigoMuse — ${r.title ?? '온라인 합주실'}`
      expect(pageTitleFor(sample), r.path).toBe(expected)
    }
  })

  it('없는 경로는 404 제목', () => {
    expect(pageTitleFor('/nope')).toBe('IndigoMuse — 페이지를 찾을 수 없음')
    expect(pageTitleFor('/rooms/a/b')).toBe('IndigoMuse — 페이지를 찾을 수 없음')
  })
})
