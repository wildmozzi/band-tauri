import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  DEV_SERVER_STORAGE_KEY,
  getApiUrl,
  getDevServerSetting,
  getDevServerTarget,
  getSignalingUrl,
  isApiUrlConfigured,
  isDevServerProxyEnabled,
  setDevServerSetting,
  usesAutoServerUrl,
} from './apiUrl'

describe('getApiUrl', () => {
  afterEach(() => { vi.unstubAllEnvs(); vi.unstubAllGlobals() })

  it('uses VITE_API_URL without trailing slashes', () => {
    vi.stubEnv('VITE_API_URL', 'https://api.example//')
    expect(getApiUrl()).toBe('https://api.example')
  })

  it('falls back to the page host on port 8080 when unset or auto', () => {
    vi.stubGlobal('window', { location: { protocol: 'https:', hostname: 'band.local' } })
    const expected = 'https://band.local:8080'
    vi.stubEnv('VITE_API_URL', 'auto')
    expect(getApiUrl()).toBe(expected)
    vi.stubEnv('VITE_API_URL', '')
    expect(getApiUrl()).toBe(expected)
  })
})

describe('getSignalingUrl', () => {
  afterEach(() => { vi.unstubAllEnvs(); vi.unstubAllGlobals() })

  it('uses VITE_SIGNALING_URL as is', () => {
    vi.stubEnv('VITE_SIGNALING_URL', 'wss://sig.example')
    expect(getSignalingUrl()).toBe('wss://sig.example')
  })

  it('falls back to ws(s) on the page host port 8080 when unset or auto', () => {
    vi.stubGlobal('window', { location: { protocol: 'https:', hostname: 'band.local' } })
    vi.stubEnv('VITE_SIGNALING_URL', 'auto')
    expect(getSignalingUrl()).toBe('wss://band.local:8080')
    vi.stubGlobal('window', { location: { protocol: 'http:', hostname: 'localhost' } })
    vi.stubEnv('VITE_SIGNALING_URL', '')
    expect(getSignalingUrl()).toBe('ws://localhost:8080')
  })
})

describe('isApiUrlConfigured', () => {
  afterEach(() => { vi.unstubAllEnvs() })

  it('is true for any non-empty VITE_API_URL including auto', () => {
    vi.stubEnv('VITE_API_URL', '')
    expect(isApiUrlConfigured()).toBe(false)
    vi.stubEnv('VITE_API_URL', 'auto')
    expect(isApiUrlConfigured()).toBe(true)
    vi.stubEnv('VITE_API_URL', 'https://api.example')
    expect(isApiUrlConfigured()).toBe(true)
  })
})

describe('개발 서버 프록시(P1-12b)', () => {
  const storage = new Map<string, string>()
  const stubWindow = () => vi.stubGlobal('window', {
    location: { protocol: 'https:', hostname: 'localhost', origin: 'https://localhost:5173' },
    localStorage: {
      getItem: (k: string) => storage.get(k) ?? null,
      setItem: (k: string, v: string) => { storage.set(k, v) },
      removeItem: (k: string) => { storage.delete(k) },
    },
  })
  afterEach(() => { vi.unstubAllEnvs(); vi.unstubAllGlobals(); storage.clear() })

  it('플러그인이 없으면(운영 빌드) 페이지 프로토콜 규칙 그대로', () => {
    stubWindow()
    vi.stubEnv('VITE_BAND_DEV_PROXY', '')
    vi.stubEnv('VITE_SIGNALING_URL', 'auto')
    expect(isDevServerProxyEnabled()).toBe(false)
    expect(getDevServerTarget()).toBeNull()
    expect(getSignalingUrl()).toBe('wss://localhost:8080')
  })

  it('개발 실행 auto는 같은 출처 프록시로 — 평문 로컬 서버도 wss로 붙는다', () => {
    stubWindow()
    vi.stubEnv('VITE_BAND_DEV_PROXY', 'http')
    vi.stubEnv('VITE_SIGNALING_URL', 'auto')
    vi.stubEnv('VITE_API_URL', '')
    expect(getSignalingUrl()).toBe('wss://localhost:5173/__band_server/http/localhost:8080')
    expect(getApiUrl()).toBe('https://localhost:5173/__band_server/http/localhost:8080')
  })

  it('로컬 서버에 인증서가 있으면 기본 대상이 https', () => {
    stubWindow()
    vi.stubEnv('VITE_BAND_DEV_PROXY', 'https')
    expect(getApiUrl()).toBe('https://localhost:5173/__band_server/https/localhost:8080')
  })

  it('설정한 사설 IP 서버로, 틀린 저장값은 기본값으로', () => {
    stubWindow()
    vi.stubEnv('VITE_BAND_DEV_PROXY', 'http')
    setDevServerSetting(' 192.168.0.10 ')
    expect(getDevServerSetting()).toBe('192.168.0.10')
    expect(getSignalingUrl()).toBe('wss://localhost:5173/__band_server/http/192.168.0.10:8080')
    storage.set(DEV_SERVER_STORAGE_KEY, 'band-server.fly.dev')
    expect(getDevServerTarget()).toEqual({ scheme: 'http', host: 'localhost', port: 8080 })
    setDevServerSetting(null)
    expect(getDevServerSetting()).toBeNull()
  })

  it('직접 지정한 URL은 프록시를 쓰지 않는다', () => {
    stubWindow()
    vi.stubEnv('VITE_BAND_DEV_PROXY', 'http')
    vi.stubEnv('VITE_SIGNALING_URL', 'ws://localhost:8080')
    vi.stubEnv('VITE_API_URL', 'http://localhost:8080')
    expect(getSignalingUrl()).toBe('ws://localhost:8080')
    expect(getApiUrl()).toBe('http://localhost:8080')
    expect(usesAutoServerUrl()).toBe(false)
    vi.stubEnv('VITE_API_URL', 'auto')
    expect(usesAutoServerUrl()).toBe(true)
  })
})
