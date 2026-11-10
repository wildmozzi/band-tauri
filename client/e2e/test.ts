/**
 * 모든 E2E가 쓰는 test. 자동 fixture:
 *   mock          가짜 백엔드 설치(테스트마다 새 DB). 끝날 때 차단·미처리 요청이 0이어야 한다
 *   consoleErrors 페이지 console.error·처리 안 된 예외 수집. 끝날 때 0이어야 한다
 *                 (의도한 실패 응답은 consoleErrors.allow(/패턴/)로 그 테스트에서만 허용)
 */
import { test as base, expect, type ConsoleMessage } from '@playwright/test'
import { installMocks, type MockBackend } from './mocks'

export interface ConsoleErrors {
  /** 이 테스트에서 허용할 오류 (메시지 + 위치 URL에 대해 검사) */
  allow(pattern: RegExp): void
  readonly errors: string[]
}

export const test = base.extend<{ mock: MockBackend; consoleErrors: ConsoleErrors }>({
  mock: [
    async ({ context }, use) => {
      const mock = await installMocks(context)
      await use(mock)
      expect(mock.violations, 'mock이 처리하지 못했거나 차단한 요청').toEqual([])
    },
    { auto: true },
  ],

  consoleErrors: [
    async ({ page }, use) => {
      const allowed: RegExp[] = []
      const errors: string[] = []
      const record = (text: string) => {
        if (!allowed.some(re => re.test(text))) errors.push(text)
      }
      page.on('console', (msg: ConsoleMessage) => {
        if (msg.type() !== 'error') return
        const where = msg.location().url
        record(where ? `${msg.text()} @ ${where}` : msg.text())
      })
      page.on('pageerror', err => record(`pageerror: ${err.message}`))
      await use({ allow: re => allowed.push(re), errors })
      expect(errors, '브라우저 콘솔 에러').toEqual([])
    },
    { auto: true },
  ],
})

export { expect }
