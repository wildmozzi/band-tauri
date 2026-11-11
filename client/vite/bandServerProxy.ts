/**
 * 개발 실행 전용 서버 프록시(P1-12b). `vite`(serve)에서만 켜진다 — 운영 빌드(`vite build`)·vitest에는 없다.
 *
 * 앱(https://localhost:5173)이 `/__band_server/<http|https>/<host>:<port>/...`로 보낸 HTTP 요청과 WebSocket 업그레이드를
 * 대상 서버로 그대로 넘긴다. 대상은 localhost·루프백·사설 IPv4만(src/lib/devServerTarget). 그래서
 * - 인증서 없는 로컬 서버(ws://)에 https 화면이 붙고(VITE_SIGNALING_URL=auto가 wss://localhost:8080을 만들던 문제),
 * - 두 번째 PC가 첫 PC의 서버(사설 IP)에 붙는다(mixed content·devCsp 확장, netsh portproxy 없이).
 * 클라이언트 IP 헤더(X-Forwarded-For)는 붙이지 않는다: 서버는 프록시를 띄운 PC의 IP를 클라이언트 IP로 본다.
 */
import fs from 'node:fs'
import http from 'node:http'
import https from 'node:https'
import net from 'node:net'
import path from 'node:path'
import tls from 'node:tls'
import type { Duplex } from 'node:stream'
import type { Plugin } from 'vite'
import { matchDevServerProxyPath, type DevServerTarget } from '../src/lib/devServerTarget'

/** 홉 단위 헤더: 넘기지 않는다(HTTP/2 응답에 넣으면 오류) */
const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-connection', 'transfer-encoding', 'upgrade', 'te', 'trailer',
  'proxy-authenticate', 'proxy-authorization',
])

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
  req.pipe(upstream)
}

function proxyUpgrade(req: http.IncomingMessage, socket: Duplex, head: Buffer, target: DevServerTarget, reqPath: string) {
  const onConnect = () => {
    const lines = [`${req.method ?? 'GET'} ${reqPath} HTTP/1.1`]
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const name = req.rawHeaders[i]
      if (name.startsWith(':') || name.toLowerCase() === 'host') continue
      lines.push(`${name}: ${req.rawHeaders[i + 1]}`)
    }
    lines.push(`Host: ${target.host}:${target.port}`)
    upstream.write(`${lines.join('\r\n')}\r\n\r\n`)
    if (head.length) upstream.write(head)
    upstream.pipe(socket)
    socket.pipe(upstream)
  }
  const upstream: net.Socket = target.scheme === 'https'
    ? tls.connect({ host: target.host, port: target.port, rejectUnauthorized: false, servername: net.isIP(target.host) ? undefined : target.host }, onConnect)
    : net.connect({ host: target.host, port: target.port }, onConnect)
  const close = () => { upstream.destroy(); socket.destroy() }
  upstream.on('error', close)
  socket.on('error', close)
  upstream.on('close', () => socket.destroy())
  socket.on('close', () => upstream.destroy())
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
      return { define: { 'import.meta.env.VITE_BAND_DEV_PROXY': JSON.stringify(tlsServer ? 'https' : 'http') } }
    },
    configureServer(server) {
      if (process.env.VITEST) return
      // Vite 내부 미들웨어보다 먼저(반환 함수가 아니라 바로 등록)
      server.middlewares.use((req, res, next) => {
        const m = matchDevServerProxyPath(req.url ?? '')
        if (m.kind === 'none') return next()
        if (m.kind === 'invalid') {
          res.statusCode = 403
          res.setHeader('content-type', 'text/plain; charset=utf-8')
          res.end('개발 서버 프록시: localhost·사설 IPv4(10/8, 172.16/12, 192.168/16) 대상만 허용')
          return
        }
        proxyHttp(req, res, m.target, m.path)
      })
      server.httpServer?.on('upgrade', (req: http.IncomingMessage, socket: Duplex, head: Buffer) => {
        const m = matchDevServerProxyPath(req.url ?? '')
        if (m.kind === 'none') return // Vite HMR 등 다른 업그레이드
        if (m.kind === 'invalid') {
          socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n')
          return
        }
        proxyUpgrade(req, socket, head, m.target, m.path)
      })
    },
  }
}
