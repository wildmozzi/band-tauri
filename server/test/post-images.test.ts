/**
 * P3-10: 게시글 이미지 서명 업로드 URL 발급 / 조회 리다이렉트
 * - 인증 필수, MIME·크기 제한, key는 서버가 사용자별 경로로 정함 (클라이언트가 고른 경로·userId 무시)
 * - 조회 경로 순회 차단 (C1과 같은 %2F 디코딩 우회 포함)
 * - 가짜 S3 서버로 발급 URL → 실제 업로드 → 리다이렉트 조회까지
 */
import { after, before, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { createApp } from '../src/app'
import { setTokenVerifier } from '../src/lib/auth'
import { POST_IMAGE_MAX_BYTES, postImageKeyFromParams, postImageViewUrl } from '../src/lib/imageStorage'
import { createS3StorageAdapter } from '../src/storage/s3Adapter'
import type { StorageAdapter } from '../src/storage/types'
import { bearer, testVerifier } from './helpers'
import { memoryStorage } from './fakes/memoryStorage'
import { startFakeS3, type FakeS3 } from './fakes/fakeS3Server'

const UID = '11111111-2222-3333-4444-555555555555'

async function listen(storage: StorageAdapter | null): Promise<{ server: Server; url: string }> {
  const server = createApp({ imageStorage: storage, rateLimits: false }).listen(0)
  await new Promise(r => server.once('listening', r))
  return { server, url: `http://127.0.0.1:${(server.address() as { port: number }).port}` }
}

const post = (url: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(`${url}/api/storage/post-images/upload-url`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body),
  })

describe('POST /api/storage/post-images/upload-url (memory store)', () => {
  const mem = memoryStorage('post-images')
  let s: { server: Server; url: string }
  before(async () => {
    setTokenVerifier(testVerifier)
    s = await listen(mem.adapter)
  })
  after(() => {
    s.server.close()
    setTokenVerifier()
  })

  it('requires authentication', async () => {
    assert.equal((await post(s.url, { contentType: 'image/png', size: 10 })).status, 401)
    assert.equal((await post(s.url, { contentType: 'image/png', size: 10 }, { Authorization: 'Bearer nope' })).status, 401)
  })

  it('rejects non-image MIME types and bad sizes', async () => {
    for (const body of [
      { contentType: 'image/svg+xml', size: 10 }, // 스크립트 실행 가능
      { contentType: 'text/html', size: 10 },
      { contentType: 'image/png' },
      { contentType: 'image/png', size: 0 },
      { contentType: 'image/png', size: -1 },
      { contentType: 'image/png', size: 1.5 },
      { contentType: 'image/png', size: '10' },
    ]) {
      const r = await post(s.url, body, bearer(UID))
      assert.equal(r.status, 400, JSON.stringify(body))
    }
    const big = await post(s.url, { contentType: 'image/jpeg', size: POST_IMAGE_MAX_BYTES + 1 }, bearer(UID))
    assert.equal(big.status, 413)
    assert.equal(mem.calls.filter(c => c.op === 'signPut').length, 0)
  })

  it('issues a signed PUT under the caller\'s own folder, ignoring client-supplied paths/ids', async () => {
    const r = await post(s.url, { contentType: 'image/webp', size: POST_IMAGE_MAX_BYTES, key: '../admin/x.webp', userId: 'someone-else' }, bearer(UID))
    assert.equal(r.status, 200)
    const j = await r.json() as { key: string; upload: { url: string; method: string; headers: Record<string, string> }; url: string }
    assert.match(j.key, new RegExp(`^${UID}/\\d+-[0-9a-f-]{36}\\.webp$`))
    assert.equal(j.upload.method, 'PUT')
    const [key, opts] = mem.calls.find(c => c.op === 'signPut')!.arg as [string, { contentType: string; contentLength: number }]
    // 미검증 업로드는 pending/ 에만 쓸 수 있고, 조회 URL은 /complete 검증 뒤에만 준다 (리뷰2 M3)
    assert.equal(key, `pending/${j.key}`)
    assert.equal(opts.contentType, 'image/webp')
    assert.equal(opts.contentLength, POST_IMAGE_MAX_BYTES)
    assert.equal(j.url, undefined)
  })

  it('refuses a user id that cannot be a path segment', async () => {
    const r = await post(s.url, { contentType: 'image/png', size: 1 }, bearer('../evil'))
    assert.equal(r.status, 400)
  })

  it('GET redirects to a short-lived signed URL', async () => {
    const r = await fetch(`${s.url}/api/storage/post-images/${UID}/1700000000000-abc.png`, { redirect: 'manual' })
    assert.equal(r.status, 302)
    assert.equal(r.headers.get('location'), `https://signed.test/post-images/${UID}/1700000000000-abc.png?ttl=3600`)
    assert.match(r.headers.get('cache-control') ?? '', /max-age=600/)
  })

  it('GET rejects traversal (C1 regression: encoded %2F / dot segments)', async () => {
    const before_ = mem.calls.filter(c => c.op === 'signGet').length
    for (const p of [
      `${UID}/..%2F..%2Faudio-stems%2Fu%2Fs%2Fvocals.ogg`,
      `..%2F${UID}/x.png`,
      `${UID}/.hidden`,
      `${UID}/..`,
      `${UID}/a%5Cb.png`,
      `${UID}/a..png`,
    ]) {
      const r = await fetch(`${s.url}/api/storage/post-images/${p}`, { redirect: 'manual' })
      assert.ok(r.status === 400 || r.status === 404, `${p} → ${r.status}`)
    }
    assert.equal(mem.calls.filter(c => c.op === 'signGet').length, before_, 'nothing outside the image key pattern is signed')
  })
})

describe('postImageKeyFromParams / postImageViewUrl', () => {
  it('accepts new and legacy file names only', () => {
    assert.equal(postImageKeyFromParams(UID, '1700000000000-k3j2h1.JPG'), `${UID}/1700000000000-k3j2h1.JPG`)
    for (const [o, f] of [[UID, '../x'], ['a/b', 'x.png'], [UID, ''], [UID, 'x/y.png'], [1, 'x.png'], [UID, '-x.png']] as const) {
      assert.equal(postImageKeyFromParams(o, f), null, `${String(o)} ${String(f)}`)
    }
  })
  it('prefers the store\'s public URL, else the API redirect (PUBLIC_API_URL prefix)', () => {
    const pub = memoryStorage('post-images', { publicBase: 'https://cdn.example' }).adapter
    assert.equal(postImageViewUrl(pub, 'u/x.png'), 'https://cdn.example/post-images/u/x.png')
    const priv = memoryStorage('post-images').adapter
    assert.equal(postImageViewUrl(priv, 'u/x.png', 'https://api.example/'), 'https://api.example/api/storage/post-images/u/x.png')
  })
})

describe('storage not configured', () => {
  it('returns 503', async () => {
    setTokenVerifier(testVerifier)
    const s = await listen(null)
    try {
      assert.equal((await post(s.url, { contentType: 'image/png', size: 1 }, bearer(UID))).status, 503)
      assert.equal((await fetch(`${s.url}/api/storage/post-images/${UID}/x.png`, { redirect: 'manual' })).status, 503)
    } finally {
      s.server.close()
      setTokenVerifier()
    }
  })
})

describe('end to end with a fake S3 store', () => {
  let s3: FakeS3
  let s: { server: Server; url: string }
  before(async () => {
    setTokenVerifier(testVerifier)
    s3 = await startFakeS3()
    s = await listen(createS3StorageAdapter({
      endpoint: s3.endpoint, region: s3.region, bucket: s3.bucket,
      accessKeyId: s3.accessKeyId, secretAccessKey: s3.secretAccessKey, forcePathStyle: true,
    }, 'post-images'))
  })
  after(async () => {
    s.server.close()
    await s3.close()
    setTokenVerifier()
  })

  it('upload to pending → /complete (Range read, move) → view through the redirect; a bigger body is refused', async () => {
    const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(4096, 7)])
    const r = await post(s.url, { contentType: 'image/png', size: png.length }, bearer(UID))
    const j = await r.json() as { key: string; upload: { url: string; headers: Record<string, string> } }

    const tooBig = await fetch(j.upload.url, { method: 'PUT', headers: j.upload.headers, body: Buffer.concat([png, png]) })
    assert.equal(tooBig.status, 403)

    const put = await fetch(j.upload.url, { method: 'PUT', headers: j.upload.headers, body: png })
    assert.equal(put.status, 200)
    assert.ok(s3.objects.has(`post-images/pending/${j.key}`))
    // 검증 전에는 조회 라우트로 볼 수 없다
    assert.equal((await fetch(`${s.url}/api/storage/post-images/${j.key}`)).status, 404)

    const before = s3.bytesServed()
    const done = await fetch(`${s.url}/api/storage/post-images/complete`, {
      method: 'POST', headers: { 'Content-Type': 'application/json', ...bearer(UID) }, body: JSON.stringify({ key: j.key }),
    })
    assert.equal(done.status, 200)
    const after = s3.bytesServed()
    assert.equal(after.full - before.full, 0, '/complete must not download the whole object (M4)')
    assert.ok(after.range - before.range <= 16)
    assert.ok(s3.objects.has(`post-images/${j.key}`))
    assert.ok(!s3.objects.has(`post-images/pending/${j.key}`))

    const { url } = await done.json() as { url: string }
    const view = await fetch(`${s.url}${url}`)
    assert.equal(view.status, 200)
    assert.deepEqual(Buffer.from(await view.arrayBuffer()), png)
  })
})
