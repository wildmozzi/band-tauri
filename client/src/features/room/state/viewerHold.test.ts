import { describe, expect, it } from 'vitest'
import { holdViewerOnClose, keepHeldViewer } from './viewerHold'

describe('관람 수신 유지 (시그널링 재개)', () => {
  it('로그인 관람자가 수신 중에 끊기면 지금 clientId로 유지해 둔다', () => {
    expect(holdViewerOnClose({ loggedIn: true, selfId: 'c1', hasViewer: true })).toBe('c1')
  })
  it('익명·수신 없음·clientId 모름이면 유지하지 않는다(지금 닫는다)', () => {
    expect(holdViewerOnClose({ loggedIn: false, selfId: 'c1', hasViewer: true })).toBeNull()
    expect(holdViewerOnClose({ loggedIn: true, selfId: 'c1', hasViewer: false })).toBeNull()
    expect(holdViewerOnClose({ loggedIn: true, selfId: null, hasViewer: true })).toBeNull()
  })
  it('같은 clientId로 재개됐을 때만 계속 쓴다', () => {
    expect(keepHeldViewer('c1', 'c1', true)).toBe(true)
    expect(keepHeldViewer('c1', 'c2', false)).toBe(false) // 재개 실패(유예 지남·서버 재시작): 새 id
    expect(keepHeldViewer('c1', 'c1', false)).toBe(false)
    expect(keepHeldViewer(null, 'c1', true)).toBe(false)
  })
})
