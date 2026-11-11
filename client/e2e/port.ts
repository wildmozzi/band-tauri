/**
 * E2E 미리보기 서버 포트 (P3-10f).
 *
 * 예전에는 47173 고정이라 여러 워크트리·CI self-hosted runner(같은 PC)가 동시에 e2e를 돌리면 서로 포트를 다퉜다.
 * 이제는 실행마다 OS에게 빈 포트를 받아 쓴다(포트 0 바인딩). 정하는 곳은 Playwright 메인 프로세스 한 곳이고,
 * 정한 값을 process.env.E2E_PORT에 넣어 두면 워커 프로세스와 webServer(vite preview)가 환경변수로 같은 값을 물려받는다.
 *
 * - `E2E_PORT=<n>` 으로 덮어쓸 수 있다(디버깅·방화벽 예외 등). 잘못된 값이면 바로 실패한다.
 * - vite preview는 strictPort라 그 포트를 못 쓰면 다른 포트로 넘어가지 않고 실패하며,
 *   playwright.config의 reuseExistingServer: false라 이미 떠 있는 남의 서버에 붙지도 않는다.
 * - 한계: 빈 포트를 확인한 뒤 vite preview가 바인딩하기까지 틈이 있다. 빌드는 포트 선택 전에 끝내므로(npm run e2e =
 *   e2e:build → playwright test, server.ts) 틈은 preview 기동 시간(보통 1초 안팎)뿐이다. 그 사이 같은 PC의 다른 프로세스가
 *   같은 포트를 잡으면 strictPort 때문에 엉뚱한 서버에 붙지 않고 실패로 드러난다(선택된 포트는 실행 첫 줄에 로그).
 */
import { execFileSync } from 'node:child_process'

export const E2E_PORT_ENV = 'E2E_PORT'

/** 127.0.0.1의 포트 0에 바인딩해 OS가 고른 빈 포트를 출력하고 바로 닫는 스크립트 (별도 node 프로세스에서 동기로 실행) */
export const FREE_PORT_PROBE = [
  "const s = require('node:net').createServer();",
  "s.on('error', e => { console.error(e.message); process.exit(1) });",
  "s.listen({ port: 0, host: '127.0.0.1', exclusive: true }, () => {",
  '  process.stdout.write(String(s.address().port)); s.close();',
  '});',
].join('\n')

/** 지금 비어 있는 127.0.0.1 포트 하나 (설정 파일은 동기로 평가되므로 동기 함수) */
export function findFreePortSync(): number {
  const out = execFileSync(process.execPath, ['-e', FREE_PORT_PROBE], { encoding: 'utf8', timeout: 10_000 }).trim()
  return parsePort(out, '빈 포트 탐색 결과')
}

export function parsePort(value: string, source: string): number {
  const port = /^\d+$/.test(value.trim()) ? Number(value) : NaN
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`${source}가 올바른 포트가 아닙니다: ${JSON.stringify(value)} (1~65535 정수)`)
  }
  return port
}

/**
 * E2E 포트를 정한다. env에 E2E_PORT가 있으면 그 값(검증), 없으면 빈 포트를 찾아 env에 기록한다
 * (같은 env를 물려받는 워커·webServer가 같은 포트를 쓰게).
 */
export function resolveE2EPort(env: NodeJS.ProcessEnv = process.env, findFree: () => number = findFreePortSync): number {
  const fromEnv = env[E2E_PORT_ENV]
  if (fromEnv !== undefined && fromEnv !== '') return parsePort(fromEnv, `환경변수 ${E2E_PORT_ENV}`)
  const port = findFree()
  env[E2E_PORT_ENV] = String(port)
  return port
}
