/**
 * 시그널링 모듈 경계 검사 — docs/server-architecture.md 2.1절(계층·허용 간선), 2.5절, 3.1절 "S2 생성".
 *
 * 서버에는 ESLint가 없으므로 import 문을 읽어 강제한다(엔진 tests/architecture.rs와 같은 역할).
 * - `signaling/*.ts`는 `services/wsService`를 import하지 않는다(조립은 wsService → signaling 한 방향).
 * - 모든 `signaling/*.ts`는 아래 LAYERS에 계층이 정해져 있어야 한다(새 파일은 여기에 넣으면서 계층을 정한다).
 * - signaling 안의 간선은 아래 계층으로만. 같은 계층 간선은 계층 0(순수 모듈)과 SAME_LAYER_ALLOWED뿐이다.
 * - 순환 금지.
 * 예외 목록은 고정 상수다. 늘리려면 설계 문서 2.1절을 같은 PR에서 고친다.
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

const SIGNALING_DIR = join(__dirname, '..', 'src', 'signaling')

/** 2.1절 의존 방향. roomDeps·supabaseDeps(WsDeps 어댑터, S1)는 순수 계층 0에 둔다 */
const LAYERS: Record<string, number> = {
  protocol: 0, types: 0, rateLimit: 0, descriptor: 0, roomDeps: 0, supabaseDeps: 0,
  registry: 1, roomMeta: 1,
  capacity: 2, password: 2, performers: 2, metronome: 2, requests: 2, nativeSession: 2, chat: 2, viewerRelay: 2,
  membership: 3, resume: 3, auth: 3,
  dispatch: 4, connection: 4,
  index: 5,
}

/** 계층 2 사이에 허용하는 간선(1.2절): 재전송·ssrc 변경 정지, 승인 기록·악기 변경, 연주 중단 시 ssrc 삭제 */
const SAME_LAYER_ALLOWED = new Set([
  'nativeSession->metronome',
  'requests->performers',
  'performers->nativeSession',
])

/** `import ... from '<spec>'`, `export ... from '<spec>'`, `import('<spec>')`, `require('<spec>')` */
function importSpecifiers(source: string): string[] {
  const specs: string[] = []
  const patterns = [
    /^\s*(?:import|export)\s+(?:type\s+)?[^'"]*?\sfrom\s+['"]([^'"]+)['"]/gm,
    /^\s*import\s+['"]([^'"]+)['"]/gm,
    /\b(?:import|require)\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
  ]
  for (const re of patterns) {
    for (const m of source.matchAll(re)) specs.push(m[1])
  }
  return specs
}

function signalingModules(): Map<string, string[]> {
  const out = new Map<string, string[]>()
  for (const file of readdirSync(SIGNALING_DIR)) {
    if (!file.endsWith('.ts')) continue
    out.set(file.slice(0, -3), importSpecifiers(readFileSync(join(SIGNALING_DIR, file), 'utf8')))
  }
  return out
}

/** signaling 안의 상대 import(`./x`)만 모듈 이름으로 */
const localTarget = (spec: string): string | null => (spec.startsWith('./') ? spec.slice(2).replace(/\.ts$/, '') : null)

describe('signaling architecture (S2)', () => {
  const modules = signalingModules()

  it('reads the import statements it is meant to check', () => {
    assert.deepEqual(
      importSpecifiers([
        "import type { A } from './a'",
        "import { B,\n  C } from '../services/wsService'",
        "export { D } from './d'",
        "export * from './e'",
        "const x = require('./f')",
        "import './g'",
      ].join('\n')).sort(),
      ['../services/wsService', './a', './d', './e', './f', './g'],
    )
    assert.ok(modules.size >= 10, `signaling modules found: ${[...modules.keys()].join(', ')}`)
  })

  it('no signaling module imports services/wsService', () => {
    const offenders: string[] = []
    for (const [name, specs] of modules) {
      for (const spec of specs) if (/(^|\/)services\/wsService$/.test(spec)) offenders.push(`${name} -> ${spec}`)
    }
    assert.deepEqual(offenders, [])
  })

  it('every signaling module has a layer', () => {
    const missing = [...modules.keys()].filter(name => LAYERS[name] === undefined)
    assert.deepEqual(missing, [], 'add new signaling files to LAYERS (docs/server-architecture.md 2.1)')
  })

  it('signaling imports only go down a layer (or use an allowed same-layer edge)', () => {
    const violations: string[] = []
    for (const [name, specs] of modules) {
      for (const spec of specs) {
        const target = localTarget(spec)
        if (!target) continue
        const from = LAYERS[name]
        const to = LAYERS[target]
        if (to === undefined) {
          violations.push(`${name} -> ${target} (unknown module)`)
          continue
        }
        if (to < from) continue
        if (to === from && (from === 0 || SAME_LAYER_ALLOWED.has(`${name}->${target}`))) continue
        violations.push(`${name}(${from}) -> ${target}(${to})`)
      }
    }
    assert.deepEqual(violations, [])
  })

  it('signaling imports have no cycles', () => {
    const edges = new Map<string, string[]>()
    for (const [name, specs] of modules) {
      edges.set(name, specs.map(localTarget).filter((t): t is string => t !== null && modules.has(t)))
    }
    const state = new Map<string, 'visiting' | 'done'>()
    const cycles: string[] = []
    const visit = (n: string, path: string[]) => {
      if (state.get(n) === 'done') return
      if (state.get(n) === 'visiting') {
        cycles.push([...path.slice(path.indexOf(n)), n].join(' -> '))
        return
      }
      state.set(n, 'visiting')
      for (const t of edges.get(n) || []) visit(t, [...path, n])
      state.set(n, 'done')
    }
    for (const n of edges.keys()) visit(n, [])
    assert.deepEqual(cycles, [])
  })
})
