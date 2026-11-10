/**
 * P3-10: 게시글 이미지 업로드 — 서버 서명 URL 발급 → PUT. Supabase Storage 직접 업로드 없음.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'

vi.mock('../lib/authHeaders', () => ({
  authHeaders: async () => ({ Authorization: 'Bearer tok' }),
}))

import { absoluteImageUrl, checkPostImage, uploadPostImage, POST_IMAGE_MAX_BYTES } from './storageApi'

const png = (size = 8) => new File([new Uint8Array(size)], 'a.png', { type: 'image/png' })

function json(status: number, body: unknown) {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

describe('checkPostImage', () => {
  it('accepts allowed images within the size limit', () => {
    expect(checkPostImage(png())).toBeNull()
  })
  it('rejects other types and oversized files', () => {
    expect(checkPostImage(new File(['<svg/>'], 'x.svg', { type: 'image/svg+xml' }))).toMatch(/jpg, png/)
    const big = new File([new Uint8Array(1)], 'big.png', { type: 'image/png' })
    Object.defineProperty(big, 'size', { value: POST_IMAGE_MAX_BYTES + 1 })
    expect(checkPostImage(big)).toMatch(/5MB/)
  })
})

describe('absoluteImageUrl', () => {
  it('keeps absolute URLs and resolves server-relative paths against the API base', () => {
    expect(absoluteImageUrl('https://cdn.example/post-images/u/x.png', 'https://api.example')).toBe('https://cdn.example/post-images/u/x.png')
    expect(absoluteImageUrl('/api/storage/post-images/u/x.png', 'https://api.example')).toBe('https://api.example/api/storage/post-images/u/x.png')
  })
})

describe('uploadPostImage', () => {
  let calls: { url: string; init?: RequestInit }[]
  beforeEach(() => {
    calls = []
    vi.stubEnv('VITE_API_URL', 'https://api.example')
  })
  afterEach(() => { vi.unstubAllEnvs() })

  it('asks the server for a signed URL (type + size only), PUTs the file with the given headers, returns an absolute view URL', async () => {
    const fetchImpl = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init })
      if (String(url).endsWith('/api/storage/post-images/upload-url')) {
        return json(200, {
          success: true,
          key: 'u1/1-abc.png',
          url: '/api/storage/post-images/u1/1-abc.png',
          upload: { url: 'https://acc.r2.cloudflarestorage.com/b/post-images/u1/1-abc.png?X-Amz-Signature=s', method: 'PUT', headers: { 'Content-Type': 'image/png' }, expiresAt: '' },
        })
      }
      return new Response(null, { status: 200 })
    })
    const file = png(8)
    const url = await uploadPostImage(file, fetchImpl as unknown as typeof fetch)

    expect(calls).toHaveLength(2)
    expect(calls[0].url).toBe('https://api.example/api/storage/post-images/upload-url')
    expect(calls[0].init?.method).toBe('POST')
    expect((calls[0].init?.headers as Record<string, string>).Authorization).toBe('Bearer tok')
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({ contentType: 'image/png', size: 8 })
    expect(calls[1].url).toContain('r2.cloudflarestorage.com')
    expect(calls[1].init?.method).toBe('PUT')
    expect(calls[1].init?.headers).toEqual({ 'Content-Type': 'image/png' })
    expect(calls[1].init?.body).toBe(file)
    expect(url).toMatch(/^https?:\/\/.+\/api\/storage\/post-images\/u1\/1-abc\.png$/)
  })

  it('surfaces server refusals and storage PUT failures', async () => {
    const refused = vi.fn(async () => json(413, { success: false, error: '이미지는 5MB 이하만' }))
    await expect(uploadPostImage(png(), refused as unknown as typeof fetch)).rejects.toThrow('5MB')

    const putFails = vi.fn(async (url: RequestInfo | URL) =>
      String(url).endsWith('/upload-url')
        ? json(200, { success: true, key: 'k', url: 'https://cdn/x.png', upload: { url: 'https://store/x', method: 'PUT', headers: {}, expiresAt: '' } })
        : new Response(null, { status: 403 }))
    await expect(uploadPostImage(png(), putFails as unknown as typeof fetch)).rejects.toThrow('403')
  })

  it('does not call the server for invalid files', async () => {
    const f = vi.fn()
    await expect(uploadPostImage(new File(['x'], 'x.txt', { type: 'text/plain' }), f as unknown as typeof fetch)).rejects.toThrow()
    expect(f).not.toHaveBeenCalled()
  })
})
