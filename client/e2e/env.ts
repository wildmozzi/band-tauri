/**
 * E2E 빌드에 굽는 가짜 엔드포인트. 실제 서비스 주소가 아니며(.test 최상위 도메인은 예약어라 DNS로 풀리지 않는다),
 * 모든 요청은 Playwright가 브라우저 안에서 가로챈다(mocks/*). 가로채지 못한 요청은 guard가 막고 테스트를 실패시킨다.
 *
 * 운영 Supabase 프로젝트는 하나뿐이다 — E2E는 어떤 경우에도 실제 Supabase/서버/결제에 닿으면 안 된다.
 */
import { resolveE2EPort } from './port'

/** 실행마다 빈 포트(또는 환경변수 E2E_PORT) — port.ts 참고 */
export const E2E_PORT = resolveE2EPort()
export const E2E_BASE_URL = `http://127.0.0.1:${E2E_PORT}`

export const SUPABASE_URL = 'http://supabase.e2e.test'
export const API_URL = 'http://api.e2e.test'
export const SIGNALING_URL = 'ws://signaling.e2e.test'
export const TOSS_SCRIPT_HOST = 'js.tosspayments.com'

/** e2e/vite.config.ts가 빌드에 넣는 VITE_* 값 (placeholder만 — 비밀값 아님) */
export const E2E_VITE_ENV: Record<string, string> = {
  VITE_SUPABASE_URL: SUPABASE_URL,
  VITE_SUPABASE_ANON_KEY: 'e2e-anon-key-placeholder',
  VITE_API_URL: API_URL,
  VITE_SIGNALING_URL: SIGNALING_URL,
  VITE_DESKTOP_DOWNLOAD_URL: 'https://download.e2e.test/band-desktop',
  VITE_PUBLIC_WEB_URL: 'https://band.e2e.test',
  VITE_TOSS_PAYMENTS_CLIENT_KEY: 'test_ck_e2e_placeholder',
  VITE_DEMUCS_API_URL: 'http://demucs.e2e.test',
  VITE_USE_BACKEND_DEMUCS: 'true',
}

/** 요청을 허용(=mock이 처리)하는 호스트. 그 밖의 호스트는 guard가 차단·기록한다 */
export const MOCKED_HOSTS = new Set([
  new URL(SUPABASE_URL).host,
  new URL(API_URL).host,
  new URL(SIGNALING_URL).host,
  TOSS_SCRIPT_HOST,
])

/** supabase-js가 세션을 저장하는 localStorage 키 (sb-<호스트 첫 라벨>-auth-token) */
export const SUPABASE_STORAGE_KEY = `sb-${new URL(SUPABASE_URL).hostname.split('.')[0]}-auth-token`
