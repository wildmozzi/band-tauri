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

/**
 * 엄격한 IPv4 파서: 정규 점-10진 4옥텟만(각 0~255, 앞자리 0 거부).
 * `010.8.8.8`처럼 앞자리 0이 붙은 표기는 glibc·macOS의 inet_aton이 8진수로 읽어(→ 8.8.8.8) 검사한 주소와
 * 실제 연결 주소가 달라지므로 받지 않는다(PR #56 M2). 16진(0x7f)·정수(2130706433)·축약 표기도 거부.
 */
export function parseIpv4(host: string): number[] | null {
  const parts = host.split('.')
  if (parts.length !== 4) return null
  const nums: number[] = []
  for (const p of parts) {
    if (!/^(0|[1-9]\d{0,2})$/.test(p)) return null
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

/** 대상이 이 PC 자신(localhost·127/8)인가 */
export function isLoopbackTarget(host: string): boolean {
  if (host === 'localhost') return true
  const ip = parseIpv4(host)
  return ip != null && ip[0] === 127
}

/** 소켓 원격 주소(Node `socket.remoteAddress`)가 루프백인가: 127/8, ::1, ::ffff:127.x */
export function isLoopbackRemoteAddress(addr: string | undefined): boolean {
  if (!addr) return false
  const a = addr.toLowerCase()
  if (a === '::1') return true
  const ip = parseIpv4(a.startsWith('::ffff:') ? a.slice('::ffff:'.length) : a)
  return ip != null && ip[0] === 127
}

/** 검사를 통과한 호스트를 파싱한 숫자로 다시 조립한다(검사한 주소와 연결하는 주소가 같도록) */
function canonicalHost(host: string): string {
  const ip = parseIpv4(host)
  return ip ? ip.join('.') : host
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
  return { scheme, host: canonicalHost(host), port }
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
  return { kind: 'ok', target: { scheme: m[1] as DevServerScheme, host: canonicalHost(host), port }, path }
}

/**
 * 프록시가 넘겨 주는 대상 경로를 밴드 서버(server/src/app.ts·wsService)의 표면으로 고정한다(PR #56 H1).
 * - HTTP: `/api/...`, `/uploads/...`, `/health`
 * - WebSocket 업그레이드: 시그널링(`/` + 쿼리)만
 * `.`·`..` 경로 조각(퍼센트 인코딩 포함)과 역슬래시는 거부한다(업스트림이 정규화하면 다른 경로가 됨).
 */
export function isAllowedProxyPath(path: string, upgrade: boolean): boolean {
  const q = path.indexOf('?')
  const pathname = q >= 0 ? path.slice(0, q) : path
  if (pathname.includes('\\') || /%5c/i.test(pathname)) return false
  for (const seg of pathname.split('/')) {
    if (/^(\.|%2e){1,2}$/i.test(seg)) return false
  }
  if (upgrade) return pathname === '/'
  return pathname === '/health' || pathname.startsWith('/api/') || pathname.startsWith('/uploads/')
}

export interface ProxyRequestInfo {
  /** 요청 소켓의 원격 주소 */
  remoteAddress: string | undefined
  /** Origin 헤더(없으면 undefined) */
  origin: string | undefined
  /** Sec-Fetch-Site 헤더(없으면 undefined) */
  secFetchSite: string | undefined
  /** 업그레이드 요청이면 Upgrade 헤더 값, 일반 HTTP면 null */
  upgrade: string | null
}

export interface ProxyPolicy {
  /** 허용 출처(개발 페이지 자신의 출처). 소문자 정확 일치 */
  allowedOrigins: ReadonlySet<string>
  /** 허용 대상 포트(밴드 서버 포트) */
  allowedPorts: ReadonlySet<number>
}

/**
 * 프록시 요청 허용 여부. 거부면 사유 문자열, 허용이면 null. Vite 플러그인이 HTTP·업그레이드 모두에 쓴다.
 * - H1: 업그레이드는 `websocket`만. 대상 포트·경로는 밴드 서버로 고정. 요청자가 루프백이 아니면(LAN 기기)
 *   대상이 루프백인 요청 거부 → LAN 기기가 개발 PC의 127.0.0.1 전용 포트에 닿지 못한다.
 * - M1: Origin이 있으면 허용 출처만. 없으면 Sec-Fetch-Site가 same-origin/none일 때만(교차 출처 no-cors GET 차단).
 */
export function checkProxyRequest(
  req: ProxyRequestInfo,
  target: DevServerTarget,
  path: string,
  policy: ProxyPolicy,
): string | null {
  if (req.upgrade != null && req.upgrade.trim().toLowerCase() !== 'websocket') return 'WebSocket 업그레이드만 허용'
  if (req.origin !== undefined) {
    if (!policy.allowedOrigins.has(req.origin.toLowerCase())) return `허용되지 않은 출처(${req.origin})`
  } else if (req.secFetchSite !== undefined && req.secFetchSite !== 'same-origin' && req.secFetchSite !== 'none') {
    return `교차 출처 요청(Sec-Fetch-Site: ${req.secFetchSite})`
  }
  if (!policy.allowedPorts.has(target.port)) {
    return `허용되지 않은 대상 포트 ${target.port}(허용: ${Array.from(policy.allowedPorts).join(', ')})`
  }
  if (!isAllowedProxyPath(path, req.upgrade != null)) return '밴드 서버 경로(/api/·/uploads/·/health, WebSocket은 /)만 허용'
  if (isLoopbackTarget(target.host) && !isLoopbackRemoteAddress(req.remoteAddress)) {
    return '다른 기기에서는 이 PC의 루프백(localhost) 대상으로 프록시할 수 없음 — 서버 PC의 사설 IP를 지정'
  }
  return null
}
