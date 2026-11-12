/**
 * 스타일 스냅샷 비교 (P3-2 ③-3): node e2e/tools/style-diff.ts <기준 폴더> <비교 폴더> [--max 200]
 * 화면(JSON 파일)·요소(DOM 경로)·속성별 차이를 출력한다. 차이가 하나라도 있으면 종료 코드 1.
 * (Node 22.6+의 타입 제거 실행. 사용법은 style-snapshot.tool.ts 머리말)
 */
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

type Styles = Record<string, string>
type Entry = { c: string; s: Styles; before?: Styles; after?: Styles }
type Dump = Record<string, Entry>

const args = process.argv.slice(2)
const maxIdx = args.indexOf('--max')
const max = maxIdx >= 0 ? Number(args[maxIdx + 1]) : 200
const [baseDir, nextDir] = args.filter((_, i) => maxIdx < 0 || (i !== maxIdx && i !== maxIdx + 1))
if (!baseDir || !nextDir) {
  console.error('사용법: node e2e/tools/style-diff.ts <기준 폴더> <비교 폴더> [--max 200]')
  process.exit(2)
}

const load = (dir: string, file: string): Dump => JSON.parse(readFileSync(join(dir, file), 'utf8')) as Dump
const files = [...new Set([...readdirSync(baseDir), ...readdirSync(nextDir)])].filter(f => f.endsWith('.json')).sort()

let total = 0
let printed = 0
const print = (line: string) => {
  total++
  if (printed < max) {
    console.log(line)
    printed++
  }
}

function diffStyles(screen: string, key: string, part: string, a: Styles | undefined, b: Styles | undefined, cls: string) {
  if (!a && !b) return
  if (!a || !b) {
    print(`[${screen}] ${key}${part} (${cls}): ${a ? '있음 → 없음' : '없음 → 있음'}`)
    return
  }
  for (const prop of new Set([...Object.keys(a), ...Object.keys(b)])) {
    if (a[prop] !== b[prop]) print(`[${screen}] ${key}${part} (${cls}) ${prop}: ${a[prop]} → ${b[prop]}`)
  }
}

for (const file of files) {
  const screen = file.replace(/\.json$/, '')
  if (!existsSync(join(baseDir, file)) || !existsSync(join(nextDir, file))) {
    print(`[${screen}] 화면 파일이 한쪽에만 있다`)
    continue
  }
  const a = load(baseDir, file)
  const b = load(nextDir, file)
  for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
    const ea = a[key]
    const eb = b[key]
    if (!ea || !eb) {
      print(`[${screen}] ${key}: 요소가 ${ea ? '사라짐' : '새로 생김'} (${(ea ?? eb).c})`)
      continue
    }
    const cls = ea.c === eb.c ? ea.c : `${ea.c} → ${eb.c}`
    diffStyles(screen, key, '', ea.s, eb.s, cls)
    diffStyles(screen, key, '::before', ea.before, eb.before, cls)
    diffStyles(screen, key, '::after', ea.after, eb.after, cls)
  }
}

const elements = files.reduce((n, f) => (existsSync(join(baseDir, f)) ? n + Object.keys(load(baseDir, f)).length : n), 0)
console.log(`\n화면 ${files.length}개, 요소 ${elements}개 비교 — 차이 ${total}건${total > printed ? ` (앞 ${printed}건만 출력)` : ''}`)
process.exit(total === 0 ? 0 : 1)
