import type { CorsOptions } from 'cors'

type Env = Record<string, string | undefined>

/** Tauri 앱 origin (Windows: http(s)://tauri.localhost, macOS/Linux: tauri://localhost) — 항상 허용 */
export const TAURI_ORIGINS = ['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost']

/**
 * CORS 허용 origin 결정 (M5)
 * - CORS_ORIGINS(쉼표 구분)가 있으면 그 목록 + Tauri origin
 * - 없으면: NODE_ENV==='development'일 때만 전체 허용, 그 외(운영 포함)는 Tauri origin만 (브라우저 origin 거부)
 */
export function resolveCorsOrigin(env: Env = process.env): { origin: CorsOptions['origin']; mode: 'list' | 'dev-all' | 'tauri-only' } {
  const list = (env.CORS_ORIGINS || '')
    .split(',')
    .map(s => s.trim())
    .filter(Boolean)
  if (list.length > 0) return { origin: [...list, ...TAURI_ORIGINS], mode: 'list' }
  if (env.NODE_ENV === 'development') return { origin: true, mode: 'dev-all' }
  return { origin: TAURI_ORIGINS, mode: 'tauri-only' }
}

/**
 * WS 비보안 개발 모드 (M2): 클라이언트가 보낸 userId/isHost를 신뢰한다.
 * Supabase 미설정 + NODE_ENV==='development' + ALLOW_INSECURE_WS==='1' 세 조건이 모두 참일 때만 켜진다.
 */
export function isInsecureDevMode(hasSupabase: boolean, env: Env = process.env): boolean {
  return !hasSupabase && env.NODE_ENV === 'development' && env.ALLOW_INSECURE_WS === '1'
}
