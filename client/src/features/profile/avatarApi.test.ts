/**
 * P2-12: 프로필 사진 업로드 — 서버 서명 URL 발급 → PUT → 서버 확인. metadata(=JWT)에 사진을 넣지 않는다.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('../../shared/lib/authHeaders', () => ({
  authHeaders: async () => ({ Authorization: 'Bearer tok' }),
}))

import { AVATAR_MAX_BYTES, checkAvatar, uploadAvatar } from './avatarApi'

const webp = (size = 8) => new File([new Uint8Array(size)], 'me.webp', { type: 'image/webp' })
const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })

describe('checkAvatar', () => {
  it('jpg·png·webp, 2MB 이하만', () => {
    expect(checkAvatar(webp())).toBeNull()
    expect(checkAvatar(new File(['GIF89a'], 'a.gif', { type: 'image/gif' }))).toMatch(/jpg, png, webp/)
    expect(checkAvatar(new File(['<svg/>'], 'a.svg', { type: 'image/svg+xml' }))).toMatch(/jpg, png, webp/)
    const big = webp(1)
    Object.defineProperty(big, 'size', { value: AVATAR_MAX_BYTES + 1 })
    expect(checkAvatar(big)).toMatch(/2MB/)
  })
})

describe('uploadAvatar', () => {
  beforeEach(() => vi.stubEnv('VITE_API_URL', 'https://api.example'))
  afterEach(() => vi.unstubAllEnvs())

  it('형식·크기만 보내 서명 URL을 받고, 파일을 PUT, 확인 후 절대 URL을 돌려준다', async () => {
    const calls: { url: string; init?: RequestInit }[] = []
    const fetchImpl = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init })
      const u = String(url)
      if (u.endsWith('/api/storage/avatars/upload-url')) {
        return json(200, { success: true, key: 'u1/1-x.webp', upload: { url: 'https://store/put', method: 'PUT', headers: { 'Content-Type': 'image/webp' } } })
      }
      if (u === 'https://store/put') return new Response(null, { status: 200 })
      if (u.endsWith('/api/storage/avatars/complete')) return json(200, { success: true, url: '/api/storage/avatars/u1/1-x.webp' })
      return new Response(null, { status: 404 })
    }) as unknown as typeof fetch

    const file = webp(16)
    const url = await uploadAvatar(file, fetchImpl)
    expect(url).toBe('https://api.example/api/storage/avatars/u1/1-x.webp')
    expect(calls.map(c => c.url)).toEqual([
      'https://api.example/api/storage/avatars/upload-url',
      'https://store/put',
      'https://api.example/api/storage/avatars/complete',
    ])
    expect(JSON.parse(String(calls[0].init!.body))).toEqual({ contentType: 'image/webp', size: 16 })
    expect((calls[0].init!.headers as Record<string, string>).Authorization).toBe('Bearer tok')
    expect(calls[1].init!.body).toBe(file)
    expect(JSON.parse(String(calls[2].init!.body))).toEqual({ key: 'u1/1-x.webp' })
    // 사진 바이트는 저장소 PUT에만 — 서버 API 본문에는 들어가지 않는다
    for (const c of [calls[0], calls[2]]) expect(String(c.init!.body)).not.toMatch(/data:|base64/)
  })

  it('서버 오류 메시지를 그대로 던진다', async () => {
    const fetchImpl = vi.fn(async () => json(429, { success: false, error: '오늘 프로필 사진을 바꿀 수 있는 횟수를 넘었습니다.' })) as unknown as typeof fetch
    await expect(uploadAvatar(webp(), fetchImpl)).rejects.toThrow(/횟수/)
    await expect(uploadAvatar(webp(), fetchImpl)).rejects.toMatchObject({ kind: 'server', status: 429 })
  })

  it('오프라인·서버 다운(fetch 실패)이면 "Failed to fetch" 대신 연결 안내를 던진다', async () => {
    const offline = vi.fn(async () => { throw new TypeError('Failed to fetch') }) as unknown as typeof fetch
    await expect(uploadAvatar(webp(), offline)).rejects.toThrow('서버에 연결할 수 없습니다')
    await expect(uploadAvatar(webp(), offline)).rejects.toMatchObject({ kind: 'network' })
    const putOffline = vi.fn(async (url: RequestInfo | URL) => {
      if (String(url).endsWith('/upload-url')) return json(200, { success: true, key: 'k', upload: { url: 'https://store/x', method: 'PUT', headers: {} } })
      throw new TypeError('Failed to fetch')
    }) as unknown as typeof fetch
    await expect(uploadAvatar(webp(), putOffline)).rejects.toThrow('서버에 연결할 수 없습니다')
  })

  it('검사에 걸리면 서버를 부르지 않는다', async () => {
    const fetchImpl = vi.fn() as unknown as typeof fetch
    await expect(uploadAvatar(new File(['x'], 'a.gif', { type: 'image/gif' }), fetchImpl)).rejects.toThrow(/jpg, png, webp/)
    expect(fetchImpl).not.toHaveBeenCalled()
  })
})
