/**
 * 개발 실행 전용 서버 프록시(P1-12b). `vite`(serve)에서만 켜진다 — 운영 빌드(`vite build`)·vitest에는 없다.
 *
 * 앱(https://localhost:5173)이 `/__band_server/<http|https>/<host>:<port>/...`로 보낸 HTTP 요청과 WebSocket 업그레이드를
 * 대상 서버로 그대로 넘긴다. 대상은 localhost·루프백·사설 IPv4만(src/shared/lib/devServerTarget). 그래서
 * - 인증서 없는 로컬 서버(ws://)에 https 화면이 붙고(VITE_SIGNALING_URL=auto가 wss://localhost:8080을 만들던 문제),
 * - 두 번째 PC가 첫 PC의 서버(사설 IP)에 붙는다(mixed content·devCsp 확장, netsh portproxy 없이).
 * 클라이언트 IP 헤더(X-Forwarded-For)는 붙이지 않는다: 서버는 프록시를 띄운 PC의 IP를 클라이언트 IP로 본다.
 *
 * 개발 서버는 LAN에 열려 있으므로(`host: true`) 범용 터널이 되지 않게 막는다(PR #56 리뷰, checkProxyRequest):
 * - 업그레이드는 `Upgrade: websocket`만, 업스트림이 `101`로 답할 때만 양방향 파이프를 연다(H1)
 * - 대상 포트는 밴드 서버 포트(기본 8080·server/.env PORT·BAND_DEV_PROXY_PORTS), 경로는 /api/·/uploads/·/health·시그널링 / 만(H1)
 * - LAN 기기(루프백이 아닌 요청자)는 루프백 대상으로 프록시할 수 없다(H1)
 * - Origin은 이 개발 서버 자신의 출처만(M1)
 */
import fs from 'node:fs'
import http from 'node:http'
import https from 'node:https'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import tls from 'node:tls'
import type { Duplex } from 'node:stream'
import type { Connect, Plugin } from 'vite'
import {
  DEFAULT_DEV_SERVER_PORT,
  checkProxyRequest,
  matchDevServerProxyPath,
  type DevServerTarget,
  type ProxyPolicy,
} from '../src/shared/lib/devServerTarget'

/** 홉 단위 헤더: 넘기지 않는다(HTTP/2 응답에 넣으면 오류) */
const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-connection', 'transfer-encoding', 'upgrade', 'te', 'trailer',
  'proxy-authenticate', 'proxy-authorization',
])

/** 업스트림 업그레이드 응답 헤더 최대 크기 */
const MAX_UPGRADE_RESPONSE_HEAD = 16 * 1024

function forwardHeaders(headers: http.IncomingHttpHeaders, target: DevServerTarget): http.OutgoingHttpHeaders {
  const out: http.OutgoingHttpHeaders = {}
  for (const [k, v] of Object.entries(headers)) {
    // HTTP/2 의사 헤더(:authority 등)·홉 단위 헤더 제외
    if (v === undefined || k.startsWith(':') || HOP_BY_HOP.has(k)) continue
    out[k] = v
  }
  out.host = `${target.host}:${target.port}`
  return out
}

function headerValue(v: string | string[] | undefined): string | undefined {
  return Array.isArray(v) ? v[0] : v
}

function proxyHttp(req: http.IncomingMessage, res: http.ServerResponse, target: DevServerTarget, reqPath: string) {
  const mod = target.scheme === 'https' ? https : http
  const upstream = mod.request(
    {
      host: target.host,
      port: target.port,
      method: req.method,
      path: reqPath,
      headers: forwardHeaders(req.headers, target),
      // 개발용 로컬·사설 서버의 자체 서명 인증서(mkcert 루트가 다른 PC에 없을 수 있음)
      ...(target.scheme === 'https' ? { rejectUnauthorized: false } : {}),
    },
    (up) => {
      const headers: http.OutgoingHttpHeaders = {}
      for (const [k, v] of Object.entries(up.headers)) {
        if (v !== undefined && !HOP_BY_HOP.has(k)) headers[k] = v
      }
      res.writeHead(up.statusCode ?? 502, headers)
      up.pipe(res)
    },
  )
  upstream.on('error', (err) => {
    if (!res.headersSent) {
      res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8' })
      res.end(`개발 서버 프록시: ${target.scheme}://${target.host}:${target.port} 연결 실패 (${err.message})`)
    } else {
      res.destroy()
    }
  })
  // 클라이언트가 먼저 끊으면(SSE·롱폴 중단 등) 업스트림 요청도 정리(L2). 정상 완료 후의 close에서는 이미 끝나 있다
  res.on('close', () => upstream.destroy())
  req.pipe(upstream)
}

function proxyUpgrade(req: http.IncomingMessage, socket: Duplex, head: Buffer, target: DevServerTarget, reqPath: string) {
  let connected = false
  let piped = false
  /** 101이 아닌 응답을 돌려주고 닫는 중(끝까지 흘려보내도록 소켓을 바로 부수지 않음) */
  let refused = false
  // 업스트림이 101로 답하기 전에는 클라이언트 바이트를 넘기지 않는다
  socket.pause()
  const onConnect = () => {
    connected = true
    const lines = [`${req.method ?? 'GET'} ${reqPath} HTTP/1.1`]
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const name = req.rawHeaders[i]
      if (name.startsWith(':') || name.toLowerCase() === 'host') continue
      lines.push(`${name}: ${req.rawHeaders[i + 1]}`)
    }
    lines.push(`Host: ${target.host}:${target.port}`)
    upstream.write(`${lines.join('\r\n')}\r\n\r\n`)
    if (head.length) upstream.write(head)
  }
  const upstream: net.Socket = target.scheme === 'https'
    ? tls.connect({ host: target.host, port: target.port, rejectUnauthorized: false, servername: net.isIP(target.host) ? undefined : target.host }, onConnect)
    : net.connect({ host: target.host, port: target.port }, onConnect)

  // 업스트림 응답 헤더를 다 받을 때까지 모은 뒤, 101이면 파이프·아니면 그 응답만 돌려주고 닫는다(H1)
  let buf = Buffer.alloc(0)
  const onData = (chunk: Buffer) => {
    buf = Buffer.concat([buf, chunk])
    const end = buf.indexOf('\r\n\r\n')
    if (end < 0) {
      if (buf.length > MAX_UPGRADE_RESPONSE_HEAD) close()
      return
    }
    upstream.off('data', onData)
    const statusLine = buf.subarray(0, buf.indexOf('\r\n')).toString('latin1')
    if (!/^HTTP\/1\.1 101 /.test(statusLine)) {
      refused = true
      socket.end(buf)
      socket.resume() // 남은 클라이언트 바이트는 어디에도 넘기지 않고 버린다(FIN을 읽어 소켓이 닫히도록)
      upstream.destroy()
      return
    }
    piped = true
    socket.write(buf)
    upstream.pipe(socket)
    socket.pipe(upstream)
    socket.resume()
  }
  upstream.on('data', onData)

  const close = () => { upstream.destroy(); socket.destroy() }
  upstream.on('error', (err) => {
    // 연결 전 실패는 원인을 한 줄로 알려 준다(L3)
    if (!connected && !piped && socket.writable) {
      socket.end(`HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n개발 서버 프록시: ${target.scheme}://${target.host}:${target.port} 연결 실패 (${err.message})`)
      return
    }
    close()
  })
  socket.on('error', close)
  upstream.on('close', () => { if (!refused && (piped || connected)) socket.destroy() })
  socket.on('close', () => upstream.destroy())
}

/** 밴드 서버 포트: 기본 8080, server/.env의 PORT, 환경 변수 BAND_DEV_PROXY_PORTS(쉼표 구분) */
export function resolveAllowedPorts(serverDir: string, env: NodeJS.ProcessEnv = process.env): Set<number> {
  const ports = new Set<number>([DEFAULT_DEV_SERVER_PORT])
  const add = (s: string | undefined) => {
    const n = Number(s?.trim())
    if (Number.isInteger(n) && n >= 1 && n <= 65535) ports.add(n)
  }
  try {
    const m = /^\s*PORT\s*=\s*["']?(\d+)["']?\s*$/m.exec(fs.readFileSync(path.join(serverDir, '.env'), 'utf8'))
    if (m) add(m[1])
  } catch { /* .env 없음 */ }
  for (const p of (env.BAND_DEV_PROXY_PORTS ?? '').split(',')) if (p.trim()) add(p)
  return ports
}

/** 개발 페이지 자신의 출처들: http(s)://{localhost,127.0.0.1,[::1],이 PC의 IPv4}:<개발 서버 포트> + Tauri 출처 */
export function devOrigins(port: number): Set<string> {
  const hosts = new Set(['localhost', '127.0.0.1', '[::1]'])
  for (const list of Object.values(os.networkInterfaces())) {
    for (const ni of list ?? []) if (ni.family === 'IPv4') hosts.add(ni.address)
  }
  const out = new Set(['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost'])
  for (const h of Array.from(hosts)) {
    out.add(`https://${h}:${port}`)
    out.add(`http://${h}:${port}`)
  }
  return out
}

/** 프록시 처리기(미들웨어·업그레이드). 플러그인과 재현 테스트(bandServerProxy.test)가 같이 쓴다 */
export function createBandServerProxyHandlers(getPolicy: () => ProxyPolicy) {
  const deny = (reason: string) => `개발 서버 프록시 거부: ${reason}`
  const middleware: Connect.NextHandleFunction = (req, res, next) => {
    const m = matchDevServerProxyPath(req.url ?? '')
    if (m.kind === 'none') return next()
    const reason = m.kind === 'invalid'
      ? 'localhost·사설 IPv4(10/8, 172.16/12, 192.168/16) 대상만 허용'
      : checkProxyRequest({
        remoteAddress: req.socket?.remoteAddress,
        origin: headerValue(req.headers.origin),
        secFetchSite: headerValue(req.headers['sec-fetch-site']),
        upgrade: null,
      }, m.target, m.path, getPolicy())
    if (reason != null || m.kind !== 'ok') {
      res.statusCode = 403
      res.setHeader('content-type', 'text/plain; charset=utf-8')
      res.end(deny(reason ?? ''))
      return
    }
    proxyHttp(req, res, m.target, m.path)
  }
  const onUpgrade = (req: http.IncomingMessage, socket: Duplex, head: Buffer) => {
    const m = matchDevServerProxyPath(req.url ?? '')
    if (m.kind === 'none') return // Vite HMR 등 다른 업그레이드
    const reason = m.kind === 'invalid'
      ? 'localhost·사설 IPv4 대상만 허용'
      : checkProxyRequest({
        remoteAddress: req.socket.remoteAddress,
        origin: headerValue(req.headers.origin),
        secFetchSite: headerValue(req.headers['sec-fetch-site']),
        upgrade: headerValue(req.headers.upgrade) ?? '',
      }, m.target, m.path, getPolicy())
    if (reason != null || m.kind !== 'ok') {
      const body = Buffer.from(deny(reason ?? ''), 'utf8')
      socket.end(`HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: ${body.length}\r\nConnection: close\r\n\r\n${body.toString('utf8')}`)
      return
    }
    proxyUpgrade(req, socket, head, m.target, m.path)
  }
  return { middleware, onUpgrade }
}

/**
 * @param serverDir 로컬 서버 폴더(인증서 `certs/{cert,key}.pem`이 있으면 기본 스킴을 https로 — server/src/index.ts와 같은 규칙)
 */
export function bandServerProxy(serverDir: string): Plugin {
  return {
    name: 'band-server-proxy',
    apply: 'serve',
    config() {
      if (process.env.VITEST) return
      const tlsServer = fs.existsSync(path.join(serverDir, 'certs', 'cert.pem')) && fs.existsSync(path.join(serverDir, 'certs', 'key.pem'))
      // VITE_ 접두사 없는 상수: .env로는 켤 수 없고 이 플러그인(serve)만 정의한다(L1)
      return { define: { __BAND_DEV_PROXY__: JSON.stringify(tlsServer ? 'https' : 'http') } }
    },
    configureServer(server) {
      if (process.env.VITEST) return
      const allowedPorts = resolveAllowedPorts(serverDir)
      let cached: { port: number; origins: Set<string> } | null = null
      const getPolicy = (): ProxyPolicy => {
        const addr = server.httpServer?.address()
        const port = addr && typeof addr === 'object' ? addr.port : (server.config.server.port ?? 5173)
        if (!cached || cached.port !== port) cached = { port, origins: devOrigins(port) }
        return { allowedOrigins: cached.origins, allowedPorts }
      }
      const { middleware, onUpgrade } = createBandServerProxyHandlers(getPolicy)
      // Vite 내부 미들웨어보다 먼저(반환 함수가 아니라 바로 등록) — 출처·요청자 검사는 checkProxyRequest가 한다
      server.middlewares.use(middleware)
      server.httpServer?.on('upgrade', onUpgrade)
    },
  }
}
