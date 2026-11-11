/**
 * 안전장치: 테스트 대상 빌드가 가짜 엔드포인트로 구워졌는지 확인한다.
 * 개발자 .env(운영 Supabase 등)가 섞여 들어가면 여기서 바로 실패한다(네트워크 guard와 이중 방어).
 */
import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { E2E_VITE_ENV } from './env'
import { expect, test } from './test'

function bundleJs(): string {
  const assets = fileURLToPath(new URL('./.dist/assets/', import.meta.url))
  return readdirSync(assets)
    .filter(f => f.endsWith('.js'))
    .map(f => readFileSync(assets + f, 'utf8'))
    .join('\n')
}

test('E2E 빌드에는 가짜 env 값만 들어 있다', () => {
  const js = bundleJs()

  // 양성 대조: 앱이 읽는 VITE_* 값이 전부 가짜 값으로 들어갔다(빠지면 하드코딩 fallback으로 샐 수 있다)
  // 예외: VITE_DEMUCS_API_URL은 VITE_USE_BACKEND_DEMUCS=true면 쓰는 경로가 상수 접힘으로 사라진다(죽은 코드 — 유출 아님)
  const foldedAway = new Set(['VITE_DEMUCS_API_URL'])
  for (const [key, value] of Object.entries(E2E_VITE_ENV)) {
    if (foldedAway.has(key)) continue
    expect(js.includes(value), `${key}=${value}가 번들에 없음`).toBe(true)
  }

  // 음성 대조: 운영 흔적
  const forbidden: [string, RegExp][] = [
    ['Supabase 호스트', /https?:\/\/[a-z0-9-]+\.supabase\.(co|in)/],
    ['fly.io 서버', /[a-z0-9-]+\.fly\.dev/],
    ['Vercel 도메인', /[a-z0-9-]+\.vercel\.app/],
    // Supabase anon/service 키는 JWT(eyJhbGciOi…)다. 가짜 env에는 JWT가 없다
    ['JWT 형태 키', /eyJhbGciOi[A-Za-z0-9_-]{10,}/],
    ['토스 실키', /\b(live|test)_(ck|sk|gck|gsk)_(?!e2e_placeholder)[A-Za-z0-9]+/],
  ]
  for (const [name, re] of forbidden) {
    expect(js.match(re)?.[0] ?? null, `번들에 ${name}`).toBeNull()
  }
})

test('운영 빌드에는 개발 서버 설정(개발 실행 전용)이 빠져 있다', () => {
  const js = bundleJs()
  // 양성 대조: 같은 화면(🎛️ 오디오 설정)의 한글 문구는 번들에 그대로 남는다(문자열 이스케이프로 빠진 척하는 것 방지)
  expect(js.includes('오디오 설정')).toBe(true)
  // import.meta.env.DEV로 감싼 /settings/audio·🎛️ 모달의 개발 서버 칸(P1-12b·P1-12d)
  expect(js.includes('개발 실행 전용')).toBe(false)
  expect(js.includes('개발 서버 주소')).toBe(false)
})
