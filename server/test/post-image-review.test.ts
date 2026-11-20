/**
 * P3-10 리뷰 반영: 업로드 완료 검증(M2), 예전 파일명 호환(M3), 사용자별 상한·고아 정리(M4)
 */
import { after, before, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { createApp } from '../src/app'
import { setTokenVerifier } from '../src/lib/auth'
import { POST_IMAGE_MAX_BYTES, postImageKeyFromParams } from '../src/lib/imageStorage'
import {
  UploadQuota, VerifiedKeys, cleanupOrphanPostImages, postImageKeyFromAnyUrl, uploadedAtOfKey, verifyPostImageUpload,
} from '../src/services/postImageService'
import { bearer, testVerifier } from './helpers'
import { memoryStorage } from './fakes/memoryStorage'

const UID = '11111111-2222-3333-4444-555555555555'
const KEY = `${UID}/1700000000000-0f0e0d0c-0b0a-4908-8706-050403020100.png`
const KEY2 = `${UID}/1700000000001-1f0e0d0c-0b0a-4908-8706-050403020100.png`
const KEY3 = `${UID}/1700000000002-2f0e0d0c-0b0a-4908-8706-050403020100.png`
const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13])

describe('M3: legacy client file names are viewable', () => {
  it('accepts `${ms}-${base36}.${anything without . / \\}` and still blocks traversal', () => {
    for (const ok of ['1700000000000-k3j2h1.my photo', '1700000000000-abc.사진', '1700000000000-abc.a+b(1)', '1700000000000-abc.JPEG']) {
      assert.equal(postImageKeyFromParams(UID, ok), `${UID}/${ok}`, ok)
    }
    for (const bad of ['1700000000000-abc.x/y', '1700000000000-abc.a\\b', '1700000000000-abc..', '.hidden', '1700000000000-abc.\u0001x', '../1700000000000-abc.png']) {
      assert.equal(postImageKeyFromParams(UID, bad), null, JSON.stringify(bad))
    }
  })
})

describe('verifyPostImageUpload (pending → verified)', () => {
  const P = `pending/${KEY}`

  it('accepts a real image of the declared type, moves it out of pending, reads only the first bytes (M4)', async () => {
    const m = memoryStorage('post-images')
    await m.adapter.put(P, Buffer.concat([PNG, Buffer.alloc(100_000)]), { contentType: 'image/png' })
    assert.deepEqual(await verifyPostImageUpload(m.adapter, UID, KEY), { ok: true })
    assert.ok(m.objects.has(KEY))
    assert.ok(!m.objects.has(P))
    assert.equal(m.calls.filter(c => c.op === 'get').length, 0, 'never downloads the whole object')
    const ranges = m.calls.filter(c => c.op === 'getRange').map(c => c.arg as [string, number, number])
    assert.deepEqual(ranges, [[P, 0, 15]])
  })

  it('repeated /complete for a verified key does not touch storage again (cache), or only HEADs it (restart)', async () => {
    const m = memoryStorage('post-images')
    await m.adapter.put(P, PNG, { contentType: 'image/png' })
    const cache = new VerifiedKeys()
    assert.ok((await verifyPostImageUpload(m.adapter, UID, KEY, cache)).ok)
    const n = m.calls.length
    for (let i = 0; i < 5; i++) assert.ok((await verifyPostImageUpload(m.adapter, UID, KEY, cache)).ok)
    assert.equal(m.calls.length, n, 'cached: no storage calls')
    // 캐시 없음(재시작): pending 없음 + 최종 경로 HEAD로 성공, 내용은 안 읽음
    assert.ok((await verifyPostImageUpload(m.adapter, UID, KEY, new VerifiedKeys())).ok)
    assert.equal(m.calls.slice(n).filter(c => c.op === 'getRange' || c.op === 'get').length, 0)
  })

  it('deletes pending uploads whose content, stored type or size does not match (Supabase signed PUT cannot enforce them)', async () => {
    const cases: [Buffer, string, RegExp][] = [
      [Buffer.from('<html><script>alert(1)</script>'), 'image/png', /내용/],
      [PNG, 'text/html', /형식/],
      [Buffer.alloc(POST_IMAGE_MAX_BYTES + 1, 0x89), 'image/png', /크기/],
    ]
    for (const [body, type, msg] of cases) {
      const m = memoryStorage('post-images')
      await m.adapter.put(P, body, { contentType: type })
      const r = await verifyPostImageUpload(m.adapter, UID, KEY)
      assert.ok(!r.ok && r.status === 422 && msg.test(r.error) && r.deleted, `${type} ${body.length}`)
      assert.equal(m.objects.size, 0, 'nothing reaches the final path')
    }
  })

  it('refuses keys outside the caller\'s folder or not in server format (and does not delete them)', async () => {
    const m = memoryStorage('post-images')
    const other = KEY.replace(UID, 'someone-else')
    await m.adapter.put(`pending/${other}`, PNG, { contentType: 'image/png' })
    const r = await verifyPostImageUpload(m.adapter, UID, other)
    assert.ok(!r.ok && r.status === 400)
    assert.ok(m.objects.has(`pending/${other}`))
    assert.ok(!(await verifyPostImageUpload(m.adapter, UID, `${UID}/../x.png`)).ok)
    assert.ok(!(await verifyPostImageUpload(m.adapter, UID, `pending/${KEY}`)).ok)
    const missing = await verifyPostImageUpload(m.adapter, UID, KEY)
    assert.ok(!missing.ok && missing.status === 404)
  })
})

describe('M4: per-user upload quota', () => {
  it('limits count and bytes per 24h window per user', () => {
    let t = 0
    const q = new UploadQuota({ maxCount: 2, maxBytes: 10 }, () => t)
    assert.equal(q.tryConsume('a', 4), true)
    assert.equal(q.tryConsume('a', 7), false, 'bytes over')
    assert.equal(q.tryConsume('a', 6), true)
    assert.equal(q.tryConsume('a', 0), false, 'count over')
    assert.equal(q.tryConsume('b', 10), true, 'other user independent')
    t = 24 * 3600 * 1000 + 1
    assert.equal(q.tryConsume('a', 10), true, 'window slides')
  })
})

describe('routes: upload-url quota + complete + resolve', () => {
  const m = memoryStorage('post-images')
  const legacy = memoryStorage('post-images', { driver: 'supabase', publicBase: 'https://x.supabase.co/storage/v1/object/public' })
  let server: Server
  let url = ''
  before(async () => {
    setTokenVerifier(testVerifier)
    server = createApp({
      imageStorage: m.adapter,
      legacyImageStorage: legacy.adapter,
      rateLimits: false,
      imagePerUserPerMinute: 8,
      imageUploadQuota: new UploadQuota({ maxCount: 1, maxBytes: 100 }),
    }).listen(0)
    await new Promise(r => server.once('listening', r))
    url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
  })
  after(() => {
    server.close()
    setTokenVerifier()
  })
  const post = (path: string, body: unknown, user = UID) => fetch(`${url}/api/storage/post-images/${path}`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', ...bearer(user) }, body: JSON.stringify(body),
  })

  it('429 after the user quota', async () => {
    assert.equal((await post('upload-url', { contentType: 'image/png', size: 50 })).status, 200)
    assert.equal((await post('upload-url', { contentType: 'image/png', size: 1 })).status, 429)
  })

  it('complete: requires auth, verifies, returns the view URL; bad uploads are removed', async () => {
    const anon = await fetch(`${url}/api/storage/post-images/complete`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{"key":"x"}' })
    assert.equal(anon.status, 401)

    await m.adapter.put(`pending/${KEY}`, PNG, { contentType: 'image/png' })
    const ok = await post('complete', { key: KEY })
    assert.equal(ok.status, 200)
    assert.equal(((await ok.json()) as { url: string }).url, `/api/storage/post-images/${KEY}`)

    await m.adapter.put(`pending/${KEY2}`, Buffer.from('GIF89a-not-png'), { contentType: 'image/png' })
    assert.equal((await post('complete', { key: KEY2 })).status, 422)
    assert.equal(m.objects.has(`pending/${KEY2}`), false)
    assert.equal(m.objects.has(KEY2), false)

    assert.equal((await post('complete', { key: KEY }, 'intruder')).status, 400)
  })

  it('resolve (M1·M3): keeps verified images, maps migrated Supabase URLs, rejects pending/unknown/deleted', async () => {
    const SBP = 'https://x.supabase.co/storage/v1/object/public/post-images'
    const migrated = `${UID}/1600000000000-old1.png` // 원본 삭제됨, 대상(R2)에만 사본
    const notYet = `${UID}/1600000000001-old2.png` // 아직 Supabase에만
    const gone = `${UID}/1600000000002-old3.png` // 어디에도 없음
    await m.adapter.put(migrated, PNG)
    await legacy.adapter.put(notYet, PNG)
    await m.adapter.put(`pending/${KEY3}`, PNG, { contentType: 'image/png' })

    const r = await post('resolve', {
      urls: [
        `/api/storage/post-images/${KEY}`, // 검증 완료
        `${SBP}/${migrated}`,
        `${SBP}/${notYet}`,
        `${SBP}/${gone}`,
        `${SBP}/pending/${KEY3}`, // 미검증
        `/api/storage/post-images/${KEY3}`, // 미검증 key를 최종 경로처럼 주장
        'https://elsewhere.example/x.png',
      ],
    })
    assert.equal(r.status, 200)
    const j = await r.json() as { urls: string[]; rejected: { url: string }[] }
    assert.deepEqual(j.urls, [
      `/api/storage/post-images/${KEY}`,
      `/api/storage/post-images/${migrated}`,
      `${SBP}/${notYet}`,
      'https://elsewhere.example/x.png',
    ])
    assert.deepEqual(j.rejected.map(x => x.url), [`${SBP}/${gone}`, `${SBP}/pending/${KEY3}`, `/api/storage/post-images/${KEY3}`])
  })

  it('per-user limit on complete/resolve (M4)', async () => {
    let last = 200
    for (let i = 0; i < 12 && last !== 429; i++) last = (await post('complete', { key: KEY }, 'limited-user')).status
    assert.equal(last, 429)
  })
})

describe('M4: orphan post image cleanup', () => {
  const DAY = 24 * 3600 * 1000
  const now = 1700000000000 + 3 * DAY
  const old = (n: number) => `${UID}/${1700000000000 + n}-k${n}.png`

  it('extracts keys from every stored URL form and the upload time from the file name', () => {
    assert.equal(postImageKeyFromAnyUrl(`https://x.supabase.co/storage/v1/object/public/post-images/${old(1)}`), old(1))
    assert.equal(postImageKeyFromAnyUrl(`https://api.example/api/storage/post-images/${UID}/1700000000001-k1.my%20photo`), `${UID}/1700000000001-k1.my photo`)
    assert.equal(postImageKeyFromAnyUrl(`https://cdn.example/post-images/${old(2)}`), old(2))
    assert.equal(postImageKeyFromAnyUrl('https://elsewhere/x.png'), null)
    assert.equal(uploadedAtOfKey(old(5)), 1700000000005)
    assert.equal(uploadedAtOfKey(`${UID}/avatar.png`), null)
  })

  it('deletes only unreferenced objects older than the grace period; dry run deletes nothing', async () => {
    const m = memoryStorage('post-images')
    for (const k of [old(1), old(2), `${UID}/${now - 1000}-fresh.png`, `${UID}/no-timestamp.png`]) await m.adapter.put(k, Buffer.from('x'))
    const refs = async () => [`https://x.supabase.co/storage/v1/object/public/post-images/${old(1)}`]

    const dry = await cleanupOrphanPostImages(m.adapter, refs, { apply: false, now })
    assert.deepEqual(dry.orphans, [old(2)])
    assert.equal(m.objects.size, 4)

    const real = await cleanupOrphanPostImages(m.adapter, refs, { apply: true, now })
    assert.equal(real.deleted, 1)
    assert.deepEqual([...m.objects.keys()].sort(), [old(1), `${UID}/${now - 1000}-fresh.png`, `${UID}/no-timestamp.png`].sort())
  })

  it('deletes nothing when posts cannot be read', async () => {
    const m = memoryStorage('post-images')
    await m.adapter.put(old(1), Buffer.from('x'))
    await assert.rejects(cleanupOrphanPostImages(m.adapter, async () => { throw new Error('db down') }, { apply: true, now }))
    assert.equal(m.objects.size, 1)
  })
})
