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
      return r
        ? {
            authorId: r.author_id, images: r.images ?? [], title: r.title as string, content: r.content as string,
            tags: (r.tags as string[] | null) ?? [], instrument: r.instrument as string, category: r.category as string,
          }
        : null
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
      [postImageViewUrl(mem.adapter, legacyStyleKey), '이 형식의 이미지는 다시 붙일 수 없습니다'],
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
    // 400에는 필드별 이유가 담긴다 (PR #25 M2)
    const res = await req(s.url, 'POST', '', { title: 'x'.repeat(201), content: 'c', instrument: 'kazoo' }, bearer(ALICE))
    const body = await res.json() as { issues: { field: string; message: string }[] }
    assert.deepEqual(body.issues.map(i => i.field).sort(), ['instrument', 'title'])
    assert.ok(body.issues.find(i => i.field === 'title')!.message.includes('200자'))
  })

  it('legacy posts that break the new rules stay editable: unchanged old values pass, new values follow the rules (PR #25 M2)', async () => {
    const oldImages = Array.from({ length: 7 }, (_, i) => `https://cdn.example/legacy-${i}.png`)
    const oldTags = Array.from({ length: 22 }, (_, i) => (i === 0 ? 'x'.repeat(60) : `tag${i}`))
    const legacy = await posts.repo.insert({
      authorId: ALICE, authorName: 'alice', authorRole: 'Session Member', title: 'T'.repeat(250), content: 'c',
      tags: oldTags, images: oldImages, instrument: 'Guitar', category: 'general',
    })
    // EditPost는 모든 필드를 보낸다: 내용만 바꿔도 통과
    const ok = await req(s.url, 'PUT', `/${legacy.id}`, {
      title: 'T'.repeat(250), content: 'edited', tags: oldTags, images: oldImages, instrument: 'Guitar', category: 'general',
    }, bearer(ALICE))
    assert.equal(ok.status, 200)
    const row = posts.rows.get(legacy.id)!
    assert.equal(row.content, 'edited')
    assert.equal(row.title, 'T'.repeat(250))
    assert.equal(row.instrument, 'Guitar')
    assert.deepEqual(row.images, oldImages)
    // 줄이는 건 된다 (태그 하나 빼기, 이미지 하나 빼기)
    const shrink = await req(s.url, 'PUT', `/${legacy.id}`, { tags: oldTags.slice(1), images: oldImages.slice(1) }, bearer(ALICE))
    assert.equal(shrink.status, 200)

    // 바꾼 값은 새 규칙: 너무 긴 새 제목·목록 밖 새 악기·새 긴 태그 → 400 + 필드별 이유, 아무것도 쓰지 않음
    const bad = await req(s.url, 'PUT', `/${legacy.id}`, {
      title: 'U'.repeat(201), instrument: 'kazoo', tags: [...oldTags.slice(1), 'y'.repeat(51)],
    }, bearer(ALICE))
    assert.equal(bad.status, 400)
    const { issues } = await bad.json() as { issues: { field: string; message: string }[] }
    assert.deepEqual(issues.map(i => i.field).sort(), ['instrument', 'tags', 'title'])
    assert.equal(posts.rows.get(legacy.id)!.title, 'T'.repeat(250))
    // 이미지 개수는 기존 개수를 넘을 수 없다
    const more = await req(s.url, 'PUT', `/${legacy.id}`, { images: [...oldImages, 'https://cdn.example/new.png'] }, bearer(ALICE))
    assert.equal(more.status, 400)
  })

  it('existing images match by key: a relative stored path sent back absolutized stays editable even if the file is gone (PR #25 L2)', async () => {
    const goneKey = newPostImageKey(ALICE, 'image/png') // 저장소에 없음 (정리·이전으로 사라짐)
    const relative = `/api/storage/post-images/${goneKey}` // PUBLIC_API_URL 없이 저장된 값
    const created = await posts.repo.insert({
      authorId: ALICE, authorName: 'alice', authorRole: 'Session Member', title: 't', content: 'c',
      tags: [], images: [relative], instrument: 'other', category: 'general',
    })
    // 클라이언트 toPost가 절대 URL로 바꿔 폼에 넣고 그대로 돌려보낸다 → 422가 아니라 원래 저장값 유지
    const absolutized = `https://api.example${relative}`
    const res = await req(s.url, 'PUT', `/${created.id}`, { content: 'edited', images: [absolutized] }, bearer(ALICE))
    assert.equal(res.status, 200)
    assert.deepEqual(posts.rows.get(created.id)!.images, [relative])
    // 같은 key를 다른 호스트로 보내도 클라이언트 URL은 저장되지 않는다
    const swapped = await req(s.url, 'PUT', `/${created.id}`, { images: [`https://evil.test/post-images/${goneKey}`] }, bearer(ALICE))
    assert.equal(swapped.status, 200)
    assert.deepEqual(posts.rows.get(created.id)!.images, [relative])
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

describe('per-user post write limits (P3-10g, PR #25 L4)', () => {
  let server: Server
  let url: string
  const posts = memoryPosts()
  before(async () => {
    setTokenVerifier(testVerifier)
    server = createApp({
      imageStorage: null, postRepository: posts.repo, rateLimits: false,
      postUserLimits: { createPerMinute: 2, createPerDay: 3, editPerMinute: 2 },
    }).listen(0)
    await new Promise(r => server.once('listening', r))
    url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
  })
  after(() => {
    server.close()
    setTokenVerifier()
  })

  it('limits each user by token (not IP) and leaves other users alone', async () => {
    // 같은 사용자가 IP(X-Forwarded-For)를 바꿔도 같은 상한
    const statuses: number[] = []
    for (let i = 0; i < 3; i++) {
      statuses.push((await req(url, 'POST', '', { title: `t${i}`, content: 'c' }, { ...bearer(ALICE), 'X-Forwarded-For': `10.0.0.${i}` })).status)
    }
    assert.deepEqual(statuses, [201, 201, 429])
    const limited = await req(url, 'POST', '', { title: 'x', content: 'c' }, bearer(ALICE))
    assert.equal(limited.status, 429)
    assert.match(((await limited.json()) as { error: string }).error, /너무 자주/)
    assert.equal(posts.rows.size, 2)
    // 다른 사용자는 영향 없음
    assert.equal((await req(url, 'POST', '', { title: 'b', content: 'c' }, bearer(BOB))).status, 201)
  })

  it('limits edits per user separately from creates', async () => {
    const [id] = [...posts.rows.values()].filter(r => r.author_id === BOB).map(r => r.id)
    const statuses: number[] = []
    for (let i = 0; i < 3; i++) statuses.push((await req(url, 'PUT', `/${id}`, { content: `c${i}` }, bearer(BOB))).status)
    assert.deepEqual(statuses, [200, 200, 429])
  })
})
