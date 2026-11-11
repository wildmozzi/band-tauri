import { defineConfig, devices } from '@playwright/test'
import { E2E_BASE_URL, E2E_PORT } from './e2e/env'
import { E2E_PORT_ENV } from './e2e/port'

/**
 * 웹(비 Tauri) E2E. 앱을 가짜 엔드포인트로 빌드해(e2e/vite.config.ts) vite preview로 띄우고,
 * Supabase·서버 API·시그널링·결제 SDK는 전부 브라우저 안에서 가로챈다(e2e/mocks). 운영 DB에 닿지 않는다.
 * 파일 이름은 *.e2e.ts — vitest(*.test.ts / *.spec.ts)가 집어 가지 않게.
 */
export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.e2e.ts',
  outputDir: './e2e/.results',
  timeout: 30_000,
  expect: { timeout: 7_000 },
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: process.env.CI
    ? [['list'], ['html', { outputFolder: './e2e/.report', open: 'never' }], ['github']]
    : [['list'], ['html', { outputFolder: './e2e/.report', open: 'never' }]],
  use: {
    baseURL: E2E_BASE_URL,
    locale: 'ko-KR',
    timezoneId: 'Asia/Seoul',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'off',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npx vite build --config e2e/vite.config.ts --logLevel warn && npx vite preview --config e2e/vite.config.ts',
    url: E2E_BASE_URL,
    // 포트는 실행마다 빈 포트(e2e/port.ts). e2e/vite.config.ts가 같은 env로 같은 포트를 읽는다(명시적으로 넘김)
    env: { [E2E_PORT_ENV]: String(E2E_PORT) },
    // 재사용 금지: 같은 포트의 다른 서버(다른 워크트리의 실제 env 빌드 등)에 붙으면 안 된다. 포트가 차 있으면 바로 실패한다
    reuseExistingServer: false,
    timeout: 180_000,
    stdout: 'ignore',
    stderr: 'pipe',
  },
})
