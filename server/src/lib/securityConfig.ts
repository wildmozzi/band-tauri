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

/**
 * 이 서버의 공개 주소(PUBLIC_API_URL) 확인 (P3-10g, PR #25 L2).
 * 게시글 이미지 조회 URL(`${PUBLIC_API_URL}/api/storage/post-images/...`)이 posts.images에 저장되므로,
 * 없으면 상대 경로가 저장돼 다른 오리진 클라이언트(웹 관람·band-web)에서 깨지고 수정 왕복 시 "기존 이미지" 일치가 깨진다.
 * - 운영(NODE_ENV=production): 없거나 http(s) 절대 URL이 아니면 throw → 서버 시작 실패
 * - 그 외: 경고만 (로컬 개발은 상대 경로로 동작)
 * 반환값: 끝 슬래시를 뗀 URL, 없으면 null
 */
export function requirePublicApiUrl(env: Env = process.env, warn: (msg: string) => void = console.warn): string | null {
  const raw = (env.PUBLIC_API_URL ?? '').trim()
  const production = env.NODE_ENV === 'production'
  let problem: string | null = null
  if (!raw) {
    problem = 'PUBLIC_API_URL is not set'
  } else {
    try {
      const u = new URL(raw)
      if (u.protocol !== 'https:' && u.protocol !== 'http:') problem = `PUBLIC_API_URL must be an http(s) URL (got ${u.protocol})`
      else if (production && u.protocol !== 'https:') problem = 'PUBLIC_API_URL must use https in production'
    } catch {
      problem = 'PUBLIC_API_URL is not a valid absolute URL'
    }
  }
  if (problem) {
    const hint = 'Set it to this server\'s public origin, e.g. `fly secrets set PUBLIC_API_URL=https://<app>.fly.dev` (see server/.env.example).'
    if (production) throw new Error(`[CONFIG] ${problem}. ${hint}`)
    warn(`[CONFIG] ${problem} — post image URLs will be stored as relative paths (dev only). ${hint}`)
    return null
  }
  return raw.replace(/\/+$/, '')
}
