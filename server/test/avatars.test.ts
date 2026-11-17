/**
 * P2-12: 프로필 사진을 Storage로 — user_metadata에는 URL만.
 * - 업로드 경로: 인증 필수, MIME(정지 이미지)·크기(2MB) 제한, key는 서버가 JWT 신원 폴더로 정함
 * - /complete: 검증(매직 바이트) → 최종 경로 → metadata에 URL + 예전 base64 키 삭제 → 예전 사진 삭제
 * - 남의 key·미검증 key 거부, 저장소/metadata 미설정 503
 * - JWT 크기: metadata 갱신 내용은 짧은 URL뿐이고 data: URL(base64)이 들어가지 않는다
 */
import { after, before, beforeEach, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { createApp } from '../src/app'
import { getBearerToken, setTokenVerifier } from '../src/lib/auth'
import { AVATAR_MAX_BYTES, avatarViewUrl } from '../src/lib/avatarStorage'
import { profilePhotoMetadataPatch, type ProfilePhotoMetadata } from '../src/services/profilePhotoService'
import type { StorageAdapter } from '../src/storage/types'
import { bearer, testVerifier } from './helpers'
import { memoryStorage } from './fakes/memoryStorage'

const UID = '11111111-2222-3333-4444-555555555555'
const OTHER = '99999999-2222-3333-4444-555555555555'
const PNG = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(512, 7)])

function fakeMeta() {
  const writes: { userId: string; url: string | null }[] = []
  const meta: ProfilePhotoMetadata = {
    async set(userId, url) {
      writes.push({ userId, url })
    },
  }
  return { meta, writes }
}

async function listen(storage: StorageAdapter | null, meta: ProfilePhotoMetadata | null): Promise<{ server: Server; url: string }> {
  const server = createApp({ avatarStorage: storage, profilePhotoMetadata: meta, imageStorage: null, rateLimits: false }).listen(0)
  await new Promise(r => server.once('listening', r))
  return { server, url: `http://127.0.0.1:${(server.address() as { port: number }).port}` }
}

const call = (url: string, path: string, body: unknown, headers: Record<string, string> = {}, method = 'POST') =>
  fetch(`${url}/api/storage/avatars${path}`, {
    method,
    headers: { 'Content-Type': 'application/json', ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  })

describe('프로필 사진 업로드 (/api/storage/avatars)', () => {
  let mem: ReturnType<typeof memoryStorage>
  let m: ReturnType<typeof fakeMeta>
  let s: { server: Server; url: string }
  before(() => setTokenVerifier(testVerifier))
  after(() => setTokenVerifier())
  beforeEach(async () => {
    s?.server.close()
    mem = memoryStorage('avatars')
    m = fakeMeta()
    s = await listen(mem.adapter, m.meta)
  })
  after(() => s?.server.close())

  /** 서명 URL 발급 → (클라이언트 PUT 대신) pending에 직접 기록 → key */
  async function uploadPending(body: Buffer, contentType = 'image/png', uid = UID): Promise<string> {
    const r = await call(s.url, '/upload-url', { contentType, size: body.length }, bearer(uid))
    assert.equal(r.status, 200)
    const j = await r.json() as { key: string }
    await mem.adapter.put(`pending/${j.key}`, body, { contentType })
    return j.key
  }

  it('인증이 없으면 401', async () => {
    assert.equal((await call(s.url, '/upload-url', { contentType: 'image/png', size: 10 })).status, 401)
    assert.equal((await call(s.url, '/complete', { key: `${UID}/x.png` })).status, 401)
    assert.equal((await call(s.url, '', undefined, {}, 'DELETE')).status, 401)
  })

  it('정지 이미지·2MB 이하만, 클라이언트가 보낸 경로·userId는 무시', async () => {
    for (const body of [
      { contentType: 'image/gif', size: 10 },
      { contentType: 'image/svg+xml', size: 10 },
      { contentType: 'image/png', size: 0 },
      { contentType: 'image/png' },
    ]) {
      assert.equal((await call(s.url, '/upload-url', body, bearer(UID))).status, 400, JSON.stringify(body))
    }
    assert.equal((await call(s.url, '/upload-url', { contentType: 'image/jpeg', size: AVATAR_MAX_BYTES + 1 }, bearer(UID))).status, 413)

    const r = await call(s.url, '/upload-url', { contentType: 'image/webp', size: 100, key: '../x', userId: OTHER }, bearer(UID))
    const j = await r.json() as { key: string; upload: { method: string } }
    assert.match(j.key, new RegExp(`^${UID}/\\d+-[0-9a-f-]{36}\\.webp$`))
    const [signedKey] = mem.calls.find(c => c.op === 'signPut')!.arg as [string]
    assert.equal(signedKey, `pending/${j.key}`, '미검증 업로드는 pending/ 에만')
  })

  it('/complete: 검증 → 최종 경로 → metadata에 URL만, 예전 사진 삭제', async () => {
    const first = await uploadPending(PNG)
    const r1 = await call(s.url, '/complete', { key: first }, bearer(UID))
    assert.equal(r1.status, 200)
    const j1 = await r1.json() as { url: string }
    assert.ok(mem.objects.has(first))
    assert.ok(!mem.objects.has(`pending/${first}`))
    assert.deepEqual(m.writes, [{ userId: UID, url: j1.url }])
    assert.equal(j1.url, avatarViewUrl(mem.adapter, first))

    const second = await uploadPending(PNG)
    assert.equal((await call(s.url, '/complete', { key: second }, bearer(UID))).status, 200)
    assert.ok(mem.objects.has(second))
    assert.ok(!mem.objects.has(first), '예전 사진은 지운다')
    assert.equal(m.writes.length, 2)
  })

  it('남의 key·형식이 다른 내용·미업로드는 거부하고 metadata를 건드리지 않는다', async () => {
    const mine = await uploadPending(PNG)
    assert.equal((await call(s.url, '/complete', { key: mine }, bearer(OTHER))).status, 400)

    const fake = await uploadPending(Buffer.from('<html>not an image</html>'))
    assert.equal((await call(s.url, '/complete', { key: fake }, bearer(UID))).status, 422)
    assert.ok(!mem.objects.has(`pending/${fake}`), '검증 실패 업로드는 지운다')

    assert.equal((await call(s.url, '/complete', { key: `${UID}/1700000000000-00000000-0000-0000-0000-000000000000.png` }, bearer(UID))).status, 404)
    assert.equal((await call(s.url, '/complete', { key: `pending/${mine}` }, bearer(UID))).status, 400)
    assert.equal(m.writes.length, 0)
  })

  it('게시글 이미지 key 형식이어도 gif는 프로필 사진으로 받지 않는다', async () => {
    await mem.adapter.put(`pending/${UID}/1700000000000-00000000-0000-0000-0000-000000000000.gif`, Buffer.from('GIF89a......'), { contentType: 'image/gif' })
    assert.equal((await call(s.url, '/complete', { key: `${UID}/1700000000000-00000000-0000-0000-0000-000000000000.gif` }, bearer(UID))).status, 400)
  })

  it('지우기: metadata를 비우고 본인 사진만 지운다', async () => {
    const mine = await uploadPending(PNG)
    await call(s.url, '/complete', { key: mine }, bearer(UID))
    const theirs = await uploadPending(PNG, 'image/png', OTHER)
    await call(s.url, '/complete', { key: theirs }, bearer(OTHER))

    assert.equal((await call(s.url, '', undefined, bearer(UID), 'DELETE')).status, 200)
    assert.deepEqual(m.writes.at(-1), { userId: UID, url: null })
    assert.ok(!mem.objects.has(mine))
    assert.ok(mem.objects.has(theirs))
  })

  it('조회는 서명 GET으로 302, 경로 순회는 400', async () => {
    const r = await fetch(`${s.url}/api/storage/avatars/${UID}/1700000000000-abc.png`, { redirect: 'manual' })
    assert.equal(r.status, 302)
    assert.equal(r.headers.get('location'), `https://signed.test/avatars/${UID}/1700000000000-abc.png?ttl=3600`)
    assert.equal((await fetch(`${s.url}/api/storage/avatars/${UID}/..%2Fx.png`, { redirect: 'manual' })).status, 400)
  })

  it('저장소나 metadata 기록이 없으면 503', async () => {
    const none = await listen(null, m.meta)
    const noMeta = await listen(mem.adapter, null)
    try {
      assert.equal((await call(none.url, '/upload-url', { contentType: 'image/png', size: 10 }, bearer(UID))).status, 503)
      assert.equal((await call(noMeta.url, '/upload-url', { contentType: 'image/png', size: 10 }, bearer(UID))).status, 503)
    } finally {
      none.server.close()
      noMeta.server.close()
    }
  })
})

describe('JWT 크기 (P2-12)', () => {
  it('metadata 갱신 내용은 짧은 URL뿐 — base64 사진이 들어가지 않고 예전 base64 키는 지운다', () => {
    const storage = memoryStorage('avatars').adapter
    const key = `${UID}/1700000000000-00000000-0000-0000-0000-000000000000.webp`
    const patch = profilePhotoMetadataPatch(avatarViewUrl(storage, key, 'https://api.example'))
    assert.equal(patch.profile_photo, null, '예전 base64 키(profile_photo)는 null로 지운다')
    assert.match(patch.profile_photo_url!, /^https:\/\/api\.example\/api\/storage\/avatars\//)
    const json = JSON.stringify(patch)
    assert.ok(!json.includes('data:'), 'data: URL이 metadata에 들어가면 안 된다')
    // URL 하나는 수백 바이트 — JWT에 붙어도 서버 토큰 상한(8KB)과 멀다
    assert.ok(json.length < 300, `metadata 갱신 ${json.length}B`)
  })

  it('예전 방식(사진 base64를 metadata에)은 토큰이 서버 상한을 넘어 인증이 거부될 수 있다 — 바꾼 이유', () => {
    const base64Photo = `data:image/jpeg;base64,${Buffer.alloc(100 * 1024).toString('base64')}`
    const fakeJwt = `h.${Buffer.from(JSON.stringify({ user_metadata: { profile_photo: base64Photo } })).toString('base64url')}.s`
    assert.equal(getBearerToken(`Bearer ${fakeJwt}`), null)
    const smallJwt = `h.${Buffer.from(JSON.stringify({ user_metadata: profilePhotoMetadataPatch('https://api.example/api/storage/avatars/u/x.webp') })).toString('base64url')}.s`
    assert.equal(getBearerToken(`Bearer ${smallJwt}`), smallJwt)
  })
})
