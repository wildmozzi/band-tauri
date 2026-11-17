// 계약 감사 2차: 서버 → 클라이언트 시그널링 메시지(server/src/signaling/protocol.ts `ServerMessage`·`ServerError`)와
// 클라이언트 수신 처리의 대조. 클라이언트는 페이로드를 `Record<string, unknown>`으로 받고 문자열로 분기하므로(타입 유니언이
// 없음) 소스 텍스트로 본다:
// - 서버가 보낼 수 있는 모든 type과 에러 code를 클라이언트 어딘가가 다룬다(새 메시지를 더하고 수신 처리를 잊으면 실패).
// - 서버 메시지를 분기하는 reducer의 `case 'x'`는 모두 서버 유니언에 있다(오타·없어진 메시지를 기다리는 분기 방지).
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const ROOT = fileURLToPath(new URL('../../../../../', import.meta.url))
const read = (rel: string) => readFileSync(join(ROOT, rel), 'utf8').replace(/\r/g, '')

/** `export type ServerMessage = | … ` 유니언의 `type: 'x'` 리터럴(ServerError는 'error'로 하나) */
function serverTypes(): string[] {
  const src = read('server/src/signaling/protocol.ts')
  const start = src.indexOf('export type ServerMessage =')
  expect(start).toBeGreaterThan(0)
  // 유니언은 다음 빈 줄 두 개 또는 파일 끝까지(주석 줄 포함)
  const rest = src.slice(start)
  const end = rest.search(/\n\n(export|\/\*\*|$)/)
  const body = end > 0 ? rest.slice(0, end) : rest
  const types = [...body.matchAll(/type: '([a-z-]+)'/g)].map((m) => m[1])
  if (body.includes('ServerError')) types.push('error')
  return [...new Set(types)]
}

/** `ServerError` 유니언의 `ErrorOf<'a' | 'b', …>` 첫 인자 리터럴 */
function serverErrorCodes(): string[] {
  const src = read('server/src/signaling/protocol.ts')
  const start = src.indexOf('export type ServerError =')
  const end = src.indexOf('export type ServerMessage =')
  expect(start).toBeGreaterThan(0)
  const body = src.slice(start, end)
  const codes: string[] = []
  for (const m of body.matchAll(/ErrorOf<((?:'[a-z-]+'\s*\|?\s*)+)/g)) {
    codes.push(...[...m[1].matchAll(/'([a-z-]+)'/g)].map((x) => x[1]))
  }
  return [...new Set(codes)]
}

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name)
    if (statSync(p).isDirectory()) walk(p, out)
    else if (/\.(ts|tsx)$/.test(name) && !name.includes('.test.')) out.push(p)
  }
  return out
}

const clientSources = () => walk(join(ROOT, 'client', 'src')).map((p) => readFileSync(p, 'utf8'))

describe('서버 → 클라이언트 메시지 계약', () => {
  it('서버 유니언의 모든 type을 클라이언트가 분기한다', () => {
    const types = serverTypes()
    expect(types.length).toBeGreaterThanOrEqual(25)
    const srcs = clientSources()
    const handled = (t: string) => srcs.some((s) => new RegExp(`(case |=== |!== )'${t}'`).test(s))
    expect(types.filter((t) => !handled(t))).toEqual([])
  })

  it('서버 에러 code를 모두 클라이언트가 안다', () => {
    const codes = serverErrorCodes()
    expect(codes.length).toBeGreaterThanOrEqual(15)
    const srcs = clientSources()
    expect(codes.filter((c) => !srcs.some((s) => s.includes(`'${c}'`)))).toEqual([])
  })

  it('서버 메시지 reducer의 case는 모두 서버 유니언에 있다', () => {
    const types = new Set(serverTypes())
    // performRequestState의 switch는 자기 액션 종류(requested·resolved …)라 대상이 아니다 — 서버 type은 isPerformRequestSignal로 본다
    for (const rel of ['client/src/features/room/state/signalReducer.ts']) {
      const cases = [...read(rel).matchAll(/case '([a-z-]+)':/g)].map((m) => m[1])
      expect(cases.length, rel).toBeGreaterThan(0)
      expect(cases.filter((c) => !types.has(c)), rel).toEqual([])
    }
  })
})
