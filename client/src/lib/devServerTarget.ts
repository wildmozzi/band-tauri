/**
 * 개발 실행 전용 서버 프록시(P1-12b)의 경로 규칙. 브라우저(lib/apiUrl)와 Vite 개발 서버 플러그인(vite/bandServerProxy)이 함께 쓴다.
 *
 * 개발 모드에서 VITE_API_URL/VITE_SIGNALING_URL이 'auto'(또는 비어 있음)면 앱은 서버에 직접 붙지 않고
 * 자기 Vite 개발 서버(https://localhost:5173)의 `/__band_server/<http|https>/<host>:<port>/...`로 붙는다.
 * Vite가 그 요청을 대상 서버로 넘긴다. 페이지와 같은 출처라 https 화면에서 평문 ws 서버(mixed content)·CSP 제한 없이 붙는다.
 *
 * 대상은 localhost·루프백·사설 IPv4(10/8, 172.16/12, 192.168/16)만 허용한다. 도메인 이름은 받지 않는다(DNS 재바인딩 방지).
 * 이 파일은 DOM·Node API를 쓰지 않는다(양쪽에서 import).
 */

export const DEV_SERVER_PROXY_PREFIX = '/__band_server'
export const DEFAULT_DEV_SERVER_PORT = 8080

export type DevServerScheme = 'http' | 'https'

export interface DevServerTarget {
  scheme: DevServerScheme
  /** 'localhost' 또는 점 표기 IPv4 */
  host: string
  port: number
}

function parseIpv4(host: string): number[] | null {
  const parts = host.split('.')
  if (parts.length !== 4) return null
  const nums: number[] = []
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) return null
    const n = Number(p)
    if (n > 255) return null
    nums.push(n)
  }
  return nums
}

/** localhost, 127/8, 10/8, 172.16/12, 192.168/16만 true */
export function isLocalOrPrivateHost(host: string): boolean {
  if (host === 'localhost') return true
  const ip = parseIpv4(host)
  if (!ip) return false
  const [a, b] = ip
  return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)
}

function parsePort(s: string | undefined): number | null {
  if (s === undefined || s === '') return DEFAULT_DEV_SERVER_PORT
  if (!/^\d{1,5}$/.test(s)) return null
  const n = Number(s)
  return n >= 1 && n <= 65535 ? n : null
}

/**
 * 설정 화면 입력값 → 대상. "192.168.0.10", "192.168.0.10:8080", "http://192.168.0.10:8080", "https://localhost:8443".
 * 스킴이 없으면 defaultScheme, 포트가 없으면 8080. 경로·쿼리가 붙거나 허용 범위 밖 주소면 null.
 */
export function parseDevServerInput(input: string, defaultScheme: DevServerScheme): DevServerTarget | null {
  let s = input.trim()
  if (!s) return null
  let scheme = defaultScheme
  const m = /^(https?):\/\//i.exec(s)
  if (m) {
    scheme = m[1].toLowerCase() as DevServerScheme
    s = s.slice(m[0].length)
  }
  s = s.replace(/\/+$/, '')
  const hp = /^([^:/?#]+)(?::([^/?#]*))?$/.exec(s)
  if (!hp) return null
  const host = hp[1].toLowerCase()
  const port = parsePort(hp[2])
  if (port == null || !isLocalOrPrivateHost(host)) return null
  return { scheme, host, port }
}

/** 대상 → 프록시 경로 접두사(끝 슬래시 없음) */
export function devServerProxyPath(t: DevServerTarget): string {
  return `${DEV_SERVER_PROXY_PREFIX}/${t.scheme}/${t.host}:${t.port}`
}

/** 표시용 "http://192.168.0.10:8080" */
export function formatDevServerTarget(t: DevServerTarget): string {
  return `${t.scheme}://${t.host}:${t.port}`
}

export type ProxyPathMatch =
  /** 프록시 경로가 아님(다른 핸들러로 넘김) */
  | { kind: 'none' }
  /** 프록시 경로지만 대상이 허용 범위 밖이거나 형식이 틀림(거부) */
  | { kind: 'invalid' }
  | { kind: 'ok'; target: DevServerTarget; path: string }

/**
 * 요청 URL(경로+쿼리) → 대상과 대상에 보낼 경로. Vite 플러그인이 HTTP·WebSocket 업그레이드 둘 다에 쓴다.
 * `/__band_server/http/192.168.0.10:8080/api/rooms?x=1` → http, 192.168.0.10, 8080, `/api/rooms?x=1`
 */
export function matchDevServerProxyPath(url: string): ProxyPathMatch {
  if (url !== DEV_SERVER_PROXY_PREFIX && !url.startsWith(`${DEV_SERVER_PROXY_PREFIX}/`) && !url.startsWith(`${DEV_SERVER_PROXY_PREFIX}?`)) {
    return { kind: 'none' }
  }
  const m = /^\/__band_server\/(https?)\/([^/?#]+)([/?][^#]*)?$/.exec(url)
  if (!m) return { kind: 'invalid' }
  const hp = /^([^:]+):(\d{1,5})$/.exec(m[2])
  if (!hp) return { kind: 'invalid' }
  const host = hp[1].toLowerCase()
  const port = parsePort(hp[2])
  if (port == null || !isLocalOrPrivateHost(host)) return { kind: 'invalid' }
  let path = m[3] ?? '/'
  if (path.startsWith('?')) path = `/${path}`
  return { kind: 'ok', target: { scheme: m[1] as DevServerScheme, host, port }, path }
}
