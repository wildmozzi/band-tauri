/**
 * P3-10: StorageAdapter 계약 테스트
 * - 같은 계약을 S3Adapter(가짜 S3 서버, 실제 HTTP + SigV4 서명 검증)와 SupabaseStorageAdapter(가짜 storage 클라이언트)에 돌린다
 * - S3 서명 URL: 권한(크기·MIME 서명), 만료, 논리 버킷 격리, 페이지 목록
 * - env 설정 파싱
 */
import { after, before, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { createS3StorageAdapter, stableSigningDate, type S3Config } from '../src/storage/s3Adapter'
import { createSupabaseStorageAdapter } from '../src/storage/supabaseAdapter'
import { readStorageConfig } from '../src/storage/config'
import { assertSafeKey, StorageError, type StorageAdapter } from '../src/storage/types'
import { startFakeS3, type FakeS3 } from './fakes/fakeS3Server'
import { fakeSupabaseStorage } from './fakes/fakeSupabaseStorage'

function contract(name: string, make: () => StorageAdapter) {
  describe(`StorageAdapter contract: ${name}`, () => {
    it('put → get/head → list → delete', async () => {
      const a = make()
      await a.put('u1/s1/vocals.ogg', Buffer.from('abc'), { contentType: 'audio/ogg', cacheMaxAgeSec: 60 })
      await a.put('u1/s1/drums.ogg', Buffer.from('12345'), { contentType: 'audio/ogg' })
      await a.put('u2/s9/bass.wav', Buffer.from('x'))

      assert.equal((await a.get('u1/s1/vocals.ogg'))?.toString(), 'abc')
      assert.equal((await a.head('u1/s1/drums.ogg'))?.size, 5)
      assert.equal((await a.head('u1/s1/drums.ogg'))?.contentType, 'audio/ogg')
      assert.equal(await a.get('u1/s1/missing.ogg'), null)
      assert.equal(await a.head('u1/s1/missing.ogg'), null)

      assert.deepEqual((await a.list('u1')).sort((x, y) => x.key.localeCompare(y.key)), [
        { key: 'u1/s1/drums.ogg', size: 5 },
        { key: 'u1/s1/vocals.ogg', size: 3 },
      ])
      assert.equal((await a.list('')).length, 3)

      // 없는 key가 섞여 있어도 성공
      await a.delete(['u1/s1/vocals.ogg', 'u1/s1/vocals.wav', 'u1/s1/nope.ogg'])
      assert.equal(await a.head('u1/s1/vocals.ogg'), null)
      assert.equal((await a.head('u1/s1/drums.ogg'))?.size, 5)
      await a.delete([])
    })

    it('put overwrites', async () => {
      const a = make()
      await a.put('k/over.png', Buffer.from('one'))
      await a.put('k/over.png', Buffer.from('second'))
      assert.equal((await a.get('k/over.png'))?.toString(), 'second')
    })

    it('rejects unsafe keys everywhere (no traversal out of the logical bucket)', async () => {
      const a = make()
      for (const bad of ['../x', 'a/../../b', '/abs', 'a//b', 'a\\b', '', 'a/./b', 'a\u0000b']) {
        await assert.rejects(a.put(bad, Buffer.from('x')), StorageError, bad)
        await assert.rejects(a.get(bad), StorageError)
        await assert.rejects(a.signedGetUrl(bad, 60), StorageError)
        await assert.rejects(a.signedPutUrl(bad, { contentType: 'image/png', contentLength: 1, expiresInSec: 60 }), StorageError)
        await assert.rejects(a.delete(['ok/key', bad]), StorageError)
      }
      await assert.rejects(a.list('../'), StorageError)
    })

    it('signed URLs name the object', async () => {
      const a = make()
      const get = await a.signedGetUrl('u1/s1/vocals.ogg', 600)
      assert.match(get, /u1\/s1\/vocals\.ogg\?/)
      const put = await a.signedPutUrl('u1/p.png', { contentType: 'image/png', contentLength: 10, expiresInSec: 600 })
      assert.equal(put.method, 'PUT')
      assert.equal(put.headers['Content-Type'], 'image/png')
      assert.match(put.url, /u1\/p\.png\?/)
      assert.ok(Date.parse(put.expiresAt) > Date.now())
    })
  })
}

describe('assertSafeKey', () => {
  it('accepts normal keys', () => {
    assertSafeKey('u/s/vocals.ogg')
    assertSafeKey('a.b-c_d')
  })
})

// ---------- S3 (가짜 S3 서버) ----------

let s3: FakeS3
let clock = () => new Date()
const cfgOf = (f: FakeS3, extra: Partial<S3Config> = {}): S3Config => ({
  endpoint: f.endpoint,
  region: f.region,
  bucket: f.bucket,
  accessKeyId: f.accessKeyId,
  secretAccessKey: f.secretAccessKey,
  forcePathStyle: true,
  ...extra,
})

before(async () => {
  s3 = await startFakeS3({ pageSize: 2, now: () => clock() })
})
after(async () => {
  await s3.close()
})

contract('S3Adapter (fake S3 server)', () => {
  s3.objects.clear()
  return createS3StorageAdapter(cfgOf(s3), 'audio-stems')
})

contract('SupabaseStorageAdapter (fake client)', () => createSupabaseStorageAdapter(fakeSupabaseStorage().client, 'audio-stems'))

describe('S3Adapter specifics', () => {
  it('stores objects under the logical bucket prefix with content-type and cache-control', async () => {
    s3.objects.clear()
    const stems = createS3StorageAdapter(cfgOf(s3), 'audio-stems')
    const images = createS3StorageAdapter(cfgOf(s3), 'post-images')
    await stems.put('u/s/vocals.ogg', Buffer.from('a'), { contentType: 'audio/ogg', cacheMaxAgeSec: 31536000 })
    await images.put('u/x.png', Buffer.from('b'), { contentType: 'image/png' })
    assert.deepEqual([...s3.objects.keys()].sort(), ['audio-stems/u/s/vocals.ogg', 'post-images/u/x.png'])
    assert.equal(s3.objects.get('audio-stems/u/s/vocals.ogg')!.contentType, 'audio/ogg')
    assert.equal(s3.objects.get('audio-stems/u/s/vocals.ogg')!.cacheControl, 'public, max-age=31536000')
    // 논리 버킷끼리 섞이지 않는다
    assert.deepEqual((await stems.list('')).map(o => o.key), ['u/s/vocals.ogg'])
    assert.equal(await stems.get('u/x.png'), null)
  })

  it('lists across pages (continuation token)', async () => {
    s3.objects.clear()
    const a = createS3StorageAdapter(cfgOf(s3), 'audio-stems')
    for (let i = 0; i < 5; i++) await a.put(`u/s/f${i}.ogg`, Buffer.from('x'.repeat(i + 1)))
    const listed = await a.list('u')
    assert.deepEqual(listed.map(o => o.size), [1, 2, 3, 4, 5])
  })

  it('signed GET URL downloads, and is refused after expiry', async () => {
    s3.objects.clear()
    const a = createS3StorageAdapter(cfgOf(s3), 'audio-stems')
    await a.put('u/s/vocals.ogg', Buffer.from('opus-bytes'), { contentType: 'audio/ogg' })
    const url = await a.signedGetUrl('u/s/vocals.ogg', 600)
    const ok = await fetch(url)
    assert.equal(ok.status, 200)
    assert.equal(await ok.text(), 'opus-bytes')

    // 같은 URL을 만료 뒤에 쓰면 403
    const real = clock
    clock = () => new Date(Date.now() + 2 * 3600 * 1000)
    try {
      assert.equal((await fetch(url)).status, 403)
    } finally {
      clock = real
    }

    // 서명이 위조되면 403
    assert.equal((await fetch(url.replace(/X-Amz-Signature=[0-9a-f]{4}/, 'X-Amz-Signature=0000'))).status, 403)
    // 다른 객체로 경로를 바꾸면 403 (서명이 경로를 묶는다)
    assert.equal((await fetch(url.replace('vocals.ogg', 'drums.ogg'))).status, 403)
  })

  it('signed PUT URL accepts exactly the signed size and type', async () => {
    s3.objects.clear()
    const a = createS3StorageAdapter(cfgOf(s3), 'post-images')
    const body = Buffer.from('0123456789')
    const put = await a.signedPutUrl('u1/pic.png', { contentType: 'image/png', contentLength: body.length, expiresInSec: 600 })

    const wrongSize = await fetch(put.url, { method: 'PUT', headers: put.headers, body: Buffer.concat([body, body]) })
    assert.equal(wrongSize.status, 403, 'larger body than signed must be refused')
    const wrongType = await fetch(put.url, { method: 'PUT', headers: { 'Content-Type': 'text/html' }, body })
    assert.equal(wrongType.status, 403, 'other content-type must be refused')
    assert.equal(s3.objects.size, 0)

    const ok = await fetch(put.url, { method: 'PUT', headers: put.headers, body })
    assert.equal(ok.status, 200)
    assert.equal(s3.objects.get('post-images/u1/pic.png')!.body.toString(), '0123456789')
    assert.equal(s3.objects.get('post-images/u1/pic.png')!.contentType, 'image/png')

    // 서명 PUT은 다른 key에 쓸 수 없다
    const other = await fetch(put.url.replace('u1/pic.png', 'u2/pic.png'), { method: 'PUT', headers: put.headers, body })
    assert.equal(other.status, 403)
  })

  it('signed PUT URL expires', async () => {
    const a = createS3StorageAdapter(cfgOf(s3), 'post-images', { now: () => new Date(Date.now() - 3600 * 1000) })
    const body = Buffer.from('x')
    const put = await a.signedPutUrl('u1/late.png', { contentType: 'image/png', contentLength: 1, expiresInSec: 600 })
    assert.equal((await fetch(put.url, { method: 'PUT', headers: put.headers, body })).status, 403)
  })

  it('unsigned / wrong-key requests are refused by the store', async () => {
    const a = createS3StorageAdapter(cfgOf(s3, { secretAccessKey: 'wrong' }), 'audio-stems')
    const url = await a.signedGetUrl('u/s/vocals.ogg', 600)
    assert.equal((await fetch(url)).status, 403)
    assert.equal((await fetch(`${s3.endpoint}/${s3.bucket}/audio-stems/u/s/vocals.ogg`)).status, 403)
  })

  it('signed GET URL is stable inside a window (browser cache) and clamps to 7 days', async () => {
    const t = new Date('2026-01-01T10:20:30Z')
    const a = createS3StorageAdapter(cfgOf(s3), 'audio-stems', { now: () => t })
    const b = createS3StorageAdapter(cfgOf(s3), 'audio-stems', { now: () => new Date(t.getTime() + 60_000) })
    assert.equal(await a.signedGetUrl('u/s/v.ogg', 12 * 3600), await b.signedGetUrl('u/s/v.ogg', 12 * 3600))
    assert.equal(stableSigningDate(t, 12 * 3600).toISOString(), '2026-01-01T10:00:00.000Z')
    assert.equal(stableSigningDate(t, 120).getTime(), t.getTime(), 'short TTL is not windowed')
    assert.match(await a.signedGetUrl('u/s/v.ogg', 30 * 24 * 3600), /X-Amz-Expires=604800/)
  })

  it('publicUrl only with S3_PUBLIC_BASE_URL', () => {
    assert.equal(createS3StorageAdapter(cfgOf(s3), 'post-images').publicUrl('u/a b.png'), null)
    const pub = createS3StorageAdapter(cfgOf(s3, { publicBaseUrl: 'https://cdn.example/' }), 'post-images')
    assert.equal(pub.publicUrl('u/a b.png'), 'https://cdn.example/post-images/u/a%20b.png')
  })
})

describe('SupabaseStorageAdapter specifics', () => {
  it('uses the bucket of the same name, keeps cacheControl seconds, public URL, recursive list with paging', async () => {
    const fake = fakeSupabaseStorage()
    const a = createSupabaseStorageAdapter(fake.client, 'audio-stems')
    await a.put('u/s/vocals.ogg', Buffer.from('abc'), { contentType: 'audio/ogg', cacheMaxAgeSec: 31536000 })
    assert.equal(fake.bucketOf('audio-stems').get('u/s/vocals.ogg')!.cacheControl, '31536000')
    assert.equal(a.publicUrl('u/s/vocals.ogg'), 'https://sb.test/storage/v1/object/public/audio-stems/u/s/vocals.ogg')
    assert.equal(createSupabaseStorageAdapter(fake.client, 'audio-stems', { publicBucket: false }).publicUrl('u/s/vocals.ogg'), null)

    for (let i = 0; i < 1005; i++) fake.bucketOf('audio-stems').set(`big/f${String(i).padStart(4, '0')}`, { body: Buffer.from('x') })
    assert.equal((await a.list('big')).length, 1005)
  })

  it('signed upload URL reports the real Supabase expiry (fixed 2h, M2)', async () => {
    const t = new Date('2026-01-01T00:00:00Z')
    const a = createSupabaseStorageAdapter(fakeSupabaseStorage().client, 'post-images', { now: () => t })
    const put = await a.signedPutUrl('u/p.png', { contentType: 'image/png', contentLength: 1, expiresInSec: 600 })
    assert.equal(put.expiresAt, '2026-01-01T02:00:00.000Z')
    assert.match(put.url, /\/object\/upload\/sign\/post-images\/u\/p\.png\?token=/)
  })
})

describe('readStorageConfig', () => {
  it('defaults to supabase', () => {
    assert.deepEqual(readStorageConfig({}), { driver: 'supabase' })
    assert.deepEqual(readStorageConfig({ STORAGE_DRIVER: 'Supabase' }), { driver: 'supabase' })
  })

  it('parses s3 settings (R2: path style default on, region auto)', () => {
    const cfg = readStorageConfig({
      STORAGE_DRIVER: 's3',
      S3_ENDPOINT: 'https://acc.r2.cloudflarestorage.com',
      S3_BUCKET: 'band',
      S3_ACCESS_KEY_ID: 'id',
      S3_SECRET_ACCESS_KEY: 'secret',
      S3_PUBLIC_BASE_URL: 'https://files.example',
    })
    assert.equal(cfg.driver, 's3')
    if (cfg.driver !== 's3') return
    assert.equal(cfg.s3.region, 'auto')
    assert.equal(cfg.s3.forcePathStyle, true)
    assert.equal(cfg.s3.publicBaseUrl, 'https://files.example')
    const garage = readStorageConfig({ STORAGE_DRIVER: 's3', S3_BUCKET: 'b', S3_ACCESS_KEY_ID: 'i', S3_SECRET_ACCESS_KEY: 's', S3_REGION: 'garage', S3_FORCE_PATH_STYLE: 'false' })
    assert.ok(garage.driver === 's3' && garage.s3.forcePathStyle === false && garage.s3.region === 'garage')
  })

  it('fails loudly on bad/missing settings (never silently falls back)', () => {
    assert.throws(() => readStorageConfig({ STORAGE_DRIVER: 'gcs' }), /supabase or s3/)
    assert.throws(() => readStorageConfig({ STORAGE_DRIVER: 's3', S3_BUCKET: 'b' }), /S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY/)
    assert.throws(() => readStorageConfig({ STORAGE_DRIVER: 's3', S3_BUCKET: 'b', S3_ACCESS_KEY_ID: 'i', S3_SECRET_ACCESS_KEY: 's', S3_PUBLIC_BASE_URL: 'cdn.example' }), /http/)
  })
})
