/**
 * P3-10: Supabase → S3 이전 스크립트 (메모리 저장소·메모리 DB로, 실제 실행 없이)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import {
  migrate, parseArgs, isRoutableImageKey, planCopy, rewritePostImages, rewriteStemRow, supabaseKeyFromUrl, supabaseKeyFromAnyUrl, contentTypeOf,
  type MigrationDb, type StemRowForMigration,
} from '../scripts/migrate-storage'
import { STEM_REF_PREFIX } from '../src/lib/stemStorage'
import type { LogicalBucket } from '../src/storage/types'
import { memoryStorage } from './fakes/memoryStorage'

const SB = 'https://x.supabase.co/storage/v1/object/public'

describe('parseArgs', () => {
  it('is a dry run by default and never deletes unless asked', () => {
    assert.deepEqual(parseArgs([]), { apply: false, deleteSource: false, buckets: ['audio-stems', 'post-images'], limit: null })
    assert.equal(parseArgs(['--apply']).deleteSource, false)
    assert.deepEqual(parseArgs(['--apply', '--delete-source', '--bucket', 'post-images', '--limit', '5']),
      { apply: true, deleteSource: true, buckets: ['post-images'], limit: 5 })
  })
  it('rejects --delete-source without --apply and unknown args', () => {
    assert.throws(() => parseArgs(['--delete-source']), /requires --apply/)
    assert.throws(() => parseArgs(['--bucket', 'avatars']), /must be one of/)
    assert.throws(() => parseArgs(['--yes']), /unknown/)
    assert.throws(() => parseArgs(['--limit', '0']), /positive/)
  })
})

describe('pure helpers', () => {
  it('planCopy skips same-size objects already in the target', () => {
    const p = planCopy([{ key: 'a', size: 1 }, { key: 'b', size: 2 }, { key: 'c', size: 3 }], new Map([['a', 1], ['b', 99]]))
    assert.deepEqual(p.present.map(o => o.key), ['a'])
    assert.deepEqual(p.copy.map(o => o.key), ['b', 'c'])
  })
  it('supabaseKeyFromUrl only matches the given bucket', () => {
    assert.equal(supabaseKeyFromUrl(`${SB}/post-images/u/a%20b.png`, 'post-images'), 'u/a b.png')
    assert.equal(supabaseKeyFromUrl(`${SB}/audio-stems/u/s/v.ogg`, 'post-images'), null)
    assert.equal(supabaseKeyFromUrl('not a url', 'post-images'), null)
  })
  it('rewriteStemRow rewrites only verified Supabase URLs inside the folder', () => {
    const target = memoryStorage('audio-stems').adapter
    const row: StemRowForMigration = {
      id: 's', storage_folder: 'u/s',
      vocals_url: `${SB}/audio-stems/u/s/vocals.ogg`,
      drums_url: `${SB}/audio-stems/u/s/drums.ogg`, // 검증 안 됨
      bass_url: `${STEM_REF_PREFIX}u/s/bass.ogg`, // 이미 이전됨
      other_url: `${SB}/audio-stems/u/OTHER/other.ogg`, // 폴더 밖
      piano_url: null,
    }
    assert.deepEqual(rewriteStemRow(row, new Set(['u/s/vocals.ogg', 'u/s/bass.ogg', 'u/OTHER/other.ogg']), target), {
      vocals_url: `${STEM_REF_PREFIX}u/s/vocals.ogg`,
    })
    const pub = memoryStorage('audio-stems', { publicBase: 'https://cdn' }).adapter
    assert.deepEqual(rewriteStemRow(row, new Set(['u/s/vocals.ogg']), pub), { vocals_url: 'https://cdn/audio-stems/u/s/vocals.ogg' })
  })
  it('rewritePostImages keeps unknown/unverified URLs', () => {
    const view = (k: string) => `https://api/i/${k}`
    assert.deepEqual(rewritePostImages([`${SB}/post-images/u/1700000000000-a1.png`, 'https://other/x.png', `${SB}/post-images/u/2.png`], new Set(['u/1700000000000-a1.png']), view),
      ['https://api/i/u/1700000000000-a1.png', 'https://other/x.png', `${SB}/post-images/u/2.png`])
  })
  it('M3: rewritePostImages leaves keys the view route cannot serve (and reports them)', () => {
    const view = (k: string) => `https://api/i/${k}`
    const bad = 'u/sub/1700000000000-a.png' // 중첩 경로: /:owner/:file 라우트로 열 수 없음
    const seen: string[] = []
    assert.equal(rewritePostImages([`${SB}/post-images/${bad}`], new Set([bad]), view, k => seen.push(k)), null)
    assert.deepEqual(seen, [bad])
    assert.equal(isRoutableImageKey('u/1700000000000-k3j2h1.my photo'), true)
    assert.equal(rewritePostImages(['https://other/x.png'], new Set(), view), null)
  })
  it('contentTypeOf', () => {
    assert.equal(contentTypeOf('a/b.ogg'), 'audio/ogg')
    assert.equal(contentTypeOf('a/b.JPG'), 'image/jpeg')
    assert.equal(contentTypeOf('a/b.bin'), 'application/octet-stream')
  })
})

function world() {
  const src = { 'audio-stems': memoryStorage('audio-stems', { driver: 'supabase' }), 'post-images': memoryStorage('post-images', { driver: 'supabase' }) }
  const dst = { 'audio-stems': memoryStorage('audio-stems'), 'post-images': memoryStorage('post-images') }
  src['audio-stems'].objects.set('u/s/vocals.ogg', { body: Buffer.from('vvvv') })
  src['audio-stems'].objects.set('u/s/drums.wav', { body: Buffer.from('dd') })
  src['post-images'].objects.set('u/1700000000000-a1.png', { body: Buffer.from('png1') })
  const stemRows: StemRowForMigration[] = [{
    id: 's', storage_folder: 'u/s',
    vocals_url: `${SB}/audio-stems/u/s/vocals.ogg`, drums_url: `${SB}/audio-stems/u/s/drums.wav`,
  }]
  const posts = [{ id: 'p', images: [`${SB}/post-images/u/1700000000000-a1.png`, 'https://keep.example/x.png'] as string[] | null }]
  const updates: string[] = []
  const recheckCalls: string[] = []
  const hooks: { beforeRecheck?: (bucket: LogicalBucket, key: string) => void } = {}
  const db: MigrationDb = {
    listStemRows: async () => stemRows.map(r => ({ ...r })),
    async updateStemRow(id, before, update) {
      const r = stemRows.find(x => x.id === id)
      if (!r || Object.entries(before).some(([c, v]) => (r[c] ?? null) !== v)) return 0
      Object.assign(r, update)
      updates.push(`stem:${id}`)
      return 1
    },
    listPosts: async () => posts.map(p => ({ ...p, images: p.images && [...p.images] })),
    async updatePostImages(id, before, images) {
      const p = posts.find(x => x.id === id)
      if (!p || JSON.stringify(p.images) !== JSON.stringify(before)) return 0
      p.images = images
      updates.push(`post:${id}`)
      return 1
    },
    async isSourceKeyReferenced(bucket, key) {
      hooks.beforeRecheck?.(bucket, key)
      recheckCalls.push(`${bucket}:${key}`)
      const urls = bucket === 'audio-stems'
        ? stemRows.flatMap(r => Object.entries(r).filter(([c, v]) => c.endsWith('_url') && typeof v === 'string').map(([, v]) => v as string))
        : posts.flatMap(p => p.images ?? [])
      return urls.some(u => supabaseKeyFromAnyUrl(u, bucket) === key)
    },
  }
  const deps = {
    source: (b: LogicalBucket) => src[b].adapter,
    target: (b: LogicalBucket) => dst[b].adapter,
    db,
    imageViewUrl: (k: string) => `https://api.example/api/storage/post-images/${k}`,
    log: () => {},
  }
  return { src, dst, stemRows, posts, updates, deps, recheckCalls, hooks }
}

describe('migrate()', () => {
  it('dry run changes nothing but reports what it would do', async () => {
    const w = world()
    const reports = await migrate(parseArgs([]), w.deps)
    assert.deepEqual(reports.map(r => [r.bucket, r.objects, r.toCopy, r.dbPlanned, r.copied]), [
      ['audio-stems', 2, 2, 1, 0],
      ['post-images', 1, 1, 1, 0],
    ])
    assert.equal(w.dst['audio-stems'].objects.size + w.dst['post-images'].objects.size, 0)
    assert.deepEqual(w.updates, [])
    assert.equal(w.src['audio-stems'].objects.size, 2)
  })

  it('--apply copies, verifies size, rewrites DB URLs, keeps the source', async () => {
    const w = world()
    const reports = await migrate(parseArgs(['--apply']), w.deps)
    assert.equal(w.dst['audio-stems'].objects.get('u/s/vocals.ogg')!.body.toString(), 'vvvv')
    assert.equal(w.dst['audio-stems'].objects.get('u/s/vocals.ogg')!.contentType, 'audio/ogg')
    assert.equal(w.stemRows[0].vocals_url, `${STEM_REF_PREFIX}u/s/vocals.ogg`)
    assert.equal(w.stemRows[0].drums_url, `${STEM_REF_PREFIX}u/s/drums.wav`)
    assert.deepEqual(w.posts[0].images, ['https://api.example/api/storage/post-images/u/1700000000000-a1.png', 'https://keep.example/x.png'])
    assert.equal(w.src['audio-stems'].objects.size, 2, 'source untouched without --delete-source')
    assert.ok(reports.every(r => r.failed.length === 0 && r.deleted === 0))

    // 재실행: 이미 있음 → 복사 0, DB 갱신 0 (멱등)
    const again = await migrate(parseArgs(['--apply']), w.deps)
    assert.deepEqual(again.map(r => [r.toCopy, r.dbPlanned]), [[0, 0], [0, 0]])
  })

  it('a failed copy (size mismatch) is not verified: its DB URL and source stay', async () => {
    const w = world()
    const target = w.dst['audio-stems'].adapter
    const realPut = target.put.bind(target)
    target.put = async (k, b, o) => realPut(k, k.endsWith('drums.wav') ? Buffer.from('X') : b, o)
    const [stems] = await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'audio-stems']), w.deps)
    assert.deepEqual(stems.failed.map(f => f.key), ['u/s/drums.wav'])
    assert.equal(w.stemRows[0].drums_url, `${SB}/audio-stems/u/s/drums.wav`)
    assert.equal(w.stemRows[0].vocals_url, `${STEM_REF_PREFIX}u/s/vocals.ogg`)
    // 검증된 vocals만 원본 삭제
    assert.deepEqual([...w.src['audio-stems'].objects.keys()], ['u/s/drums.wav'])
  })

  it('--delete-source is skipped for a bucket whose DB rewrite failed', async () => {
    const w = world()
    w.deps.db.updatePostImages = async () => { throw new Error('db down') }
    const reports = await migrate(parseArgs(['--apply', '--delete-source']), w.deps)
    const img = reports.find(r => r.bucket === 'post-images')!
    assert.equal(img.dbFailed, 1)
    assert.equal(img.deleted, 0)
    assert.equal(w.src['post-images'].objects.size, 1)
    assert.equal(w.src['audio-stems'].objects.size, 0, 'stems bucket was fine → deleted')
  })

  it('H2: --delete-source keeps every source object the DB still points at (0-row race, unparsable refs, new refs)', async () => {
    const w = world()
    // 해석 못 하는 참조: 폴더 밖·다른 확장자 (rewriteStemRow가 바꾸지 못함)
    w.src['audio-stems'].objects.set('u/s/vocals.opus', { body: Buffer.from('oo') })
    w.stemRows.push({ id: 't', storage_folder: 'u/OTHER', other_url: `${SB}/audio-stems/u/s/vocals.opus` })
    // 경합: 갱신 직전에 사용자가 글을 고쳐 조건부 갱신이 0행
    const realUpdate = w.deps.db.updatePostImages
    w.deps.db.updatePostImages = async (id, before, images) => {
      w.posts[0].images = [...before, 'https://keep.example/new.png']
      return realUpdate(id, before, images)
    }
    const reports = await migrate(parseArgs(['--apply', '--delete-source']), w.deps)
    const stems = reports.find(r => r.bucket === 'audio-stems')!
    const imgs = reports.find(r => r.bucket === 'post-images')!

    assert.ok(w.src['audio-stems'].objects.has('u/s/vocals.opus'), 'unparsable ref keeps its source')
    assert.ok(!w.src['audio-stems'].objects.has('u/s/vocals.ogg'), 'rewritten stem source deleted')
    assert.equal(stems.keptReferenced, 1)
    assert.equal(imgs.dbSkipped, 1)
    assert.ok(w.src['post-images'].objects.has('u/1700000000000-a1.png'), 'post still points at Supabase → source kept')
    assert.equal(imgs.deleted, 0)
  })

  it('H2: deletion is skipped when the DB re-check fails', async () => {
    const w = world()
    let calls = 0
    const realList = w.deps.db.listStemRows
    w.deps.db.listStemRows = async () => {
      if (++calls > 1) throw new Error('db down')
      return realList()
    }
    const [stems] = await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'audio-stems']), w.deps)
    assert.equal(stems.deleted, 0)
    assert.match(stems.deleteSkippedReason ?? '', /재확인 실패/)
    assert.equal(w.src['audio-stems'].objects.size, 2)
  })

  it('M3: images the view route cannot serve stay on Supabase and are not deleted', async () => {
    const w = world()
    const bad = 'u/sub/1700000000000-x.png'
    w.src['post-images'].objects.set(bad, { body: Buffer.from('p') })
    w.posts.push({ id: 'q', images: [`${SB}/post-images/${bad}`] })
    const [imgs] = await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'post-images']), w.deps)
    assert.deepEqual(imgs.unroutable, [bad])
    assert.equal(w.posts[1].images![0], `${SB}/post-images/${bad}`)
    assert.ok(w.src['post-images'].objects.has(bad))
    assert.ok(!w.src['post-images'].objects.has('u/1700000000000-a1.png'), 'the routable, rewritten one is deleted')
  })

  it('M1: a stale edit form re-saves the old Supabase URL after its source was deleted → re-run maps it to the copy', async () => {
    const w = world()
    await migrate(parseArgs(['--apply', '--delete-source']), w.deps)
    assert.equal(w.src['post-images'].objects.size, 0, 'source deleted')
    // 오래된 편집 폼이 예전 URL을 다시 저장
    w.posts[0].images = [`${SB}/post-images/u/1700000000000-a1.png`]
    w.stemRows[0].vocals_url = `${SB}/audio-stems/u/s/vocals.ogg`
    const again = await migrate(parseArgs(['--apply']), w.deps)
    assert.deepEqual(w.posts[0].images, ['https://api.example/api/storage/post-images/u/1700000000000-a1.png'])
    assert.equal(w.stemRows[0].vocals_url, `${STEM_REF_PREFIX}u/s/vocals.ogg`)
    assert.ok(again.every(r => r.dbUpdated === 1))
  })

  it('M2: a reference that appears between the full re-scan and the delete is caught by the per-key recheck', async () => {
    const w = world()
    // 전체 재확인이 끝난 뒤, 그 key를 지우기 직전에 다른 게시글이 예전 URL을 저장
    w.hooks.beforeRecheck = (bucket, key) => {
      if (bucket === 'post-images' && key === 'u/1700000000000-a1.png') w.posts.push({ id: 'late', images: [`${SB}/post-images/${key}`] })
    }
    const reports = await migrate(parseArgs(['--apply', '--delete-source']), w.deps)
    const img = reports.find(r => r.bucket === 'post-images')!
    assert.ok(w.src['post-images'].objects.has('u/1700000000000-a1.png'))
    assert.equal(img.deleted, 0)
    assert.equal(img.keptReferenced, 1)
    assert.ok(w.recheckCalls.includes('post-images:u/1700000000000-a1.png'))
  })

  it('L1: signed/authenticated/render Supabase URL forms count as references', async () => {
    assert.equal(supabaseKeyFromAnyUrl('https://x.supabase.co/storage/v1/object/sign/post-images/u/a.png?token=t', 'post-images'), 'u/a.png')
    assert.equal(supabaseKeyFromAnyUrl('https://x.supabase.co/storage/v1/render/image/public/post-images/u/a.png?width=100', 'post-images'), 'u/a.png')
    assert.equal(supabaseKeyFromAnyUrl('https://x.supabase.co/storage/v1/object/authenticated/audio-stems/u/s/v.ogg', 'audio-stems'), 'u/s/v.ogg')
    assert.equal(supabaseKeyFromAnyUrl('https://x.supabase.co/storage/v1/object/public/audio-stems/u/s/v.ogg', 'post-images'), null)
    const w = world()
    w.posts[0].images = ['https://x.supabase.co/storage/v1/object/sign/post-images/u/1700000000000-a1.png?token=t']
    await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'post-images']), w.deps)
    assert.ok(w.src['post-images'].objects.has('u/1700000000000-a1.png'), 'signed-URL reference keeps the source')
  })

  it('without an image view URL, images are copied but posts are not rewritten nor sources deleted', async () => {
    const w = world()
    const reports = await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'post-images']), { ...w.deps, imageViewUrl: null })
    assert.equal(w.dst['post-images'].objects.size, 1)
    assert.equal(w.posts[0].images![0], `${SB}/post-images/u/1700000000000-a1.png`)
    assert.equal(reports[0].deleted, 0)
  })
})
