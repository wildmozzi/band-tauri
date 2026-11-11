import { describe, expect, it } from 'vitest'
import {
  devServerProxyPath,
  formatDevServerTarget,
  isLocalOrPrivateHost,
  matchDevServerProxyPath,
  parseDevServerInput,
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
