/**
 * 웹 관람 묶음 분리 규칙(docs/frontend-architecture.md 1절): 데스크톱 전용 엔진 런타임(연주 세션 본체·상태기계·관람 방송)은
 * runtimeLoader가 동적 import하는 nativeRuntime 묶음에만 있어야 한다. 다른 코드가 값으로 정적 import하면 웹 첫 묶음에
 * 다시 실린다(타입만 쓰는 `import type`은 빌드에서 지워지므로 괜찮다). 실제로 안 받는지는 e2e native-split이,
 * 하위 의존(relayLogic 등)까지 첫 화면 JS에 없는지는 e2e bundle-budget(빌드 산출물 검사)이 본다.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const SRC = fileURLToPath(new URL('..', import.meta.url))
/** 지연 묶음 전용 모듈 → 값으로 정적 import해도 되는 파일(그 묶음 안의 파일) */
const RUNTIME_ONLY: Record<string, string[]> = {
  nativeRuntime: [],
  performSession: ['native/nativeRuntime.ts'],
  mixBridge: ['native/nativeRuntime.ts'],
  sessionLogic: ['native/performSession.ts'],
}

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = join(dir, e.name)
    if (e.isDirectory()) return sourceFiles(p)
    return /\.(ts|tsx)$/.test(e.name) && !/\.test\.(ts|tsx)$/.test(e.name) ? [p] : []
  })
}

describe('엔진 런타임은 지연 묶음에만', () => {
  it('nativeRuntime·performSession·mixBridge·sessionLogic을 값으로 정적 import하는 곳은 그 묶음 안뿐', () => {
    const offenders: string[] = []
    for (const file of sourceFiles(SRC)) {
      const rel = relative(SRC, file).replace(/\\/g, '/')
      const text = readFileSync(file, 'utf8')
      // 값 import·re-export(`from '…'`)와 부수효과 import(`import '…'`) 모두. 확장자를 붙인 경로('./performSession.ts')도 (#216 리뷰 L-3)
      for (const m of text.matchAll(/^(import|export)\s+(?!type\b)(?:[^;'"]*?from\s+)?['"]([^'"]+)['"]/gms)) {
        const name = m[2].split('/').pop()!.replace(/\.(ts|tsx|js|mjs)$/, '')
        const allowed = RUNTIME_ONLY[name]
        if (allowed && !allowed.includes(rel)) offenders.push(`${rel} → ${m[2]}`)
      }
    }
    expect(offenders).toEqual([])
  })

  // 내 채널 진단 줄 문구(#250): EngineDiagLines는 performerUi 지연 묶음이다. 첫 화면 모듈(roomDetailModel 등)이
  // engineDiagFormat을 값으로 import하면 공유 모듈이 되어 릴레이·송신 큐 문구가 웹 첫 묶음에 다시 실린다(피어 툴팁은 peerDiagFormat)
  it('engineDiagFormat을 값으로 정적 import하는 곳은 EngineDiagLines뿐', () => {
    const offenders: string[] = []
    for (const file of sourceFiles(SRC)) {
      const rel = relative(SRC, file).replace(/\\/g, '/')
      const text = readFileSync(file, 'utf8')
      for (const m of text.matchAll(/^(import|export)\s+(?!type\b)(?:[^;'"]*?from\s+)?['"]([^'"]+)['"]/gms)) {
        const name = m[2].split('/').pop()!.replace(/\.(ts|tsx|js|mjs)$/, '')
        if (name === 'engineDiagFormat' && rel !== 'features/room/components/EngineDiagLines.tsx') offenders.push(`${rel} → ${m[2]}`)
      }
    }
    expect(offenders).toEqual([])
  })
})
