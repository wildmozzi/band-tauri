import { describe, expect, it } from 'vitest'
import {
  checkProxyRequest,
  devServerProxyPath,
  formatDevServerTarget,
  isLocalOrPrivateHost,
  isLoopbackRemoteAddress,
  matchDevServerProxyPath,
  parseDevServerInput,
  parseIpv4,
} from './devServerTarget'

describe('isLocalOrPrivateHost', () => {
  it('localhost·루프백·사설 IPv4만 허용한다', () => {
    for (const h of ['localhost', '127.0.0.1', '127.5.5.5', '10.0.0.1', '172.16.0.1', '172.31.255.255', '192.168.0.10']) {
      expect(isLocalOrPrivateHost(h)).toBe(true)
    }
    for (const h of ['8.8.8.8', '172.15.0.1', '172.32.0.1', '192.169.0.1', 'band-server.fly.dev', 'localhost.evil.com',
      '192.168.0', '192.168.0.256', '0x7f.0.0.1', '', '::1']) {
      expect(isLocalOrPrivateHost(h)).toBe(false)
    }
  })
})

describe('parseDevServerInput', () => {
  it('스킴·포트 생략을 채운다', () => {
    expect(parseDevServerInput(' 192.168.0.10 ', 'http')).toEqual({ scheme: 'http', host: '192.168.0.10', port: 8080 })
    expect(parseDevServerInput('192.168.0.10:9000', 'https')).toEqual({ scheme: 'https', host: '192.168.0.10', port: 9000 })
    expect(parseDevServerInput('HTTPS://LocalHost:8443/', 'http')).toEqual({ scheme: 'https', host: 'localhost', port: 8443 })
  })

  it('공인 주소·도메인·경로·잘못된 포트는 거부한다', () => {
    for (const s of ['', 'example.com', '8.8.8.8', '192.168.0.10/api', '192.168.0.10:0', '192.168.0.10:70000',
      '192.168.0.10:abc', 'ws://192.168.0.10', 'user@192.168.0.10']) {
      expect(parseDevServerInput(s, 'http')).toBeNull()
    }
  })

  it('경로·표시 형식', () => {
    const t = parseDevServerInput('10.0.0.2', 'http')!
    expect(devServerProxyPath(t)).toBe('/__band_server/http/10.0.0.2:8080')
    expect(formatDevServerTarget(t)).toBe('http://10.0.0.2:8080')
  })
})

describe('matchDevServerProxyPath', () => {
  it('프록시가 아닌 경로는 none', () => {
    expect(matchDevServerProxyPath('/')).toEqual({ kind: 'none' })
    expect(matchDevServerProxyPath('/src/main.tsx')).toEqual({ kind: 'none' })
    expect(matchDevServerProxyPath('/__band_serverx/http/localhost:8080')).toEqual({ kind: 'none' })
  })

  it('대상과 나머지 경로·쿼리를 나눈다', () => {
    expect(matchDevServerProxyPath('/__band_server/http/192.168.0.10:8080/api/rooms?x=1')).toEqual({
      kind: 'ok', target: { scheme: 'http', host: '192.168.0.10', port: 8080 }, path: '/api/rooms?x=1',
    })
    expect(matchDevServerProxyPath('/__band_server/https/localhost:8080')).toEqual({
      kind: 'ok', target: { scheme: 'https', host: 'localhost', port: 8080 }, path: '/',
    })
    expect(matchDevServerProxyPath('/__band_server/http/localhost:8080?token=a')).toEqual({
      kind: 'ok', target: { scheme: 'http', host: 'localhost', port: 8080 }, path: '/?token=a',
    })
  })

  it('허용 범위 밖·형식 오류는 invalid', () => {
    for (const u of ['/__band_server', '/__band_server/', '/__band_server/ftp/localhost:21/', '/__band_server/http/8.8.8.8:80/',
      '/__band_server/http/example.com:80/', '/__band_server/http/localhost/', '/__band_server/http/localhost:99999/',
      '/__band_server/http/user@localhost:8080/']) {
      expect(matchDevServerProxyPath(u).kind).toBe('invalid')
    }
  })
})

describe('M2: 엄격한 IPv4 파서', () => {
  it('앞자리 0·16진·정수·축약 표기는 사설로 보지 않는다', () => {
    for (const h of ['010.8.8.8', '172.016.0.1', '127.0.0.01', '192.168.00.1', '00.0.0.0', '0x7f.0.0.1', '2130706433', '127.1', '1e1.0.0.1', '+10.0.0.1']) {
      expect(isLocalOrPrivateHost(h)).toBe(false)
      expect(matchDevServerProxyPath(`/__band_server/http/${h}:8080/`).kind).toBe('invalid')
      expect(parseDevServerInput(h, 'http')).toBeNull()
    }
    expect(parseIpv4('10.0.0.1')).toEqual([10, 0, 0, 1])
    expect(parseIpv4('0.0.0.0')).toEqual([0, 0, 0, 0])
  })
})

describe('checkProxyRequest', () => {
  const policy = { allowedOrigins: new Set(['https://localhost:5173']), allowedPorts: new Set([8080]) }
  const local = { scheme: 'http' as const, host: 'localhost', port: 8080 }
  const lanTarget = { scheme: 'http' as const, host: '192.168.0.10', port: 8080 }
  const ok = { remoteAddress: '127.0.0.1', origin: 'https://localhost:5173', secFetchSite: undefined, upgrade: null }

  it('정상 요청은 허용', () => {
    expect(checkProxyRequest(ok, local, '/api/rooms', policy)).toBeNull()
    expect(checkProxyRequest({ ...ok, upgrade: 'websocket' }, local, '/?token=a', policy)).toBeNull()
    expect(checkProxyRequest({ ...ok, remoteAddress: '::ffff:127.0.0.1', origin: undefined, secFetchSite: 'same-origin' }, local, '/health', policy)).toBeNull()
  })

  it('H1: websocket 외 업그레이드·다른 포트·다른 경로 거부', () => {
    expect(checkProxyRequest({ ...ok, upgrade: 'foo' }, local, '/', policy)).not.toBeNull()
    expect(checkProxyRequest({ ...ok, upgrade: '' }, local, '/', policy)).not.toBeNull()
    expect(checkProxyRequest({ ...ok, upgrade: 'websocket' }, local, '/api/rooms', policy)).not.toBeNull()
    expect(checkProxyRequest(ok, { ...local, port: 6379 }, '/api/x', policy)).not.toBeNull()
    for (const p of ['/', '/admin', '/api', '/api/../admin', '/api/%2E%2E/admin', '/api\\..\\admin', '/uploads/%5c..']) {
      expect(checkProxyRequest(ok, local, p, policy)).not.toBeNull()
    }
  })

  it('H1: LAN 요청자는 루프백 대상 거부, 사설 IP 대상은 허용', () => {
    const lan = { ...ok, remoteAddress: '192.168.0.20', origin: 'https://192.168.0.10:5173' }
    const lanPolicy = { ...policy, allowedOrigins: new Set(['https://192.168.0.10:5173']) }
    expect(checkProxyRequest(lan, local, '/api/rooms', lanPolicy)).not.toBeNull()
    expect(checkProxyRequest(lan, { ...local, host: '127.0.0.1' }, '/api/rooms', lanPolicy)).not.toBeNull()
    expect(checkProxyRequest({ ...lan, remoteAddress: undefined }, local, '/api/rooms', lanPolicy)).not.toBeNull()
    expect(checkProxyRequest(lan, lanTarget, '/api/rooms', lanPolicy)).toBeNull()
  })

  it('M1: 다른 출처·Origin 없는 교차 출처 요청 거부', () => {
    expect(checkProxyRequest({ ...ok, origin: 'https://evil.example' }, local, '/api/rooms', policy)).not.toBeNull()
    expect(checkProxyRequest({ ...ok, origin: 'null' }, local, '/api/rooms', policy)).not.toBeNull()
    expect(checkProxyRequest({ ...ok, origin: undefined, secFetchSite: 'cross-site' }, local, '/api/rooms', policy)).not.toBeNull()
    expect(checkProxyRequest({ ...ok, origin: undefined, secFetchSite: 'same-site' }, local, '/api/rooms', policy)).not.toBeNull()
  })

  it('루프백 원격 주소 판별', () => {
    for (const a of ['127.0.0.1', '127.9.9.9', '::1', '::ffff:127.0.0.1']) expect(isLoopbackRemoteAddress(a)).toBe(true)
    for (const a of [undefined, '', '192.168.0.2', '::ffff:192.168.0.2', 'fe80::1', '::ffff:0127.0.0.1']) expect(isLoopbackRemoteAddress(a)).toBe(false)
  })
})
