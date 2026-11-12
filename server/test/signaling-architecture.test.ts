/**
 * 시그널링 모듈 경계 검사 — docs/server-architecture.md 2.1절(계층·허용 간선), 2.5절, 3.1절 "S2 생성".
 *
 * 서버에는 ESLint가 없으므로 import 문을 읽어 강제한다(엔진 tests/architecture.rs와 같은 역할).
 * - `signaling/` 밖의 `src/services/*`는 SERVICES_ALLOWED만 import한다. `services/wsService`는 절대 안 된다
 *   (조립은 wsService → signaling 한 방향).
 * - 모든 `signaling/**.ts`는 아래 LAYERS에 계층이 정해져 있어야 한다(새 파일은 여기에 넣으면서 계층을 정한다).
 *   하위 폴더의 파일은 `sub/x` 이름으로 보이고, LAYERS에 없으면 실패한다.
 * - signaling 안의 간선은 아래 계층으로만. 같은 계층 간선은 계층 0(순수 모듈)과 SAME_LAYER_ALLOWED뿐이다.
 *   import 경로는 실제 파일 위치로 풀어서 본다(`./x`, `./x.js`, `./x/index`, `../signaling/x` 모두 같은 모듈).
 * - 순환 금지.
 * 예외 목록은 고정 상수다. 늘리려면 설계 문서 2.1절을 같은 PR에서 고친다.
 * 판정 함수는 가짜 모듈 표로도 돌려 위반을 실제로 잡는지 확인한다(음성 케이스).
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'

const SRC_DIR = join(__dirname, '..', 'src')
const SIGNALING_DIR = join(SRC_DIR, 'signaling')
const SERVICES_DIR = join(SRC_DIR, 'services')

/** 2.1절 의존 방향. roomDeps·supabaseDeps(WsDeps 어댑터, S1)·ctx(연결 문맥 타입, S3)는 순수 계층 0에 둔다 */
const LAYERS: Record<string, number> = {
  protocol: 0, types: 0, rateLimit: 0, descriptor: 0, roomDeps: 0, supabaseDeps: 0, ctx: 0,
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

/** signaling이 import해도 되는 `src/services/*` (2.1절: relayToken은 Rust 릴레이와 맞춘 형식이라 services에 남는다) */
const SERVICES_ALLOWED = new Set(['relayToken'])

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

/** 파일 경로(확장자 제외) → 모듈 이름. 폴더의 index는 폴더 이름으로 본다. 구분자는 `/` */
function moduleName(dir: string, absNoExt: string): string {
  const rel = relative(dir, absNoExt).split(sep).join('/')
  return rel.endsWith('/index') ? rel.slice(0, -'/index'.length) : rel
}

/** 모듈 이름 → 그 모듈이 import하는 지정자 (signaling 아래 모든 .ts, 하위 폴더 포함) */
type Modules = Map<string, string[]>

function signalingModules(dir = SIGNALING_DIR): Modules {
  const out: Modules = new Map()
  for (const entry of readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (!entry.isFile() || !entry.name.endsWith('.ts')) continue
    const abs = join(entry.parentPath, entry.name)
    out.set(moduleName(SIGNALING_DIR, abs.slice(0, -3)), importSpecifiers(readFileSync(abs, 'utf8')))
  }
  return out
}

/** import 지정자를 가리키는 곳으로 푼다. 패키지(상대 경로가 아님)는 null */
type Target = { kind: 'signaling' | 'services' | 'other'; name: string }

function resolveImport(fromModule: string, spec: string): Target | null {
  if (!spec.startsWith('.')) return null // 패키지
  const fromDir = dirname(join(SIGNALING_DIR, ...fromModule.split('/')))
  const abs = resolve(fromDir, spec).replace(/\.(ts|js)$/, '')
  const inside = (dir: string) => abs === dir || abs.startsWith(dir + sep)
  if (inside(SIGNALING_DIR)) {
    const name = moduleName(SIGNALING_DIR, abs)
    return { kind: 'signaling', name: name === '' || name === 'index' ? 'index' : name }
  }
  if (inside(SERVICES_DIR)) return { kind: 'services', name: moduleName(SERVICES_DIR, abs) }
  return { kind: 'other', name: relative(SRC_DIR, abs).split(sep).join('/') }
}

function servicesViolations(modules: Modules): string[] {
  const out: string[] = []
  for (const [name, specs] of modules) {
    for (const spec of specs) {
      const t = resolveImport(name, spec)
      if (t?.kind === 'services' && !SERVICES_ALLOWED.has(t.name)) out.push(`${name} -> services/${t.name}`)
    }
  }
  return out
}

function missingLayers(modules: Modules): string[] {
  return [...modules.keys()].filter(name => LAYERS[name] === undefined)
}

function layerViolations(modules: Modules): string[] {
  const violations: string[] = []
  for (const [name, specs] of modules) {
    for (const spec of specs) {
      const t = resolveImport(name, spec)
      if (t?.kind !== 'signaling') continue
      const target = t.name
      const from = LAYERS[name]
      const to = LAYERS[target]
      if (to === undefined) {
        violations.push(`${name} -> ${target} (unknown module)`)
        continue
      }
      if (from === undefined) continue // missingLayers가 따로 잡는다
      if (to < from) continue
      if (to === from && (from === 0 || SAME_LAYER_ALLOWED.has(`${name}->${target}`))) continue
      violations.push(`${name}(${from}) -> ${target}(${to})`)
    }
  }
  return violations
}

function cycles(modules: Modules): string[] {
  const edges = new Map<string, string[]>()
  for (const [name, specs] of modules) {
    const targets: string[] = []
    for (const spec of specs) {
      const t = resolveImport(name, spec)
      if (t?.kind === 'signaling' && modules.has(t.name)) targets.push(t.name)
    }
    edges.set(name, targets)
  }
  const state = new Map<string, 'visiting' | 'done'>()
  const found: string[] = []
  const visit = (n: string, path: string[]) => {
    if (state.get(n) === 'done') return
    if (state.get(n) === 'visiting') {
      found.push([...path.slice(path.indexOf(n)), n].join(' -> '))
      return
    }
    state.set(n, 'visiting')
    for (const t of edges.get(n) || []) visit(t, [...path, n])
    state.set(n, 'done')
  }
  for (const n of edges.keys()) visit(n, [])
  return found
}

describe('signaling architecture (S2·S3)', () => {
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
    assert.ok(modules.has('dispatch') && modules.has('ctx'), 'S3 modules are scanned')
  })

  it('resolves every import form to the module it points at', () => {
    const cases: [string, string, Target | null][] = [
      ['membership', './registry', { kind: 'signaling', name: 'registry' }],
      ['membership', './registry.ts', { kind: 'signaling', name: 'registry' }],
      ['membership', './registry.js', { kind: 'signaling', name: 'registry' }],
      ['membership', '../signaling/registry', { kind: 'signaling', name: 'registry' }],
      ['membership', './sub/x', { kind: 'signaling', name: 'sub/x' }],
      ['membership', './sub/index', { kind: 'signaling', name: 'sub' }],
      ['membership', '.', { kind: 'signaling', name: 'index' }],
      ['sub/x', '../registry', { kind: 'signaling', name: 'registry' }],
      ['sub/x', './y', { kind: 'signaling', name: 'sub/y' }],
      ['membership', '../services/wsService', { kind: 'services', name: 'wsService' }],
      ['membership', '../services/relayToken.js', { kind: 'services', name: 'relayToken' }],
      ['membership', '../lib/roomPassword', { kind: 'other', name: 'lib/roomPassword' }],
      ['membership', 'ws', null],
    ]
    for (const [from, spec, expected] of cases) assert.deepEqual(resolveImport(from, spec), expected, `${from}: ${spec}`)
  })

  it('signaling imports only allowed services (never services/wsService)', () => {
    assert.deepEqual(servicesViolations(modules), [])
  })

  it('every signaling module has a layer', () => {
    assert.deepEqual(missingLayers(modules), [], 'add new signaling files to LAYERS (docs/server-architecture.md 2.1)')
  })

  it('signaling imports only go down a layer (or use an allowed same-layer edge)', () => {
    assert.deepEqual(layerViolations(modules), [])
  })

  it('signaling imports have no cycles', () => {
    assert.deepEqual(cycles(modules), [])
  })

  // 음성 케이스: 위반을 넣으면 판정 함수가 실제로 잡는다 (검사가 조용히 무력화되지 않게, S2 #87 리뷰 L)
  it('catches violations planted in a fake module table', () => {
    const fake: Modules = new Map([
      ['registry', ['./membership']], // 1 → 3: 위로 가는 간선
      ['chat', ['./metronome']], // 2 → 2: 허용 목록에 없는 같은 계층 간선
      ['nativeSession', ['./metronome']], // 2 → 2: 허용 목록에 있음 (위반 아님)
      ['membership', ['../signaling/dispatch.js']], // 3 → 4: 다른 표기로 위로
      ['auth', ['./sub/helper']], // 계층 없는 하위 폴더 모듈로
      ['sub/helper', ['./x']], // 하위 폴더 모듈 자체도 계층이 없다
      ['metronome', ['../services/wsService']], // 금지된 services
      ['descriptor', ['../services/stemEncoder', '../services/relayToken']], // 허용 목록 밖 services 하나
      ['performers', ['./requests']], // requests → performers와 함께 순환
      ['requests', ['./performers']],
    ])
    assert.deepEqual(layerViolations(fake).sort(), [
      'auth -> sub/helper (unknown module)',
      'chat(2) -> metronome(2)',
      'membership(3) -> dispatch(4)',
      'performers(2) -> requests(2)',
      'registry(1) -> membership(3)',
      'sub/helper -> sub/x (unknown module)',
    ])
    assert.deepEqual(missingLayers(fake), ['sub/helper'])
    assert.deepEqual(servicesViolations(fake).sort(), ['descriptor -> services/stemEncoder', 'metronome -> services/wsService'])
    assert.deepEqual(cycles(fake), ['performers -> requests -> performers'])
  })
})
