/**
 * E2E 미리보기 서버 포트 (P3-10f, 경쟁 수정 P3-0d).
 *
 * 예전에는 47173 고정이라 여러 워크트리·CI runner(같은 PC)가 동시에 e2e를 돌리면 서로 포트를 다퉜다.
 * 정하는 곳은 Playwright 메인 프로세스 한 곳이고, 정한 값을 process.env.E2E_PORT에 넣어 두면 워커 프로세스와
 * webServer(vite preview)가 환경변수로 같은 값을 물려받는다.
 *
 * - 고르는 방법은 pick-port.mjs: OS 임시 포트 범위 밖(E2E_PORT_RANGE)에서 포트별 잠금 파일을 배타 생성한 뒤 실제 바인딩을
 *   확인한다. 예전 방식(포트 0으로 OS에게 받고 바로 닫기)은 닫은 포트를 OS가 동시에 고르는 다른 실행에게 다시 줄 수 있어
 *   두 실행이 같은 포트를 받았다(#61 CI, 6개 중 2개). 잠금은 이 프로세스가 끝날 때 지운다(vite preview는 그 전에 내려간다).
 * - `E2E_PORT=<n>` 으로 덮어쓸 수 있다(디버깅·방화벽 예외 등). 잘못된 값이면 바로 실패한다.
 * - vite preview는 strictPort라 그 포트를 못 쓰면 다른 포트로 넘어가지 않고 실패하며,
 *   playwright.config의 reuseExistingServer: false라 이미 떠 있는 남의 서버에 붙지도 않는다.
 * - 남은 틈: 바인딩 확인 뒤 vite preview가 바인딩하기까지(preview 기동 시간) 그 포트를 "직접 지정해" 여는 다른 프로그램.
 *   e2e 실행끼리는 잠금으로, OS 자동 배정과는 범위로 막힌다. 그래도 생기면 strictPort 때문에 엉뚱한 서버에 붙지 않고
 *   webServer 기동 실패로 드러난다(선택된 포트는 실행 첫 줄에 로그).
 */
import { execFileSync } from 'node:child_process'
import { readFileSync, unlinkSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

export const E2E_PORT_ENV = 'E2E_PORT'

/**
 * 고르는 범위 [min, max). 리눅스 임시 포트(32768~60999)·윈도우 동적 포트(49152~65535) 아래라
 * OS가 포트 0 바인딩·나가는 연결에 나눠주지 않는다.
 */
export const E2E_PORT_RANGE = { min: 20_000, max: 32_000 } as const

/** 같은 PC의 모든 e2e 실행(워크트리·CI 프로세스)이 함께 보는 잠금 폴더 */
export const E2E_PORT_LOCK_DIR = join(tmpdir(), 'band-tauri-e2e-ports')

/** 실제로 고르는 스크립트(별도 node 프로세스에서 동기로 실행) */
export const PICK_PORT_SCRIPT = fileURLToPath(new URL('./pick-port.mjs', import.meta.url))

export interface PickPortOptions {
  min?: number
  max?: number
  lockDir?: string
  /** 잠금 소유자. 기본은 이 프로세스이고, 이 프로세스가 끝나면 잠금을 지운다 */
  ownerPid?: number
  /** 범위 안 시작 위치(테스트용, 기본 무작위) */
  start?: number
}

const held = new Set<string>()
let exitHook = false

/** 잠금 해제(종료 훅·테스트). 소유자가 이 프로세스인 잠금만 지운다 */
export function releasePortLock(lock: string): void {
  held.delete(lock)
  try {
    if (Number(readFileSync(lock, 'utf8').split(' ')[0]) === process.pid) unlinkSync(lock)
  } catch {
    // 이미 없음
  }
}

/** 지금 비어 있고 다른 e2e 실행이 잡지 않은 127.0.0.1 포트 하나 (설정 파일은 동기로 평가되므로 동기 함수) */
export function findFreePortSync(opts: PickPortOptions = {}): number {
  const { min = E2E_PORT_RANGE.min, max = E2E_PORT_RANGE.max, lockDir = E2E_PORT_LOCK_DIR, ownerPid = process.pid } = opts
  const args = [PICK_PORT_SCRIPT, lockDir, String(min), String(max), String(ownerPid)]
  if (opts.start !== undefined) args.push(String(opts.start))
  const out = execFileSync(process.execPath, args, { encoding: 'utf8', timeout: 30_000 }).trim()
  const port = parsePort(out, '빈 포트 탐색 결과')
  if (ownerPid === process.pid) {
    held.add(join(lockDir, `${port}.lock`))
    if (!exitHook) {
      exitHook = true
      process.on('exit', () => [...held].forEach(releasePortLock))
    }
  }
  return port
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
export function resolveE2EPort(env: NodeJS.ProcessEnv = process.env, findFree: () => number = () => findFreePortSync()): number {
  const fromEnv = env[E2E_PORT_ENV]
  if (fromEnv !== undefined && fromEnv !== '') return parsePort(fromEnv, `환경변수 ${E2E_PORT_ENV}`)
  const port = findFree()
  env[E2E_PORT_ENV] = String(port)
  return port
}
