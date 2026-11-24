/**
 * 웹 첫 화면 묶음 예산(P2-25, #216 리뷰 L-3·L-4, P2-25b supabase 축소). e2e 빌드(운영처럼 Supabase 포함) 산출물을 직접 읽는다.
 * - 첫 화면이 받는 JS(index.html의 엔트리 + modulepreload) gzip 크기가 상한 아래인지 — 큰 의존성이 첫 묶음에 새로 실리면 실패.
 *   상한을 올려야 하면 이유를 PR에 적고 같이 올린다.
 * - 클라이언트가 쓰지 않는 supabase-js storage·functions(vite/supabaseSlim.ts가 빈 구현으로 바꿈)가 다시 실리지 않았는지.
 * - 데스크톱 전용 엔진 런타임(연주 세션 본체·상태기계·관람 방송)의 고유 문자열이 첫 화면 JS에 없고 nativeRuntime 묶음에만
 *   있는지 — 웹 코드가 그 모듈이나 그 하위 의존을 정적으로 끌어오면 실패(nativeSplit.test는 직접 import만 본다).
 */
import { readdirSync, readFileSync } from 'node:fs'
import { gzipSync } from 'node:zlib'
import { fileURLToPath } from 'node:url'
import { expect, test } from './test'

/**
 * 첫 화면 JS gzip 상한(kB, node zlib 기본 수준). 측정 193.45 kB(#216) → 186.89 kB(supabase storage·functions 제외)
 * → 179.36 kB(연주자 전용 UI 지연) + 약 3% 여유
 */
const FIRST_LOAD_JS_GZIP_KB = 185

/** 연주자 전용 UI(performerUi·EchoMeasure 지연 묶음)의 고유 문자열 — 첫 화면 JS에 없어야 한다(CSS는 첫 묶음에 그대로 둔다) */
const PERFORMER_UI_MARKERS: [chunk: RegExp, marker: string][] = [
  [/^performerUi-.*\.js$/, 'metronome-panel'],
  [/^performerUi-.*\.js$/, 'recording-folder'],
  [/^performerUi-.*\.js$/, 'engine-local-stats'],
  [/^EchoMeasure-.*\.js$/, 'echo-measure-btn'],
  // 내 채널 진단 줄 문구(engineDiagFormat — 송신 큐·릴레이, #250): EngineDiagLines와 함께 지연 묶음에만
  [/^performerUi-.*\.js$/, '송신 큐 p99'],
  [/^performerUi-.*\.js$/, '토큰 만료'],
  // 설정 화면 테스트 모니터의 드라이버 리셋 문구(testMonitorReset — AudioSettingsContext가 동적 import, #263 리뷰 M-1)
  [/^testMonitorReset-.*\.js$/, '테스트를 다시 시작했습니다'],
]

/** 런타임 묶음 모듈의 고유 문자열(mixBridge 워클릿 이름, sessionLogic 액션, performSession 문구·신호) */
const RUNTIME_MARKERS = [
  'mix-ring-processor',
  'rejectPeer',
  'addPeerFailed',
  'relay-token-request',
  '시그널링 서버 연결이 끊겨 연주를 중단했습니다',
  // 데스크톱 연주 중에만 나오는 문구(performMessages — 키 충돌, 장치 분실·드라이버 리셋 재시작 결과, #263 리뷰 M-1)
  '엔진 키 충돌이 반복',
  '쓰던 장치를 다시 열지 못해 연주를 중단',
]

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

/** 클라이언트가 쓰지 않아 뺀 supabase-js 부분(vite/supabaseSlim.ts)의 고유 문자열 — 다시 실리면(별칭 빠짐 등) 실패 */
const SUPABASE_EXCLUDED_MARKERS = ['object/upload/sign', 'object/list', 'iceberg', 'Failed to send a request to the Edge Function']

test('supabase-js의 storage·functions는 어느 JS 묶음에도 없다', () => {
  const all = readdirSync(dist + 'assets').filter(f => f.endsWith('.js')).map(f => read('assets/' + f)).join('\n')
  expect(all.includes('auth/v1'), '양성 대조: supabase auth가 번들에 있어야 함').toBe(true)
  for (const marker of SUPABASE_EXCLUDED_MARKERS) expect(all.includes(marker), `${marker}가 번들에 실림`).toBe(false)
})

test('연주자 전용 UI는 첫 화면 JS에 없고 지연 묶음에만 있다', () => {
  const firstLoad = firstLoadScripts().map(read).join('\n')
  const assets = readdirSync(dist + 'assets')
  for (const [chunk, marker] of PERFORMER_UI_MARKERS) {
    const files = assets.filter(f => chunk.test(f))
    expect(files, `${chunk} 묶음`).toHaveLength(1)
    expect(read('assets/' + files[0]).includes(marker), `양성 대조: ${marker}가 ${files[0]}에 있어야 함`).toBe(true)
    expect(firstLoad.includes(marker), `${marker}가 첫 화면 JS에 실림`).toBe(false)
  }
})

/**
 * 지연 라우트 화면(routes.tsx의 lazyScreen)의 고유 문자열 — 첫 화면 JS에 없고 제 묶음에만 있어야 한다(P3-20b).
 * 첫 화면 코드가 그 기능의 공개 API(index.ts)를 정적으로 import하면 Vite 8(Rolldown)은 화면 전체를 첫 묶음에 싣는다
 * (Rollup은 쓰이는 모듈만 남겼다 — 내비게이션의 profilePhotoOf가 그 경우였다).
 */
const LAZY_SCREEN_MARKERS: [chunk: RegExp, marker: string][] = [
  [/^profile-.*\.js$/, '프로필 설정'],
  [/^community-.*\.js$/, '새 게시물 작성'],
  [/^demo-recording-.*\.js$/, '개인 레코딩 스튜디오'],
]

test('지연 라우트 화면은 첫 화면 JS에 없고 제 묶음에만 있다', () => {
  const firstLoad = firstLoadScripts().map(read).join('\n')
  const assets = readdirSync(dist + 'assets')
  for (const [chunk, marker] of LAZY_SCREEN_MARKERS) {
    const files = assets.filter(f => chunk.test(f))
    expect(files, `${chunk} 묶음`).toHaveLength(1)
    expect(read('assets/' + files[0]).includes(marker), `양성 대조: ${marker}가 ${files[0]}에 있어야 함`).toBe(true)
    expect(firstLoad.includes(marker), `${marker}가 첫 화면 JS에 실림`).toBe(false)
  }
})

/**
 * 브라우저 지원 하한(vite/browserTargets.ts, P3-20b 리뷰 M-1): 미디어 쿼리 범위 문법 `(width<=768px)`은 Safari/iOS 16.4·Chrome 104
 * 미만에서 블록 전체가 무시된다. CSS 변환 대상(cssTarget)이 빠지거나 올라가면 Lightning CSS가 `max-width`를 범위 문법으로 바꾼다.
 */
test('빌드된 CSS의 미디어 쿼리에 범위 문법이 없다', () => {
  const css = readdirSync(dist + 'assets').filter(f => f.endsWith('.css')).map(f => read('assets/' + f)).join('\n')
  const preludes = [...css.matchAll(/@media([^{]*)\{/g)].map(m => m[1])
  expect(preludes.some(p => p.includes('max-width')), '양성 대조: max-width 미디어 쿼리가 있어야 함').toBe(true)
  const ranged = preludes.filter(p => /[<>]/.test(p))
  expect(ranged, `범위 문법 미디어 쿼리 ${ranged.length}개`).toEqual([])
})

/**
 * 첫 화면 JS 어디에도 없어야 하는 데스크톱 전용 모듈의 고유 문자열(어느 지연 묶음에 실리는지는 묶음 나누기에 맡긴다):
 * 루프백 보정값 저장소(loopbackOffsetStore — 저장 키 접두사·표시 문구), #263 리뷰 M-1.
 * 앱 업데이트 본체(native/updater·@tauri-apps/plugin-updater — 배너는 UpdateNoticeGate가 데스크톱에서만 지연 로드)와
 * 진단 내보내기 버튼(features/settings 공개 API가 데스크톱에서만 지연 로드), perf/lazy-updater
 */
const DESKTOP_ONLY_MARKERS = [
  'band.loopbackOffset.',
  '출력 지연 보정: 없음',
  'plugin:updater|check',
  '업데이트 다운로드 실패',
  '진단 내보내기',
]

test('데스크톱 전용 보조 모듈은 첫 화면 JS에 없다', () => {
  const firstLoad = firstLoadScripts().map(read).join('\n')
  const all = readdirSync(dist + 'assets').filter(f => f.endsWith('.js')).map(f => read('assets/' + f)).join('\n')
  for (const marker of DESKTOP_ONLY_MARKERS) {
    expect(all.includes(marker), `양성 대조: ${marker}가 어느 묶음엔가 있어야 함`).toBe(true)
    expect(firstLoad.includes(marker), `${marker}가 첫 화면 JS에 실림`).toBe(false)
  }
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
