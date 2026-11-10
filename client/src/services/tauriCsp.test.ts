/**
 * P3-6 리뷰 M3: 데스크톱(Tauri) WebView에서 Ogg Opus wasm 폴백 디코더가 동작하려면
 * CSP script-src에 'wasm-unsafe-eval'이 있어야 한다(WebAssembly.compile 허용, eval은 여전히 차단).
 */
import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'

const conf = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'))

describe('tauri.conf.json CSP', () => {
  for (const key of ['csp', 'devCsp'] as const) {
    it(`${key} allows WebAssembly compilation but not eval`, () => {
      const scriptSrc: string = conf.app.security[key]['script-src']
      expect(scriptSrc.split(/\s+/)).toContain("'wasm-unsafe-eval'")
      expect(scriptSrc).not.toContain("'unsafe-eval'")
    })
  }
})
