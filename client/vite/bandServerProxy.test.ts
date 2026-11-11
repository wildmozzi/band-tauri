// @vitest-environment node
/**
 * 개발 서버 프록시 보안 재현 테스트(PR #56 리뷰 H1·M1·M2). 실제 소켓으로 프록시 처리기를 띄우고,
 * 루프백 전용 가짜 서비스(원시 TCP 에코)에 터널이 열리지 않는지 확인한다.
 */
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { createBandServerProxyHandlers } from './bandServerProxy'

const ORIGIN = 'https://localhost:5173'

let proxy: http.Server
let proxyPort = 0
/** 루프백 전용 원시 TCP 에코(Redis 같은 줄 단위 서비스 흉내). 받은 바이트를 모두 기록 */
let echo: net.Server
let echoPort = 0
let echoReceived = ''
/** 밴드 서버 흉내: /api/·/health HTTP, / 에서 WebSocket 101 후 에코 */
let band: http.Server
let bandPort = 0

function listen(server: net.Server, host: string): Promise<number> {
  return new Promise((resolve) => server.listen(0, host, () => resolve((server.address() as net.AddressInfo).port)))
}

/** 원시 요청을 보내고 응답을 모은다. afterResponse가 있으면 첫 응답 조각 뒤에 그 바이트를 더 보낸다 */
function raw(text: string, opts: { host?: string; afterResponse?: string; waitMs?: number } = {}): Promise<string> {
  return new Promise((resolve) => {
    let out = ''
    let sentMore = false
    const s = net.connect({ host: opts.host ?? '127.0.0.1', port: proxyPort }, () => s.write(text))
    s.on('data', (d) => {
      out += d.toString('utf8')
      if (opts.afterResponse && !sentMore) {
        sentMore = true
        s.write(opts.afterResponse)
      }
    })
    const done = () => resolve(out)
    s.on('close', done)
    s.on('error', done)
    setTimeout(() => { s.destroy(); done() }, opts.waitMs ?? 400)
  })
}

function upgradeReq(path: string, headers: Record<string, string>): string {
  const lines = [`GET ${path} HTTP/1.1`, `Host: localhost:${proxyPort}`, 'Connection: Upgrade']
  for (const [k, v] of Object.entries(headers)) lines.push(`${k}: ${v}`)
  return `${lines.join('\r\n')}\r\n\r\n`
}

function wsHeaders(extra: Record<string, string> = {}): Record<string, string> {
  return {
    Upgrade: 'websocket', Origin: ORIGIN, 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==', ...extra,
  }
}

function lanIpv4(): string | undefined {
  for (const list of Object.values(os.networkInterfaces())) {
    for (const ni of list ?? []) if (ni.family === 'IPv4' && !ni.internal) return ni.address
  }
  return undefined
}

beforeAll(async () => {
  echo = net.createServer((s) => {
    s.on('data', (d) => {
      echoReceived += d.toString('utf8')
      s.write(`ECHO:${d.toString('utf8')}`)
    })
    s.on('error', () => {})
  })
  echoPort = await listen(echo, '127.0.0.1')

  band = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' })
    res.end(`band ${req.url}`)
  })
  band.on('upgrade', (_req, socket: net.Socket) => {
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n')
    socket.on('data', (d) => socket.write(`WS:${d.toString('utf8')}`))
    socket.on('error', () => {})
  })
  bandPort = await listen(band, '0.0.0.0')

  // 에코 포트도 허용 포트에 넣어 "포트 고정"이 아닌 다른 방어(Upgrade 종류·101 확인·경로)를 따로 검증한다
  const policy = { allowedOrigins: new Set([ORIGIN]), allowedPorts: new Set([bandPort, echoPort]) }
  const { middleware, onUpgrade } = createBandServerProxyHandlers(() => policy)
  proxy = http.createServer((req, res) => middleware(req, res, () => { res.statusCode = 404; res.end('next') }))
  proxy.on('upgrade', onUpgrade)
  proxyPort = await listen(proxy, '0.0.0.0')
})

afterAll(() => {
  proxy?.close()
  echo?.close()
  band?.close()
})

describe('H1: 원시 TCP 터널 차단', () => {
  it('Upgrade: websocket이 아니면 403, 루프백 서비스에 아무것도 닿지 않는다(리뷰 탐침 재현)', async () => {
    echoReceived = ''
    const out = await raw(upgradeReq(`/__band_server/http/127.0.0.1:${echoPort}/`, { Upgrade: 'foo', Origin: ORIGIN }), {
      afterResponse: 'RAW-BYTES-AFTER-UPGRADE\r\n',
    })
    expect(out).toMatch(/^HTTP\/1\.1 403/)
    expect(out).not.toContain('ECHO:')
    expect(echoReceived).toBe('')
  })

  it('websocket이라도 업스트림이 101로 답하지 않으면 양방향 파이프를 열지 않는다', async () => {
    echoReceived = ''
    const out = await raw(upgradeReq('/__band_server/http/127.0.0.1:' + echoPort + '/', wsHeaders()), {
      afterResponse: 'RAW-BYTES-AFTER-UPGRADE\r\n',
    })
    expect(echoReceived).not.toContain('RAW-BYTES-AFTER-UPGRADE')
    expect(out).not.toContain('ECHO:RAW-BYTES')
  })

  it('허용 포트가 아니면 403', async () => {
    const out = await raw(upgradeReq('/__band_server/http/127.0.0.1:6379/', wsHeaders()))
    expect(out).toMatch(/^HTTP\/1\.1 403/)
  })

  it('시그널링 경로(/)가 아니면 업그레이드 403, HTTP는 밴드 서버 경로만', async () => {
    expect(await raw(upgradeReq(`/__band_server/http/127.0.0.1:${bandPort}/admin`, wsHeaders()))).toMatch(/^HTTP\/1\.1 403/)
    const r = await fetch(`http://127.0.0.1:${proxyPort}/__band_server/http/127.0.0.1:${bandPort}/admin`, { headers: { origin: ORIGIN } })
    expect(r.status).toBe(403)
    const r2 = await fetch(`http://127.0.0.1:${proxyPort}/__band_server/http/127.0.0.1:${bandPort}/api/%2e%2e/admin`, { headers: { origin: ORIGIN } })
    expect(r2.status).toBe(403)
  })

  it('정상 경로: 시그널링 WebSocket(101)과 /api 요청은 통과한다', async () => {
    const out = await raw(upgradeReq(`/__band_server/http/127.0.0.1:${bandPort}/?token=a`, wsHeaders()), { afterResponse: 'hello' })
    expect(out).toMatch(/^HTTP\/1\.1 101/)
    expect(out).toContain('WS:hello')
    const r = await fetch(`http://127.0.0.1:${proxyPort}/__band_server/http/127.0.0.1:${bandPort}/api/rooms?x=1`, { headers: { origin: ORIGIN } })
    expect(r.status).toBe(200)
    expect(await r.text()).toBe('band /api/rooms?x=1')
  })

  const lan = lanIpv4()
  it.skipIf(!lan)('LAN 기기(루프백이 아닌 요청자)는 루프백 대상으로 프록시할 수 없고, 사설 IP 대상은 된다', async () => {
    const denied = await raw(upgradeReq(`/__band_server/http/127.0.0.1:${bandPort}/`, wsHeaders()), { host: lan })
    expect(denied).toMatch(/^HTTP\/1\.1 403/)
    const r = await fetch(`http://${lan}:${proxyPort}/__band_server/http/localhost:${bandPort}/health`, { headers: { origin: ORIGIN } })
    expect(r.status).toBe(403)
    const ok = await fetch(`http://${lan}:${proxyPort}/__band_server/http/${lan}:${bandPort}/health`, { headers: { origin: ORIGIN } })
    if (/^(10|172\.(1[6-9]|2\d|3[01])|192\.168)\./.test(lan!)) expect(ok.status).toBe(200)
  })

  it('연결 실패면 502 한 줄을 돌려준다(L3)', async () => {
    // 닫힌 포트를 만들기 위해 열었다 바로 닫는다
    const s = net.createServer()
    const port = await listen(s, '127.0.0.1')
    await new Promise<void>((r) => s.close(() => r()))
    const policy = { allowedOrigins: new Set([ORIGIN]), allowedPorts: new Set([port]) }
    const { onUpgrade } = createBandServerProxyHandlers(() => policy)
    const p = http.createServer()
    p.on('upgrade', onUpgrade)
    const pPort = await listen(p, '127.0.0.1')
    const out = await new Promise<string>((resolve) => {
      let acc = ''
      const c = net.connect({ host: '127.0.0.1', port: pPort }, () => c.write(upgradeReq(`/__band_server/http/127.0.0.1:${port}/`, wsHeaders())))
      c.on('data', (d) => { acc += d.toString('utf8') })
      c.on('close', () => resolve(acc))
      c.on('error', () => resolve(acc))
    })
    p.close()
    expect(out).toMatch(/^HTTP\/1\.1 502/)
  })
})

describe('M1: 출처 검사', () => {
  it('다른 출처의 WebSocket·HTTP는 403', async () => {
    const out = await raw(upgradeReq(`/__band_server/http/127.0.0.1:${bandPort}/`, wsHeaders({ Origin: 'https://evil.example' })))
    expect(out).toMatch(/^HTTP\/1\.1 403/)
    const r = await fetch(`http://127.0.0.1:${proxyPort}/__band_server/http/127.0.0.1:${bandPort}/api/rooms`, {
      method: 'POST', headers: { origin: 'https://evil.example' },
    })
    expect(r.status).toBe(403)
  })

  it('Origin 없는 교차 출처 요청(no-cors GET)은 Sec-Fetch-Site로 거부, 같은 출처는 허용', async () => {
    const base = `http://127.0.0.1:${proxyPort}/__band_server/http/127.0.0.1:${bandPort}/health`
    expect((await fetch(base, { headers: { 'sec-fetch-site': 'cross-site' } })).status).toBe(403)
    expect((await fetch(base, { headers: { 'sec-fetch-site': 'same-origin' } })).status).toBe(200)
  })
})

describe('M2: 비정규 IPv4 표기', () => {
  it('앞자리 0 표기는 사설로 보지 않고 403', async () => {
    for (const host of ['010.8.8.8', '172.016.0.1', '127.0.0.01']) {
      const r = await fetch(`http://127.0.0.1:${proxyPort}/__band_server/http/${host}:${bandPort}/health`, { headers: { origin: ORIGIN } })
      expect(r.status).toBe(403)
    }
  })
})
