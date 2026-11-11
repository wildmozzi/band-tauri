import { afterEach, describe, expect, it, vi } from 'vitest'
import { getApiUrl } from './apiUrl'

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
