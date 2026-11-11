import {
  DEFAULT_DEV_SERVER_PORT,
  devServerProxyPath,
  parseDevServerInput,
  type DevServerScheme,
  type DevServerTarget,
} from './devServerTarget'

/** 개발 실행에서 고른 서버 주소(설정 화면 "개발 서버"). 운영 빌드는 읽지 않는다 */
export const DEV_SERVER_STORAGE_KEY = 'band.devServer'

/**
 * Vite 개발 서버의 서버 프록시(vite/bandServerProxy)가 켜져 있으면 로컬 서버의 기본 스킴('http' | 'https').
 * 플러그인이 `vite`(serve)에서만 `__BAND_DEV_PROXY__` 상수로 정의한다 — 운영 빌드·vitest·e2e 빌드에서는 undefined.
 * VITE_ 접두사가 아니라 .env로는 켤 수 없고, 운영 빌드는 DEV 가드로 이 경로째 접힌다(PR #56 L1).
 */
function devProxyDefaultScheme(): DevServerScheme | null {
  if (!import.meta.env.DEV) return null
  const v = typeof __BAND_DEV_PROXY__ === 'string' ? __BAND_DEV_PROXY__ : undefined
  return v === 'http' || v === 'https' ? v : null
}

/** 개발 서버 프록시를 쓸 수 있는가(개발 실행 + 플러그인 켜짐) */
export function isDevServerProxyEnabled(): boolean {
  return devProxyDefaultScheme() != null
}

/** 저장된 개발 서버 입력값(없거나 저장소를 못 쓰면 null) */
export function getDevServerSetting(): string | null {
  try {
    return window.localStorage.getItem(DEV_SERVER_STORAGE_KEY)
  } catch {
    return null
  }
}

/** 개발 서버 입력값 저장(null/빈 값이면 지워서 기본값 localhost:8080). 서비스 모듈이 주소를 모듈 로드 때 읽으므로 저장 후 새로고침해야 한다 */
export function setDevServerSetting(value: string | null): void {
  try {
    if (value && value.trim()) window.localStorage.setItem(DEV_SERVER_STORAGE_KEY, value.trim())
    else window.localStorage.removeItem(DEV_SERVER_STORAGE_KEY)
  } catch {
    // 저장소를 못 쓰면 기본값으로 동작
  }
}

/** 개발 프록시가 넘겨 줄 대상. 프록시가 꺼져 있으면 null. 저장값이 틀렸으면 기본값(localhost:8080, 로컬 서버 인증서 유무에 따른 스킴) */
export function getDevServerTarget(): DevServerTarget | null {
  const scheme = devProxyDefaultScheme()
  if (!scheme) return null
  const saved = getDevServerSetting()
  return (saved && parseDevServerInput(saved, scheme)) || { scheme, host: 'localhost', port: DEFAULT_DEV_SERVER_PORT }
}

/**
 * VITE_*_URL 값이 없거나 'auto'일 때 쓰는 기본 서버 주소.
 * - 개발 실행(Vite 개발 서버): 같은 출처의 프록시 경로 `https://localhost:5173/__band_server/http/localhost:8080`.
 *   https 화면이 평문 로컬 서버(ws://)·다른 PC의 사설 IP 서버에 mixed content·CSP 제한 없이 붙는다(P1-12b)
 * - 그 밖(운영 빌드): 현재 페이지 호스트의 8080 포트, 페이지 프로토콜 그대로
 */
export function localServerUrl(): string {
  const target = getDevServerTarget()
  if (target) return `${window.location.origin}${devServerProxyPath(target)}`
  return `${window.location.protocol}//${window.location.hostname}:8080`
}

/**
 * API 서버 주소 (끝 슬래시 없음). 서비스 모듈(music/payment/room/storage/community)이 공유한다 (P3-10g).
 * - VITE_API_URL이 있으면 그 값
 * - 없거나 'auto'면 localServerUrl()
 * 호출할 때마다 읽는다(테스트의 vi.stubEnv 반영).
 */
export function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') return localServerUrl()
  return envUrl.replace(/\/+$/, '')
}

/**
 * API 서버를 설정했는지 (VITE_API_URL이 비어 있지 않음 — 'auto' 포함).
 * 설정 안 했으면 서버 조회를 건너뛰는 화면(구독 정보 등)이 쓴다. getApiUrl()은 미설정이어도 기본 주소를 돌려주므로 대신 쓸 수 없다.
 */
export function isApiUrlConfigured(): boolean {
  return Boolean(import.meta.env.VITE_API_URL)
}

/**
 * 시그널링 WebSocket 주소.
 * - VITE_SIGNALING_URL이 있으면 그 값
 * - 없거나 'auto'면 localServerUrl()의 http(s)를 ws(s)로 바꾼 값
 */
export function getSignalingUrl(): string {
  const envUrl = import.meta.env.VITE_SIGNALING_URL
  if (!envUrl || envUrl === 'auto') return localServerUrl().replace(/^http/, 'ws')
  return envUrl
}

/** API·시그널링이 둘 다 직접 지정(auto 아님)이면 개발 서버 설정이 아무 효과가 없다 */
export function usesAutoServerUrl(): boolean {
  const auto = (v: string | undefined) => !v || v === 'auto'
  return auto(import.meta.env.VITE_API_URL) || auto(import.meta.env.VITE_SIGNALING_URL)
}
