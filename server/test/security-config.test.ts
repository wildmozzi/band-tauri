/**
 * M2(insecureDevMode) / M5(CORS) 설정 회귀 테스트
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { isInsecureDevMode, resolveCorsOrigin, TAURI_ORIGINS } from '../src/lib/securityConfig'

describe('isInsecureDevMode', () => {
  it('is on only with development + explicit opt-in + no Supabase', () => {
    assert.equal(isInsecureDevMode(false, { NODE_ENV: 'development', ALLOW_INSECURE_WS: '1' }), true)
  })
  const off: [boolean, Record<string, string | undefined>][] = [
    [true, { NODE_ENV: 'development', ALLOW_INSECURE_WS: '1' }], // Supabase 설정됨
    [false, { NODE_ENV: 'development' }], // opt-in 없음
    [false, { NODE_ENV: 'development', ALLOW_INSECURE_WS: 'true' }],
    [false, { ALLOW_INSECURE_WS: '1' }], // NODE_ENV 비어 있음 (이전에는 켜졌음)
    [false, { NODE_ENV: 'production', ALLOW_INSECURE_WS: '1' }],
    [false, { NODE_ENV: 'test', ALLOW_INSECURE_WS: '1' }],
    [false, {}],
  ]
  for (const [hasSupabase, env] of off) {
    it(`is off for hasSupabase=${hasSupabase} env=${JSON.stringify(env)}`, () => {
      assert.equal(isInsecureDevMode(hasSupabase, env), false)
    })
  }
})

describe('resolveCorsOrigin', () => {
  it('uses CORS_ORIGINS plus Tauri origins when set', () => {
    const r = resolveCorsOrigin({ CORS_ORIGINS: 'https://a.example, https://b.example', NODE_ENV: 'production' })
    assert.equal(r.mode, 'list')
    assert.deepEqual(r.origin, ['https://a.example', 'https://b.example', ...TAURI_ORIGINS])
  })
  it('never allows all origins outside development', () => {
    for (const NODE_ENV of ['production', undefined, 'test']) {
      const r = resolveCorsOrigin({ NODE_ENV })
      assert.equal(r.mode, 'tauri-only')
      assert.deepEqual(r.origin, TAURI_ORIGINS)
    }
  })
  it('allows all origins only in development without CORS_ORIGINS', () => {
    assert.equal(resolveCorsOrigin({ NODE_ENV: 'development' }).origin, true)
  })
})
