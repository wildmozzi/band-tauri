/**
 * P3-10d: 게시글 작성·수정 서버 API (POST/PUT /api/posts)
 * - 작성자는 토큰의 userId (본문 author_id 무시), 남의 글 수정 거부
 * - posts.images에는 검증 완료(최종 경로) + 본인 + 서버 형식 key만, 저장값은 서버 조회 URL로 정규화
 * - 위조 참조(외부 URL·pending·미업로드·남의 이미지·호스트 바꿔치기) 거부, 기존 글의 예전 이미지는 하위 호환 유지
 */
import { after, before, beforeEach, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { createApp } from '../src/app'
import { setTokenVerifier } from '../src/lib/auth'
import { newPostImageKey, postImageViewUrl } from '../src/lib/imageStorage'
import type { NewPost, PostPatch, PostRepository, PostRow } from '../src/repositories/postRepository'
import { pendingKeyOf } from '../src/services/postImageService'
import type { StorageAdapter } from '../src/storage/types'
import { bearer, testVerifier } from './helpers'
import { memoryStorage } from './fakes/memoryStorage'

const ALICE = '11111111-2222-3333-4444-555555555555'
const BOB = '66666666-7777-8888-9999-000000000000'
const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0])

function memoryPosts() {
  const rows = new Map<string, PostRow>()
  let seq = 0
  const repo: PostRepository = {
    async findForEdit(id) {
      const r = rows.get(id)
      return r ? { authorId: r.author_id, images: r.images ?? [] } : null
    },
    async displayName(userId) {
      return userId === ALICE ? 'alice' : 'User'
    },
    async insert(p: NewPost) {
      const id = `00000000-0000-4000-8000-${String(++seq).padStart(12, '0')}`
      const row: PostRow = {
        id, author_id: p.authorId, author_name: p.authorName, author_role: p.authorRole, title: p.title, content: p.content,
        tags: p.tags, images: p.images, instrument: p.instrument, category: p.category, likes_count: 0, comments_count: 0,
      }
      rows.set(id, row)
      return row
    },
    async update(id, authorId, patch: PostPatch) {
      const r = rows.get(id)
      if (!r || r.author_id !== authorId) return null
      const next = { ...r, ...patch } as PostRow
      rows.set(id, next)
      return next
    },
  }
  return { repo, rows }
}

async function listen(opts: { images: StorageAdapter | null; posts: PostRepository | null }) {
  const server = createApp({ imageStorage: opts.images, postRepository: opts.posts, rateLimits: false }).listen(0)
  await new Promise(r => server.once('listening', r))
  return { server, url: `http://127.0.0.1:${(server.address() as { port: number }).port}` }
}

const req = (base: string, method: 'POST' | 'PUT', path: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(`${base}/api/posts${path}`, { method, headers: { 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) })

describe('POST/PUT /api/posts', () => {
  const mem = memoryStorage('post-images')
  const posts = memoryPosts()
  let s: { server: Server; url: string }

  /** 검증 완료(최종 경로) 이미지 하나 */
  async function verifiedImage(owner = ALICE): Promise<{ key: string; url: string }> {
    const key = newPostImageKey(owner, 'image/png')
    await mem.adapter.put(key, PNG, { contentType: 'image/png' })
    return { key, url: postImageViewUrl(mem.adapter, key) }
  }

  before(async () => {
    setTokenVerifier(testVerifier)
    s = await listen({ images: mem.adapter, posts: posts.repo })
  })
  after(() => {
    s.server.close()
    setTokenVerifier()
  })
  beforeEach(() => posts.rows.clear())

  it('requires authentication', async () => {
    assert.equal((await req(s.url, 'POST', '', { title: 't', content: 'c' })).status, 401)
    assert.equal((await req(s.url, 'PUT', '/00000000-0000-4000-8000-000000000001', { title: 't' })).status, 401)
  })

  it('creates a post as the token user (client author fields ignored) with verified images normalized to the view URL', async () => {
    const img = await verifiedImage()
    // 다른 호스트로 바꿔 보낸 같은 key도 서버 조회 URL로 저장된다
    const swapped = `https://evil.supabase.co/storage/v1/object/public/post-images/${img.key}`
    const res = await req(s.url, 'POST', '', {
      title: ' 첫 글 ', content: '내용', tags: ['jam'], images: [img.url, swapped], instrument: 'guitar', category: 'session',
      author_id: BOB, author_name: 'admin',
    }, bearer(ALICE))
    assert.equal(res.status, 201)
    const { post } = await res.json() as { post: PostRow }
    assert.equal(post.author_id, ALICE)
    assert.equal(post.author_name, 'alice')
    assert.equal(post.author_role, 'Session Member')
    assert.equal(post.title, '첫 글')
    assert.deepEqual(post.images, [img.url])
    assert.equal(post.instrument, 'guitar')
    assert.equal(post.category, 'session')
  })

  it('rejects forged image references with 422 and writes nothing', async () => {
    const mine = await verifiedImage()
    const bobs = await verifiedImage(BOB)
    const pendingKey = newPostImageKey(ALICE, 'image/png')
    await mem.adapter.put(pendingKeyOf(pendingKey), PNG, { contentType: 'image/png' })
    const legacyStyleKey = `${ALICE}/1700000000000-photo.png` // 서버 발급 형식이 아님 (예전 클라이언트 직접 업로드)
    await mem.adapter.put(legacyStyleKey, PNG, { contentType: 'image/png' })

    const forged: [string, string][] = [
      ['https://evil.test/x.png', '이 서비스에 올린 이미지만'],
      [postImageViewUrl(mem.adapter, pendingKeyOf(pendingKey)), '검증되지 않은 업로드'],
      [postImageViewUrl(mem.adapter, pendingKey), '업로드가 확인되지 않은'], // 업로드했지만 /complete 안 거침
      [postImageViewUrl(mem.adapter, newPostImageKey(ALICE, 'image/png')), '업로드가 확인되지 않은'], // 없는 key
      [bobs.url, '본인이 올린 이미지만'],
      [postImageViewUrl(mem.adapter, legacyStyleKey), '본인이 올린 이미지만'],
      ['https://api.test/api/storage/post-images/..%2F..%2Fsecret', '잘못된 이미지 경로'],
    ]
    for (const [url, reason] of forged) {
      const res = await req(s.url, 'POST', '', { title: 't', content: 'c', images: [mine.url, url] }, bearer(ALICE))
      assert.equal(res.status, 422, url)
      const body = await res.json() as { rejected: { url: string; reason: string }[] }
      assert.equal(body.rejected.length, 1, url)
      assert.equal(body.rejected[0].url, url)
      assert.ok(body.rejected[0].reason.includes(reason), `${url}: ${body.rejected[0].reason}`)
    }
    assert.equal(posts.rows.size, 0)
  })

  it('validates the body (400)', async () => {
    const bad = [
      {},
      { title: '', content: 'c' },
      { title: 't', content: 'c', category: 'admin' },
      { title: 't', content: 'c', instrument: 'kazoo' },
      { title: 't', content: 'c', images: ['a', 'b', 'c', 'd', 'e', 'f'] },
      { title: 't', content: 'c', tags: 'jam' },
    ]
    for (const body of bad) assert.equal((await req(s.url, 'POST', '', body, bearer(ALICE))).status, 400, JSON.stringify(body))
    assert.equal((await req(s.url, 'PUT', '/not-a-uuid', { title: 't' }, bearer(ALICE))).status, 400)
  })

  it('rejects edits by someone other than the author (403) and unknown posts (404)', async () => {
    const created = await (await req(s.url, 'POST', '', { title: 'mine', content: 'c' }, bearer(ALICE))).json() as { post: PostRow }
    const res = await req(s.url, 'PUT', `/${created.post.id}`, { title: 'pwned', images: [] }, bearer(BOB))
    assert.equal(res.status, 403)
    assert.equal(posts.rows.get(created.post.id)?.title, 'mine')
    assert.equal((await req(s.url, 'PUT', '/99999999-9999-4999-8999-999999999999', { title: 'x' }, bearer(ALICE))).status, 404)
  })

  it('author edit: keeps images already in the post (legacy), adds verified ones, rejects forged ones without writing', async () => {
    const legacy = 'https://old.supabase.co/storage/v1/object/public/post-images/someone/legacy.png'
    const created = await posts.repo.insert({
      authorId: ALICE, authorName: 'alice', authorRole: 'Session Member', title: 'old', content: 'c',
      tags: [], images: ['https://cdn.example/external-from-before.png', legacy], instrument: 'other', category: 'general',
    })
    const added = await verifiedImage()
    const ok = await req(s.url, 'PUT', `/${created.id}`, {
      title: 'new', images: ['https://cdn.example/external-from-before.png', legacy, added.url],
    }, bearer(ALICE))
    assert.equal(ok.status, 200)
    const { post } = await ok.json() as { post: PostRow }
    assert.equal(post.title, 'new')
    assert.deepEqual(post.images, ['https://cdn.example/external-from-before.png', legacy, added.url])

    const forged = await req(s.url, 'PUT', `/${created.id}`, { title: 'newer', images: [...post.images!, 'https://evil.test/y.png'] }, bearer(ALICE))
    assert.equal(forged.status, 422)
    assert.equal(posts.rows.get(created.id)?.title, 'new')

    // 이미지 없이 본문만 수정하면 이미지는 그대로
    const textOnly = await req(s.url, 'PUT', `/${created.id}`, { content: 'edited' }, bearer(ALICE))
    assert.equal(textOnly.status, 200)
    assert.deepEqual(posts.rows.get(created.id)?.images, post.images)
  })

  it('a migrated legacy image in the post is rewritten to the current view URL', async () => {
    const key = `${ALICE}/1700000000000-photo-migrated.png`
    await mem.adapter.put(key, PNG, { contentType: 'image/png' })
    const legacyUrl = `https://old.supabase.co/storage/v1/object/public/post-images/${key}`
    const created = await posts.repo.insert({
      authorId: ALICE, authorName: 'alice', authorRole: 'Session Member', title: 'old', content: 'c',
      tags: [], images: [legacyUrl], instrument: 'other', category: 'general',
    })
    const res = await req(s.url, 'PUT', `/${created.id}`, { images: [legacyUrl] }, bearer(ALICE))
    assert.equal(res.status, 200)
    assert.deepEqual(((await res.json()) as { post: PostRow }).post.images, [postImageViewUrl(mem.adapter, key)])
  })
})

describe('POST /api/posts without backends', () => {
  let s: { server: Server; url: string }
  const posts = memoryPosts()
  before(async () => {
    setTokenVerifier(testVerifier)
  })
  after(() => {
    s?.server.close()
    setTokenVerifier()
  })

  it('503 when the posts store is not configured', async () => {
    s = await listen({ images: null, posts: null })
    assert.equal((await req(s.url, 'POST', '', { title: 't', content: 'c' }, bearer(ALICE))).status, 503)
    s.server.close()
  })

  it('without an image store: text-only posts work, new image references → 503', async () => {
    s = await listen({ images: null, posts: posts.repo })
    assert.equal((await req(s.url, 'POST', '', { title: 't', content: 'c' }, bearer(ALICE))).status, 201)
    const res = await req(s.url, 'POST', '', { title: 't', content: 'c', images: [`https://api.test/api/storage/post-images/${newPostImageKey(ALICE, 'image/png')}`] }, bearer(ALICE))
    assert.equal(res.status, 503)
  })
})
