/**
 * P3-10: Supabase → S3 이전 스크립트 (메모리 저장소·메모리 DB로, 실제 실행 없이)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import {
  migrate, parseArgs, planCopy, rewritePostImages, rewriteStemRow, supabaseKeyFromUrl, contentTypeOf,
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
    assert.deepEqual(rewritePostImages([`${SB}/post-images/u/1.png`, 'https://other/x.png', `${SB}/post-images/u/2.png`], new Set(['u/1.png']), view),
      ['https://api/i/u/1.png', 'https://other/x.png', `${SB}/post-images/u/2.png`])
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
  src['post-images'].objects.set('u/1.png', { body: Buffer.from('png1') })
  const stemRows: StemRowForMigration[] = [{
    id: 's', storage_folder: 'u/s',
    vocals_url: `${SB}/audio-stems/u/s/vocals.ogg`, drums_url: `${SB}/audio-stems/u/s/drums.wav`,
  }]
  const posts = [{ id: 'p', images: [`${SB}/post-images/u/1.png`, 'https://keep.example/x.png'] as string[] | null }]
  const updates: string[] = []
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
  }
  const deps = {
    source: (b: LogicalBucket) => src[b].adapter,
    target: (b: LogicalBucket) => dst[b].adapter,
    db,
    imageViewUrl: (k: string) => `https://api.example/api/storage/post-images/${k}`,
    log: () => {},
  }
  return { src, dst, stemRows, posts, updates, deps }
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
    assert.deepEqual(w.posts[0].images, ['https://api.example/api/storage/post-images/u/1.png', 'https://keep.example/x.png'])
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

  it('without an image view URL, images are copied but posts are not rewritten nor sources deleted', async () => {
    const w = world()
    const reports = await migrate(parseArgs(['--apply', '--delete-source', '--bucket', 'post-images']), { ...w.deps, imageViewUrl: null })
    assert.equal(w.dst['post-images'].objects.size, 1)
    assert.equal(w.posts[0].images![0], `${SB}/post-images/u/1.png`)
    assert.equal(reports[0].deleted, 0)
  })
})
