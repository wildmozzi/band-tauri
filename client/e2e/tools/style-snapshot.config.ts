/**
 * 계산 스타일 스냅샷 도구 설정 (P3-2 ③-3). CI 밖 수동 실행 — 사용법은 style-snapshot.tool.ts 머리말.
 * 앱 E2E 설정(../../playwright.config.ts)의 빌드·가짜 백엔드·미리보기 서버를 그대로 쓰고, 테스트 파일만 *.tool.ts로 바꾼다.
 */
import { defineConfig } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import base from '../../playwright.config'

const clientRoot = fileURLToPath(new URL('../..', import.meta.url))

export default defineConfig({
  ...base,
  testDir: '.',
  testMatch: '**/*.tool.ts',
  outputDir: '../.results/style-snapshot',
  retries: 0,
  reporter: [['list']],
  // 덤프는 화면마다 독립이라 병렬로 돌려도 된다(화면마다 새 브라우저 컨텍스트·새 가짜 DB)
  fullyParallel: true,
  webServer: Array.isArray(base.webServer) ? base.webServer : { ...base.webServer!, cwd: clientRoot },
})
