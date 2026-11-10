/**
 * P3-10: 스템 업로드·다운로드(서명 URL)·삭제·만료 정리가 StorageAdapter를 거치는지
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import * as fs from 'fs'
import * as os from 'os'
import * as path from 'path'
import { StemSeparationService, stemUrlTtlSec, type StemRepos } from '../src/services/stemSeparationService'
import { STEM_REF_PREFIX, stemKeyFromRef } from '../src/lib/stemStorage'
import type { StemSeparationRow } from '../src/repositories/stemSeparationRepository'
import { memoryStorage } from './fakes/memoryStorage'

process.env.STEM_FORMAT = 'wav' // ffmpeg 없이 (인코딩 생략)

const SB = 'https://x.supabase.co/storage/v1/object/public/audio-stems'

function row(over: Partial<StemSeparationRow> = {}): StemSeparationRow {
  return {
    id: 'sep1', user_id: 'u1', original_filename: 'a.mp3', original_file_size: 1, storage_folder: 'u1/sep1',
    vocals_url: null, drums_url: null, bass_url: null, piano_url: null, guitar_url: null, other_url: null,
    status: 'completed', ...over,
  }
}

function setup(opts: {
  rows?: StemSeparationRow[]
  expired?: { id: string; storage_folder: string | null }[]
  publicBase?: string
  failDelete?: boolean
  legacy?: ReturnType<typeof memoryStorage>
} = {}) {
  const mem = memoryStorage('audio-stems', { publicBase: opts.publicBase, failDelete: opts.failDelete })
  const inserted: unknown[] = []
  const deletedIds: string[][] = []
  const repos: StemRepos = {
    separations: {
      findActiveFilenamesByPrefix: async () => [],
      insert: async r => { inserted.push(r) },
      listCompletedByUser: async () => opts.rows ?? [],
      findById: async id => (opts.rows ?? []).find(r => r.id === id) ?? null,
      deleteById: async id => { deletedIds.push([id]) },
      listExpired: async () => opts.expired ?? [],
      deleteByIds: async ids => { deletedIds.push(ids) },
    },
    storage: mem.adapter,
    legacy: opts.legacy?.adapter ?? null,
  }
  return { svc: new StemSeparationService(repos, () => new Date(), 3600), mem, inserted, deletedIds }
}

describe('stemKeyFromRef', () => {
  it('accepts storage refs, Supabase public URLs and S3 public URLs inside the folder', () => {
    assert.equal(stemKeyFromRef(`${STEM_REF_PREFIX}u1/sep1/vocals.ogg`, 'u1/sep1'), 'u1/sep1/vocals.ogg')
    assert.equal(stemKeyFromRef(`${SB}/u1/sep1/drums.wav?v=1`, 'u1/sep1'), 'u1/sep1/drums.wav')
    assert.equal(stemKeyFromRef('https://cdn.example/audio-stems/u1/sep1/bass.ogg', 'u1/sep1'), 'u1/sep1/bass.ogg')
  })
  it('refuses refs outside the record folder or with unknown names (no signing of other users\' files)', () => {
    for (const bad of [
      `${STEM_REF_PREFIX}u2/sep9/vocals.ogg`,
      `${STEM_REF_PREFIX}u1/sep1/../../u2/x/vocals.ogg`,
      `${STEM_REF_PREFIX}u1/sep1/sub/vocals.ogg`,
      `${STEM_REF_PREFIX}u1/sep1/secret.txt`,
      `${SB}/u1/sep1%2F..%2F..%2Fu2/vocals.ogg`,
      'https://cdn.example/other/u1/sep1/vocals.ogg',
      'javascript:alert(1)',
      'u1/sep1/vocals.ogg',
    ]) {
      assert.equal(stemKeyFromRef(bad, 'u1/sep1'), null, bad)
    }
  })
})

describe('StemSeparationService + StorageAdapter', () => {
  it('uploadStems puts files with cache headers and returns storage refs (no public URL on private S3)', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'band-stems-'))
    const vocals = path.join(dir, 'vocals.wav')
    fs.writeFileSync(vocals, Buffer.from('RIFFfake'))
    const { svc, mem } = setup()
    const refs = await svc.uploadStems({ vocals, drums: path.join(dir, 'missing.wav') }, 'u1', 'sep1')
    assert.deepEqual(refs, { vocals: `${STEM_REF_PREFIX}u1/sep1/vocals.wav` })
    assert.equal(mem.objects.get('u1/sep1/vocals.wav')?.contentType, 'audio/wav')
    assert.equal(fs.existsSync(vocals), false, 'local file removed')
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('uploadStems stores the public URL when the store has one (Supabase: same as before)', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'band-stems-'))
    const f = path.join(dir, 'bass.wav')
    fs.writeFileSync(f, 'x')
    const { svc } = setup({ publicBase: 'https://pub.example' })
    assert.deepEqual(await svc.uploadStems({ bass: f }, 'u1', 'sep1'), { bass: 'https://pub.example/audio-stems/u1/sep1/bass.wav' })
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('signStems / listHistory sign storage:// refs with the current store and expiry', async () => {
    const { svc, mem } = setup({
      rows: [row({
        vocals_url: `${STEM_REF_PREFIX}u1/sep1/vocals.ogg`,
        bass_url: 'https://elsewhere.example/bass.wav', // 알 수 없는 외부 URL: 그대로 (하위 호환)
        piano_url: `${STEM_REF_PREFIX}u2/other/piano.ogg`, // 폴더 밖: 서명 안 함 → null
      })],
    })
    const [h] = await svc.listHistory('u1')
    assert.equal(h.vocals_url, 'https://signed.test/audio-stems/u1/sep1/vocals.ogg?ttl=3600')
    assert.equal(h.bass_url, 'https://elsewhere.example/bass.wav')
    assert.equal(h.piano_url, null)
    assert.equal(h.guitar_url, null)
    assert.deepEqual(mem.calls.filter(c => c.op === 'signGet').map(c => (c.arg as string[])[0]), ['u1/sep1/vocals.ogg'])

    assert.deepEqual(await svc.signStems({ other: `${STEM_REF_PREFIX}u1/sep1/other.ogg` }, 'u1/sep1'), {
      other: 'https://signed.test/audio-stems/u1/sep1/other.ogg?ttl=3600',
    })
  })

  it('H1: with STORAGE_DRIVER=s3, not-yet-migrated Supabase URLs are read from Supabase, never signed as R2 keys', async () => {
    // 현재 저장소 = 비공개 S3(공개 URL 없음), legacy = Supabase 공개 버킷
    const legacy = memoryStorage('audio-stems', { driver: 'supabase', publicBase: 'https://x.supabase.co/storage/v1/object/public' })
    const { svc, mem } = setup({
      legacy,
      rows: [row({ drums_url: `${SB}/u1/sep1/drums.wav`, vocals_url: `${STEM_REF_PREFIX}u1/sep1/vocals.ogg` })],
    })
    const [h] = await svc.listHistory('u1')
    assert.equal(h.drums_url, `${SB}/u1/sep1/drums.wav`, 'Supabase public URL unchanged (cacheable, served by Supabase)')
    assert.equal(h.vocals_url, 'https://signed.test/audio-stems/u1/sep1/vocals.ogg?ttl=3600', 'migrated ref signed by R2')
    assert.deepEqual(mem.calls.filter(c => c.op === 'signGet').map(c => (c.arg as string[])[0]), ['u1/sep1/vocals.ogg'])
    assert.equal(legacy.calls.filter(c => c.op === 'signGet').length, 0)
  })

  it('H1: Supabase URLs are signed by Supabase (not R2) when the legacy bucket is private', async () => {
    const legacy = memoryStorage('audio-stems', { driver: 'supabase' }) // 공개 URL 없음 = 비공개
    const { svc, mem } = setup({ legacy })
    assert.equal(await svc.signStemRef(`${SB}/u1/sep1/drums.wav`, 'u1/sep1'), 'https://signed.test/audio-stems/u1/sep1/drums.wav?ttl=3600')
    assert.equal(legacy.calls.filter(c => c.op === 'signGet').length, 1)
    assert.equal(mem.calls.filter(c => c.op === 'signGet').length, 0)
    // 폴더 밖 Supabase URL은 서명하지 않고 그대로 (기존 공개 URL 동작)
    assert.equal(await svc.signStemRef(`${SB}/u2/x/drums.wav`, 'u1/sep1'), `${SB}/u2/x/drums.wav`)
    assert.equal(legacy.calls.filter(c => c.op === 'signGet').length, 1)
  })

  it('M1: on the supabase driver (public bucket) stems keep their stable public URL — no per-request signing', async () => {
    const pub = 'https://x.supabase.co/storage/v1/object/public'
    const { svc, mem } = setup({ publicBase: pub, legacy: memoryStorage('audio-stems', { driver: 'supabase', publicBase: pub }) })
    const url = `${SB}/u1/sep1/vocals.ogg`
    assert.equal(await svc.signStemRef(url, 'u1/sep1'), url)
    assert.equal(await svc.signStemRef(url, 'u1/sep1'), url)
    assert.equal(mem.calls.filter(c => c.op === 'signGet').length, 0)
  })

  it('signing failure of a storage:// ref yields null (never the raw ref)', async () => {
    const { svc, mem } = setup()
    mem.adapter.signedGetUrl = async () => { throw new Error('down') }
    assert.equal(await svc.signStemRef(`${STEM_REF_PREFIX}u1/sep1/vocals.ogg`, 'u1/sep1'), null)
    assert.equal(await svc.signStemRef('ftp://x/vocals.ogg', 'u1/sep1'), null)
  })

  it('deleteSeparation deletes .ogg/.wav keys through the adapter', async () => {
    const { svc, mem, deletedIds } = setup({ rows: [row()] })
    await mem.adapter.put('u1/sep1/vocals.ogg', Buffer.from('a'))
    await mem.adapter.put('u1/sep1/drums.wav', Buffer.from('b'))
    await svc.deleteSeparation('u1', 'sep1')
    assert.equal(mem.objects.size, 0)
    assert.deepEqual(deletedIds, [['sep1']])
  })

  it('cleanupExpired removes files then rows, and keeps rows whose files failed to delete', async () => {
    const ok = setup({ expired: [{ id: 'a', storage_folder: 'u1/a' }, { id: 'b', storage_folder: null }] })
    await ok.mem.adapter.put('u1/a/vocals.ogg', Buffer.from('x'))
    assert.equal(await ok.svc.cleanupExpired(), 2)
    assert.equal(ok.mem.objects.size, 0)
    assert.deepEqual(ok.deletedIds, [['a', 'b']])

    const failing = setup({ expired: [{ id: 'a', storage_folder: 'u1/a' }], failDelete: true })
    assert.equal(await failing.svc.cleanupExpired(), 0)
    assert.deepEqual(failing.deletedIds, [])
  })

  it('stemUrlTtlSec: default 12h, floor 5min, cap 7d', () => {
    assert.equal(stemUrlTtlSec({}), 43200)
    assert.equal(stemUrlTtlSec({ STEM_URL_TTL_SEC: '60' }), 43200)
    assert.equal(stemUrlTtlSec({ STEM_URL_TTL_SEC: '7200' }), 7200)
    assert.equal(stemUrlTtlSec({ STEM_URL_TTL_SEC: String(30 * 86400) }), 7 * 86400)
  })
})
