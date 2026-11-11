import { afterEach, describe, expect, it, vi } from 'vitest'
import { getApiUrl, getSignalingUrl, isApiUrlConfigured } from './apiUrl'

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
