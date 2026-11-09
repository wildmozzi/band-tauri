/**
 * P3-6: 기존 WAV 스템 마이그레이션 스크립트의 계획/인자/DB 갱신 로직 (Supabase·ffmpeg 없이)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { planMigration, parseArgs, formatBytes, commitStemUrl, verifyUploaded, type MigrationItem } from '../scripts/migrate-stems-to-opus'
import { wavDurationSec } from '../src/services/stemEncoder'

const PUB = 'https://x.supabase.co/storage/v1/object/public/audio-stems'

describe('planMigration', () => {
  it('selects only .wav stems, keeps .ogg/null untouched, maps paths to .ogg', () => {
    const rows = [
      {
        id: 's1',
        storage_folder: 'u/s1',
        vocals_url: `${PUB}/u/s1/vocals.wav`,
        drums_url: `${PUB}/u/s1/drums.ogg`,
        bass_url: null,
        other_url: `${PUB}/u/s1/other.wav?v=2`,
      },
      { id: 's2', storage_folder: 'u/s2', piano_url: `${PUB}/u/s2/piano.ogg` },
    ]
    const sizes = new Map([['u/s1/vocals.wav', 1000]])
    const { items, leftovers, skipped } = planMigration(rows, sizes)
    assert.deepEqual(items.map(i => [i.separationId, i.stem, i.column, i.wavUrl, i.wavPath, i.oggPath, i.wavBytes]), [
      ['s1', 'vocals', 'vocals_url', `${PUB}/u/s1/vocals.wav`, 'u/s1/vocals.wav', 'u/s1/vocals.ogg', 1000],
      ['s1', 'other', 'other_url', `${PUB}/u/s1/other.wav?v=2`, 'u/s1/other.wav', 'u/s1/other.ogg', null],
    ])
    assert.deepEqual(leftovers, [])
    assert.deepEqual(skipped, [])
  })

  it('skips (does not guess) when the URL is not a Supabase public URL or points outside the folder', () => {
    const { items, skipped } = planMigration([
      { id: 's', storage_folder: 'f', guitar_url: 'https://cdn.example/guitar.wav', bass_url: `${PUB}/other-user/x/bass.wav` },
    ])
    assert.deepEqual(items, [])
    assert.deepEqual(skipped.map(s => s.stem).sort(), ['bass', 'guitar'])
  })

  it('reports leftover .wav when DB already points at .ogg (interrupted previous run)', () => {
    const rows = [{ id: 's1', storage_folder: 'u/s1', vocals_url: `${PUB}/u/s1/vocals.ogg`, drums_url: `${PUB}/u/s1/drums.ogg` }]
    const sizes = new Map([['u/s1/vocals.wav', 5000], ['u/s1/vocals.ogg', 600], ['u/s1/drums.ogg', 500]])
    const { items, leftovers } = planMigration(rows, sizes)
    assert.deepEqual(items, [])
    assert.deepEqual(leftovers, [{ separationId: 's1', wavPath: 'u/s1/vocals.wav', bytes: 5000 }])
  })
})

/** commitStemUrl/verifyUploaded용 최소 Supabase 가짜 (쿼리 체인을 기록) */
function fakeDb(opts: { updatedRows?: { id: string }[]; updateError?: { message: string } | null; listed?: { name: string; metadata: { size: number } | null }[] }) {
  const calls: { eqs: [string, string][]; update?: Record<string, string>; removed: string[][] } = { eqs: [], removed: [] }
  const chain: any = {
    update(v: Record<string, string>) { calls.update = v; return chain },
    eq(c: string, v: string) { calls.eqs.push([c, v]); return chain },
    select: async () => ({ data: opts.updateError ? null : (opts.updatedRows ?? []), error: opts.updateError ?? null }),
  }
  const db: any = {
    from: () => chain,
    storage: {
      from: () => ({
        remove: async (p: string[]) => { calls.removed.push(p); return { data: [], error: null } },
        list: async () => ({ data: opts.listed ?? [], error: null }),
      }),
    },
  }
  return { db, calls }
}

const ITEM: MigrationItem = {
  separationId: 's1',
  folder: 'u/s1',
  stem: 'vocals',
  column: 'vocals_url',
  wavUrl: `${PUB}/u/s1/vocals.wav`,
  wavPath: 'u/s1/vocals.wav',
  oggPath: 'u/s1/vocals.ogg',
  wavBytes: 1000,
}

describe('commitStemUrl (conditional DB update)', () => {
  it('updates only while the column still holds the original .wav URL', async () => {
    const f = fakeDb({ updatedRows: [{ id: 's1' }] })
    await commitStemUrl(f.db, ITEM, `${PUB}/u/s1/vocals.ogg`)
    assert.deepEqual(f.calls.update, { vocals_url: `${PUB}/u/s1/vocals.ogg` })
    assert.deepEqual(f.calls.eqs, [['id', 's1'], ['vocals_url', ITEM.wavUrl]])
    assert.deepEqual(f.calls.removed, [], 'uploaded ogg kept on success')
  })

  it('0 rows updated (record deleted/changed meanwhile) → removes uploaded ogg and throws, wav untouched', async () => {
    const f = fakeDb({ updatedRows: [] })
    await assert.rejects(commitStemUrl(f.db, ITEM, 'x'), /갱신된 행이 없음/)
    assert.deepEqual(f.calls.removed, [['u/s1/vocals.ogg']])
  })

  it('DB error → removes uploaded ogg and throws', async () => {
    const f = fakeDb({ updateError: { message: 'boom' } })
    await assert.rejects(commitStemUrl(f.db, ITEM, 'x'), /DB 갱신 실패: boom/)
    assert.deepEqual(f.calls.removed, [['u/s1/vocals.ogg']])
  })
})

describe('verifyUploaded', () => {
  it('passes when remote size matches', async () => {
    const f = fakeDb({ listed: [{ name: 'vocals.ogg', metadata: { size: 123 } }] })
    await verifyUploaded(f.db, 'u/s1/vocals.ogg', 123)
  })
  it('fails on size mismatch or missing object', async () => {
    await assert.rejects(verifyUploaded(fakeDb({ listed: [{ name: 'vocals.ogg', metadata: { size: 1 } }] }).db, 'u/s1/vocals.ogg', 123), /원격 크기 1/)
    await assert.rejects(verifyUploaded(fakeDb({ listed: [] }).db, 'u/s1/vocals.ogg', 123), /원격 크기 없음/)
  })
})

describe('parseArgs', () => {
  it('defaults to dry-run', () => {
    assert.deepEqual(parseArgs([]), { apply: false, limit: null, includeExpired: false })
  })
  it('parses --apply --limit --include-expired', () => {
    assert.deepEqual(parseArgs(['--apply', '--limit', '3', '--include-expired']), { apply: true, limit: 3, includeExpired: true })
  })
  it('rejects unknown flags and bad limits', () => {
    assert.throws(() => parseArgs(['--aply']))
    assert.throws(() => parseArgs(['--limit', '0']))
  })
})

describe('helpers', () => {
  it('formatBytes', () => {
    assert.equal(formatBytes(512), '512 B')
    assert.equal(formatBytes(31711310), '30.2 MB')
  })
  it('wavDurationSec skips extra chunks', () => {
    const fmt = Buffer.alloc(24)
    fmt.write('fmt ', 0, 'latin1'); fmt.writeUInt32LE(16, 4); fmt.writeUInt16LE(1, 8); fmt.writeUInt16LE(2, 10)
    fmt.writeUInt32LE(44100, 12); fmt.writeUInt32LE(44100 * 4, 16); fmt.writeUInt16LE(4, 20); fmt.writeUInt16LE(16, 22)
    const list = Buffer.concat([Buffer.from('LIST', 'latin1'), Buffer.from([3, 0, 0, 0]), Buffer.from('abc\0', 'latin1')]) // 홀수 크기 + 패딩
    const dataHdr = Buffer.alloc(8); dataHdr.write('data', 0, 'latin1'); dataHdr.writeUInt32LE(44100 * 4 * 2, 4)
    const body = Buffer.concat([Buffer.from('WAVE', 'latin1'), fmt, list, dataHdr, Buffer.alloc(44100 * 4 * 2)])
    const riff = Buffer.alloc(8); riff.write('RIFF', 0, 'latin1'); riff.writeUInt32LE(body.length, 4)
    assert.equal(wavDurationSec(Buffer.concat([riff, body])), 2)
    assert.throws(() => wavDurationSec(Buffer.from('nope nope nope')))
  })
})
