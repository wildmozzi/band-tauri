/**
 * P3-6: 기존 WAV 스템 마이그레이션 스크립트의 계획/인자 로직 (Supabase·ffmpeg 없이)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { planMigration, parseArgs, formatBytes } from '../scripts/migrate-stems-to-opus'
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
    const items = planMigration(rows, sizes)
    assert.deepEqual(items.map(i => [i.separationId, i.stem, i.column, i.wavPath, i.oggPath, i.wavBytes]), [
      ['s1', 'vocals', 'vocals_url', 'u/s1/vocals.wav', 'u/s1/vocals.ogg', 1000],
      ['s1', 'other', 'other_url', 'u/s1/other.wav', 'u/s1/other.ogg', null],
    ])
  })

  it('falls back to storage_folder when the URL is not a Supabase public URL', () => {
    const items = planMigration([{ id: 's', storage_folder: 'f', guitar_url: 'https://cdn.example/guitar.wav' }])
    assert.equal(items[0].wavPath, 'f/guitar.wav')
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
