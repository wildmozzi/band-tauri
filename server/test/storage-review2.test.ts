/**
 * P3-10 리뷰2: keyset 페이지네이션(M2), 고아 정리 key 단위 재확인·pending 정리(M2·M3), 스템 삭제 시 Supabase 원본(L2)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { anyPostReferences, listAllPostImageUrls } from '../src/repositories/postImageRepository'
import { supabaseMigrationDb } from '../scripts/migrate-storage'
import { cleanupOrphanPostImages } from '../src/services/postImageService'
import { postImageUrlCandidates } from '../src/services/cleanupService'
import { StemSeparationService } from '../src/services/stemSeparationService'
import { memoryStorage } from './fakes/memoryStorage'

/** posts / stem_separations 최소 PostgREST 가짜: select·order·limit·gt·range·overlaps. onPage로 페이지 사이 행 삭제를 흉내 */
function fakePostgrest(tables: Record<string, Record<string, unknown>[]>, onPage?: (table: string, page: number) => void) {
  let pages = 0
  const from = (table: string) => {
    const st: { gt?: string; limit?: number; range?: [number, number]; ov?: string[] } = {}
    const chain: any = {
      select: () => chain,
      order: () => chain,
      limit: (n: number) => { st.limit = n; return chain },
      gt: (_c: string, v: string) => { st.gt = v; return chain },
      range: (a: number, b: number) => { st.range = [a, b]; return chain },
      overlaps: (_c: string, v: string[]) => { st.ov = v; return chain },
      then(res: (v: unknown) => unknown, rej: (e: unknown) => unknown) {
        let rows = [...tables[table]].sort((a, b) => String(a.id).localeCompare(String(b.id)))
        if (st.ov) rows = rows.filter(r => ((r.images as string[] | null) ?? []).some(u => st.ov!.includes(u)))
        if (st.gt !== undefined) rows = rows.filter(r => String(r.id) > st.gt!)
        if (st.range) rows = rows.slice(st.range[0], st.range[1] + 1)
        if (st.limit !== undefined) rows = rows.slice(0, st.limit)
        const out = { data: rows, error: null }
        onPage?.(table, ++pages)
        return Promise.resolve(out).then(res, rej)
      },
    }
    return chain
  }
  return { from } as any
}

const pad = (i: number) => String(i).padStart(5, '0')

describe('M2: keyset pagination does not skip rows when earlier rows are deleted mid-scan', () => {
  const rows = () => Array.from({ length: 2500 }, (_, i) => ({ id: `p${pad(i)}`, images: [`https://cdn/post-images/u/${pad(i)}.png`] }))

  it('listAllPostImageUrls (orphan cleanup)', async () => {
    const tables = { posts: rows() }
    // 첫 페이지를 읽은 직후 첫 페이지 범위의 행 10개 삭제 (offset이면 1000~1009번째를 건너뛴다)
    const sb = fakePostgrest(tables, (_t, page) => { if (page === 1) tables.posts.splice(0, 10) })
    const urls = await listAllPostImageUrls(sb)
    for (let i = 10; i < 2500; i++) assert.ok(urls.includes(`https://cdn/post-images/u/${pad(i)}.png`), `row ${i} missing`)
  })

  it('migration listPosts / listStemRows', async () => {
    const tables = {
      posts: rows(),
      stem_separations: Array.from({ length: 1500 }, (_, i) => ({ id: `s${pad(i)}`, storage_folder: 'f', vocals_url: `v${i}` })),
    }
    const sb = fakePostgrest(tables, (t, page) => {
      if (page === 1 && t === 'posts') tables.posts.splice(0, 5)
    })
    const db = supabaseMigrationDb(sb, 'https://x.supabase.co')
    // 지워진 5행은 이미 첫 페이지에서 읽혔고, 뒤 행(1000~1004번째)도 건너뛰지 않는다 → 2500개 모두
    const ids = new Set((await db.listPosts()).map(p => p.id))
    for (let i = 0; i < 2500; i++) assert.ok(ids.has(`p${pad(i)}`), `row ${i} skipped`)
    assert.equal((await db.listStemRows()).length, 1500)
  })

  it('anyPostReferences matches exact candidate URLs', async () => {
    const sb = fakePostgrest({ posts: [{ id: 'a', images: ['https://cdn/x.png'] }] })
    assert.equal(await anyPostReferences(sb, ['https://cdn/x.png', 'other']), true)
    assert.equal(await anyPostReferences(sb, ['nope']), false)
    assert.equal(await anyPostReferences(sb, []), false)
  })
})

describe('orphan cleanup: per-key recheck and pending uploads', () => {
  const UID = 'u1'
  const DAY = 24 * 3600 * 1000
  const now = 1700000000000 + 3 * DAY
  const k = (n: number) => `${UID}/${1700000000000 + n}-k${n}.png`

  it('re-checks each key right before deleting; a reference found then (or a failed check) keeps the object', async () => {
    const m = memoryStorage('post-images')
    for (const n of [1, 2, 3]) await m.adapter.put(k(n), Buffer.from('x'))
    const r = await cleanupOrphanPostImages(m.adapter, async () => [], {
      apply: true,
      now,
      isReferenced: async key => {
        if (key === k(2)) return true // 목록 조회 뒤 새로 참조됨
        if (key === k(3)) throw new Error('db down')
        return false
      },
    })
    assert.equal(r.deleted, 1)
    assert.deepEqual([...m.objects.keys()].sort(), [k(2), k(3)])
  })

  it('pending (unverified) uploads are removed after the grace period even if a post references them (M3)', async () => {
    const m = memoryStorage('post-images')
    const oldPending = `pending/${UID}/${now - 4 * 3600 * 1000}-a.png`
    const freshPending = `pending/${UID}/${now - 60 * 1000}-b.png`
    await m.adapter.put(oldPending, Buffer.from('x'))
    await m.adapter.put(freshPending, Buffer.from('x'))
    await m.adapter.put(k(1), Buffer.from('x'))
    const r = await cleanupOrphanPostImages(m.adapter, async () => [`https://x.supabase.co/storage/v1/object/public/post-images/${oldPending}`], {
      apply: true, now, pendingOnly: true,
    })
    assert.equal(r.deleted, 1)
    assert.ok(!m.objects.has(oldPending))
    assert.ok(m.objects.has(freshPending))
    assert.ok(m.objects.has(k(1)), 'pendingOnly never touches final keys')
  })

  it('postImageUrlCandidates covers view, route, API and Supabase forms', () => {
    const m = memoryStorage('post-images')
    const c = postImageUrlCandidates(m.adapter, 'u1/1700000000000-a b.png', { PUBLIC_API_URL: 'https://api.example/', SUPABASE_URL: 'https://x.supabase.co' })
    for (const want of [
      '/api/storage/post-images/u1/1700000000000-a%20b.png',
      'https://api.example/api/storage/post-images/u1/1700000000000-a%20b.png',
      'https://x.supabase.co/storage/v1/object/public/post-images/u1/1700000000000-a b.png',
      'https://x.supabase.co/storage/v1/object/public/post-images/u1/1700000000000-a%20b.png',
    ]) assert.ok(c.includes(want), want)
  })
})

describe('L2: deleting/expiring a stem record on the s3 driver also removes the Supabase original', () => {
  const row = {
    id: 'sep1', user_id: 'u1', original_filename: 'a.mp3', original_file_size: 1, storage_folder: 'u1/sep1',
    vocals_url: 'https://x.supabase.co/storage/v1/object/public/audio-stems/u1/sep1/vocals.wav', drums_url: null, bass_url: null,
    piano_url: null, guitar_url: null, other_url: null, status: 'completed',
  }
  const separations = (expired: { id: string; storage_folder: string | null }[] = []) => ({
    findActiveFilenamesByPrefix: async () => [], insert: async () => {}, listCompletedByUser: async () => [],
    findById: async () => row, deleteById: async () => {}, listExpired: async () => expired, deleteByIds: async () => {},
  })

  it('delete: both stores (s3 primary + Supabase legacy); legacy failure does not block', async () => {
    const primary = memoryStorage('audio-stems')
    const legacy = memoryStorage('audio-stems', { driver: 'supabase' })
    await legacy.adapter.put('u1/sep1/vocals.wav', Buffer.from('x'))
    await new StemSeparationService({ separations: separations(), storage: primary.adapter, legacy: legacy.adapter }).deleteSeparation('u1', 'sep1')
    assert.equal(legacy.objects.size, 0)

    const failing = memoryStorage('audio-stems', { driver: 'supabase', failDelete: true })
    await new StemSeparationService({ separations: separations(), storage: primary.adapter, legacy: failing.adapter }).deleteSeparation('u1', 'sep1')
  })

  it('expiry cleanup also removes Supabase originals; on the supabase driver legacy is not touched', async () => {
    const primary = memoryStorage('audio-stems')
    const legacy = memoryStorage('audio-stems', { driver: 'supabase' })
    await legacy.adapter.put('u1/old/vocals.wav', Buffer.from('x'))
    await new StemSeparationService({ separations: separations([{ id: 'old', storage_folder: 'u1/old' }]), storage: primary.adapter, legacy: legacy.adapter }).cleanupExpired()
    assert.equal(legacy.objects.size, 0)

    const sbPrimary = memoryStorage('audio-stems', { driver: 'supabase' })
    const sbLegacy = memoryStorage('audio-stems', { driver: 'supabase' })
    await new StemSeparationService({ separations: separations(), storage: sbPrimary.adapter, legacy: sbLegacy.adapter }).deleteSeparation('u1', 'sep1')
    assert.equal(sbLegacy.calls.filter(c => c.op === 'delete').length, 0)
  })
})
