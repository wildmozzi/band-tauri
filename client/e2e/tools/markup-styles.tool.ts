/**
 * 마크업 조각 계산 스타일 비교 (P3-2 ③-3) — 웹 E2E로 그릴 수 없는 데스크톱(Tauri) 전용 화면(합주실 연주 UI 등)의 CSS 분리 검증용.
 * 빌드된 CSS 폴더 두 개(기준·비교)를 각각 빈 페이지에 붙이고, 컴포넌트 JSX에서 옮겨 적은 마크업 조각의 계산 스타일을
 * (필요하면 :hover 상태로) 비교한다. CI 밖 수동 실행:
 *   MARKUP_BASE_CSS=<기준 dist/assets> MARKUP_AFTER_CSS=<비교 dist/assets> npx playwright test -c e2e/tools/style-snapshot.config.ts markup-styles
 * 조각 목록은 tools/markupFragments.ts. 지연 청크 CSS는 메인 CSS 뒤에 붙인다(조각마다 지정).
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { expect, test, type Page } from '@playwright/test'
import { MARKUP_FRAGMENTS, STYLE_PROPS } from './markupFragments'

const baseDir = process.env.MARKUP_BASE_CSS
const afterDir = process.env.MARKUP_AFTER_CSS

/** 메인 CSS(가장 큰 파일) + 이름이 맞는 지연 청크 CSS를 순서대로 이어 붙인다 */
function cssText(dir: string, chunks: readonly string[]): string {
  const files = readdirSync(dir).filter(f => f.endsWith('.css'))
  const main = files.reduce((a, b) => (statSync(join(dir, a)).size >= statSync(join(dir, b)).size ? a : b))
  const extra = files.filter(f => f !== main && chunks.some(c => readFileSync(join(dir, f), 'utf8').includes(c)))
  return [main, ...extra].map(f => readFileSync(join(dir, f), 'utf8')).join('\n')
}

async function measure(page: Page, css: string, html: string, hover: string | undefined) {
  await page.setContent(`<!doctype html><html><head><style>${css}</style><style>*,*::before,*::after{transition:none!important;animation:none!important}</style></head><body>${html}</body></html>`)
  if (hover) await page.hover(hover, { force: true })
  return page.evaluate(props => {
    const out: Record<string, Record<string, string>> = {}
    document.querySelectorAll('[data-probe]').forEach(el => {
      const cs = getComputedStyle(el)
      const s: Record<string, string> = {}
      for (const p of props) s[p] = cs.getPropertyValue(p)
      out[el.getAttribute('data-probe')!] = s
    })
    return out
  }, STYLE_PROPS)
}

test.use({ viewport: { width: 1280, height: 800 } })

for (const frag of MARKUP_FRAGMENTS) {
  test(`조각: ${frag.name}`, async ({ page }) => {
    test.skip(!baseDir || !afterDir, 'MARKUP_BASE_CSS·MARKUP_AFTER_CSS를 지정하세요')
    const before = await measure(page, cssText(baseDir!, frag.chunks ?? []), frag.html, frag.hover)
    const after = await measure(page, cssText(afterDir!, frag.chunks ?? []), frag.html, frag.hover)
    expect(Object.keys(before).length).toBeGreaterThan(0)
    expect(after).toEqual(before)
  })
}
