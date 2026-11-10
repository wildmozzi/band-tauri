/**
 * 안전장치: 테스트 대상 빌드가 가짜 엔드포인트로 구워졌는지 확인한다.
 * 개발자 .env(운영 Supabase)가 섞여 들어가면 여기서 바로 실패한다(네트워크 guard와 이중 방어).
 */
import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { SUPABASE_URL } from './env'
import { expect, test } from './test'

test('E2E 빌드에는 가짜 Supabase 주소만 들어 있다', () => {
  const assets = fileURLToPath(new URL('./.dist/assets/', import.meta.url))
  const js = readdirSync(assets)
    .filter(f => f.endsWith('.js'))
    .map(f => readFileSync(assets + f, 'utf8'))
    .join('\n')
  expect(js).toContain(SUPABASE_URL)
  expect(js).not.toMatch(/https:\/\/[a-z0-9]+\.supabase\.co/)
  expect(js).not.toMatch(/fly\.dev/)
})
