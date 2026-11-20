/**
 * 문서·코드 계약 대조(계약 감사): 시그널링 디스패치 표(`signaling/dispatch.ts`)의 메시지 종류가 설계 문서
 * `docs/server-architecture.md` 2.3절 발췌에 모두 있는지 본다. 새 메시지 종류를 더하면 발췌도 같이 고친다
 * (전에는 P2-9의 `demote-performer`가 발췌에서 빠져 있었다). 소스 텍스트를 읽는다 — 서버를 띄우지 않는다.
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

const ROOT = join(__dirname, '..', '..')
const read = (p: string) => readFileSync(join(ROOT, p), 'utf8').replace(/\r/g, '')

/** dispatch.ts 핸들러 표의 키(`'type': handler`) */
function dispatchTypes(): string[] {
  const src = read('server/src/signaling/dispatch.ts')
  const start = src.indexOf('const handlers')
  const end = src.indexOf('satisfies Record<string, Handler>', start)
  assert.ok(start >= 0 && end > start, 'dispatch.ts 핸들러 표를 찾지 못함 — 시험을 고칠 것')
  return [...src.slice(start, end).matchAll(/^\s*'([a-z-]+)':\s*handle[A-Za-z]+,/gm)].map((m) => m[1])
}

/** server-architecture.md 2.3절의 ```ts 발췌에 나오는 키 */
function docExcerptTypes(): string[] {
  const doc = read('docs/server-architecture.md')
  const sec = doc.indexOf('### 2.3 메시지 디스패치')
  assert.ok(sec >= 0, '2.3절 제목을 찾지 못함')
  const open = doc.indexOf('```ts', sec)
  const close = doc.indexOf('```', open + 5)
  return [...doc.slice(open, close).matchAll(/'([a-z-]+)':\s*handle[A-Za-z]+/g)].map((m) => m[1])
}

describe('문서 계약: 시그널링 메시지 종류', () => {
  it('디스패치 표의 모든 종류가 server-architecture.md 2.3절 발췌에 있고, 발췌에 없는 종류가 없다', () => {
    const code = dispatchTypes()
    const doc = docExcerptTypes()
    assert.ok(code.length >= 20, `디스패치 표 파싱 결과가 너무 적다(${code.length})`)
    // 발췌는 요청 계열을 "… 요청 계열 8개"로 줄여 적는다: 그 줄의 대표 두 개 외 요청 계열은 이름 패턴으로 허용한다
    const abbreviated = (t: string) => /perform-request$|-perform$|instrument-change/.test(t) && doc.includes('request-perform')
    const missing = code.filter((t) => !doc.includes(t) && !abbreviated(t))
    const stale = doc.filter((t) => !code.includes(t))
    assert.deepEqual(missing, [], `코드에 있고 문서 발췌에 없는 메시지 종류: ${missing.join(', ')}`)
    assert.deepEqual(stale, [], `문서 발췌에만 있는(코드에 없는) 메시지 종류: ${stale.join(', ')}`)
  })

  it('ping·auth(표 앞에서 처리)도 2.3절 본문에 적혀 있다', () => {
    const doc = read('docs/server-architecture.md')
    const sec = doc.slice(doc.indexOf('### 2.3 메시지 디스패치'))
    for (const t of ['ping', 'auth']) assert.ok(sec.includes(`\`${t}\``), `2.3절에 \`${t}\` 없음`)
  })
})
