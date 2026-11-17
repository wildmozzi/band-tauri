/**
 * 웹 첫 화면 묶음 예산(P2-25, #216 리뷰 L-3·L-4). e2e 빌드(운영처럼 Supabase 포함) 산출물을 직접 읽는다.
 * - 첫 화면이 받는 JS(index.html의 엔트리 + modulepreload) gzip 크기가 상한 아래인지 — 큰 의존성이 첫 묶음에 새로 실리면 실패.
 *   상한을 올려야 하면 이유를 PR에 적고 같이 올린다.
 * - 데스크톱 전용 엔진 런타임(연주 세션 본체·상태기계·관람 방송)의 고유 문자열이 첫 화면 JS에 없고 nativeRuntime 묶음에만
 *   있는지 — 웹 코드가 그 모듈이나 그 하위 의존을 정적으로 끌어오면 실패(nativeSplit.test는 직접 import만 본다).
 */
import { readdirSync, readFileSync } from 'node:fs'
import { gzipSync } from 'node:zlib'
import { fileURLToPath } from 'node:url'
import { expect, test } from './test'

/** 첫 화면 JS gzip 상한(kB, node zlib 기본 수준). 2026-11 측정 193.45 kB(#216) + 약 3% 여유 */
const FIRST_LOAD_JS_GZIP_KB = 200

/** 런타임 묶음 모듈의 고유 문자열(mixBridge 워클릿 이름, sessionLogic 액션, performSession 문구·신호) */
const RUNTIME_MARKERS = ['mix-ring-processor', 'rejectPeer', 'addPeerFailed', 'relay-token-request', '시그널링 서버 연결이 끊겨 연주를 중단했습니다']

const dist = fileURLToPath(new URL('./.dist/', import.meta.url))
const read = (rel: string) => readFileSync(dist + rel, 'utf8')

function firstLoadScripts(): string[] {
  const html = read('index.html')
  const entry = [...html.matchAll(/<script[^>]+src="\/([^"]+\.js)"/g)].map(m => m[1])
  const preload = [...html.matchAll(/<link[^>]+rel="modulepreload"[^>]+href="\/([^"]+\.js)"/g)].map(m => m[1])
  return [...entry, ...preload]
}

test('첫 화면 JS gzip 크기가 예산 안이다', () => {
  const scripts = firstLoadScripts()
  expect(scripts.length).toBeGreaterThan(0)
  const gzipKb = scripts.reduce((sum, f) => sum + gzipSync(readFileSync(dist + f)).length, 0) / 1000
  test.info().annotations.push({ type: 'first-load-js-gzip-kb', description: gzipKb.toFixed(2) })
  expect(gzipKb, `첫 화면 JS gzip ${gzipKb.toFixed(2)} kB`).toBeLessThanOrEqual(FIRST_LOAD_JS_GZIP_KB)
})

test('데스크톱 전용 엔진 런타임은 첫 화면 JS에 없고 nativeRuntime 묶음에만 있다', () => {
  const firstLoad = firstLoadScripts().map(read).join('\n')
  const runtimeChunks = readdirSync(dist + 'assets').filter(f => /^nativeRuntime-.*\.js$/.test(f))
  expect(runtimeChunks).toHaveLength(1)
  const runtime = read('assets/' + runtimeChunks[0])
  for (const marker of RUNTIME_MARKERS) {
    expect(runtime.includes(marker), `양성 대조: ${marker}가 런타임 묶음에 있어야 함`).toBe(true)
    expect(firstLoad.includes(marker), `${marker}가 첫 화면 JS에 실림`).toBe(false)
  }
})
