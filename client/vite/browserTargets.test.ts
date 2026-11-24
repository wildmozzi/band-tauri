import { describe, expect, it } from 'vitest'
import { BROWSER_TARGET } from './browserTargets'
import appConfig from '../vite.config'

// 브라우저 지원 하한(P3-20b 리뷰 M-1): 빠지면 vite 8 기본값(safari16.4 …)으로 올라간다.
// e2e 빌드(e2e/vite.config.ts)는 불러오면 process.env를 바꾸므로 여기서 보지 않고, 산출물 CSS를 e2e/bundle-budget이 검사한다
describe('build target', () => {
  it('vite 5 기본값과 같은 하한', () => {
    expect(BROWSER_TARGET).toEqual(['es2020', 'edge88', 'firefox78', 'chrome87', 'safari14'])
  })

  it('앱 빌드가 JS·CSS 모두 이 하한을 쓴다', () => {
    if (typeof appConfig === 'function' || appConfig instanceof Promise) throw new Error('객체 설정이어야 함')
    expect(appConfig.build?.target).toBe(BROWSER_TARGET)
    expect(appConfig.build?.cssTarget).toBe(BROWSER_TARGET)
  })
})
