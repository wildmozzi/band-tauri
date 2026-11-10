/**
 * P3-6: 스템 WAV → Opus/Ogg 인코딩, WAV 폴백, Storage 경로/정리 로직
 *
 * 인코딩 테스트는 ffmpeg가 필요하다(FFMPEG_PATH 또는 PATH). 없으면 skip 하되 이유를 출력한다.
 * CI(ubuntu)는 워크플로에서 ffmpeg를 설치한다.
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'child_process'
import * as fs from 'fs'
import * as os from 'os'
import * as path from 'path'
import {
  encodeToOpus,
  prepareStemForUpload,
  oggOpusSampleCount,
  ffmpegPath,
} from '../src/services/stemEncoder'
import { stemStoragePaths, stemFormatOf, storagePathFromPublicUrl, STEM_NAMES } from '../src/lib/stemStorage'
import { cleanupExpiredSeparations } from '../src/services/cleanupService'

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'band-stem-'))
const hasFfmpeg = spawnSync(ffmpegPath(), ['-hide_banner', '-version'], { stdio: 'ignore' }).status === 0
if (!hasFfmpeg) console.warn(`[stem-encoding.test] ffmpeg 없음 (${ffmpegPath()}) — 인코딩 테스트 skip`)
if (!hasFfmpeg && process.env.STEM_TEST_REQUIRE_FFMPEG === '1') {
  // CI는 ffmpeg를 설치하므로 skip으로 조용히 넘어가지 않게 한다
  throw new Error(`STEM_TEST_REQUIRE_FFMPEG=1 인데 ffmpeg를 찾을 수 없음: ${ffmpegPath()}`)
}

/** 16bit PCM 스테레오 WAV 생성 (사인파 + 약한 노이즈 → 실제 음악처럼 압축률이 극단적이지 않게) */
function makeWav(file: string, sampleRate: number, frames: number) {
  const ch = 2
  const data = Buffer.alloc(frames * ch * 2)
  let seed = 1
  for (let i = 0; i < frames; i++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff
    const noise = (seed / 0x7fffffff - 0.5) * 0.05
    const l = Math.sin((2 * Math.PI * 440 * i) / sampleRate) * 0.4 + noise
    const r = Math.sin((2 * Math.PI * 660 * i) / sampleRate) * 0.4 - noise
    data.writeInt16LE(Math.round(l * 32767), i * 4)
    data.writeInt16LE(Math.round(r * 32767), i * 4 + 2)
  }
  const h = Buffer.alloc(44)
  h.write('RIFF', 0, 'latin1'); h.writeUInt32LE(36 + data.length, 4); h.write('WAVE', 8, 'latin1')
  h.write('fmt ', 12, 'latin1'); h.writeUInt32LE(16, 16); h.writeUInt16LE(1, 20); h.writeUInt16LE(ch, 22)
  h.writeUInt32LE(sampleRate, 24); h.writeUInt32LE(sampleRate * ch * 2, 28); h.writeUInt16LE(ch * 2, 32); h.writeUInt16LE(16, 34)
  h.write('data', 36, 'latin1'); h.writeUInt32LE(data.length, 40)
  fs.writeFileSync(file, Buffer.concat([h, data]))
}

/** ffmpeg로 ogg를 다시 PCM으로 디코딩해 실제 디코딩 프레임 수(48kHz) 측정 — 컨테이너 메타가 아닌 실측 */
function decodedFrames(ogg: string): number {
  const r = spawnSync(ffmpegPath(), ['-hide_banner', '-loglevel', 'error', '-i', ogg, '-f', 's16le', '-ac', '2', '-ar', '48000', '-'], {
    maxBuffer: 64 * 1024 * 1024,
  })
  assert.equal(r.status, 0, r.stderr?.toString())
  return r.stdout.length / 4
}

after(() => fs.rmSync(dir, { recursive: true, force: true }))

describe('encodeToOpus (WAV → Ogg Opus)', { skip: !hasFfmpeg && 'ffmpeg not available' }, () => {
  // demucs 출력(44.1kHz)과 48kHz, 그리고 Opus 프레임(20ms) 경계에 맞지 않는 길이
  const cases = [
    { sr: 44100, frames: 44100 * 3 + 123 },
    { sr: 48000, frames: 48000 * 2 + 7 },
  ]
  for (const { sr, frames } of cases) {
    it(`preserves length within ±1 frame (${sr}Hz, ${frames} frames)`, async () => {
      const wav = path.join(dir, `in_${sr}.wav`)
      const ogg = path.join(dir, `out_${sr}.ogg`)
      makeWav(wav, sr, frames)
      await encodeToOpus(wav, ogg)

      const buf = fs.readFileSync(ogg)
      assert.equal(buf.toString('latin1', 0, 4), 'OggS')
      const meta = oggOpusSampleCount(buf)
      assert.equal(meta.channels, 2)

      const expected48k = (frames * 48000) / sr
      // 1) 컨테이너가 선언한 길이 (granule - pre-skip): 브라우저가 이 값으로 자른다
      assert.ok(Math.abs(meta.samples - expected48k) <= 1, `granule length ${meta.samples} vs expected ${expected48k}`)
      // 2) 실제 디코딩 결과 길이
      const dec = decodedFrames(ogg)
      assert.ok(Math.abs(dec - expected48k) <= 1, `decoded length ${dec} vs expected ${expected48k}`)

      const ratio = fs.statSync(ogg).size / fs.statSync(wav).size
      console.log(`[stem-encoding] ${sr}Hz WAV ${fs.statSync(wav).size}B → OGG ${fs.statSync(ogg).size}B (${(ratio * 100).toFixed(1)}%)`)
      assert.ok(ratio < 0.2, `expected strong compression, got ${ratio}`)
    })
  }

  it('prepareStemForUpload returns an .ogg next to the source', async () => {
    const wav = path.join(dir, 'prep.wav')
    makeWav(wav, 44100, 44100)
    const p = await prepareStemForUpload(wav)
    assert.equal(p.ext, 'ogg')
    assert.equal(p.contentType, 'audio/ogg')
    assert.equal(p.fellBack, false)
    assert.notEqual(p.path, wav)
    assert.ok(fs.existsSync(p.path))
    assert.ok(fs.existsSync(wav), 'source wav kept for the caller to clean up')
  })
})

describe('prepareStemForUpload fallback', () => {
  it('falls back to WAV when ffmpeg is missing', async () => {
    const wav = path.join(dir, 'fb.wav')
    makeWav(wav, 44100, 4410)
    const p = await prepareStemForUpload(wav, { ffmpeg: path.join(dir, 'no-such-ffmpeg') })
    assert.deepEqual(p, { path: wav, ext: 'wav', contentType: 'audio/wav', fellBack: true })
  })

  it('falls back to WAV when input is not audio, without leaving partial output', { skip: !hasFfmpeg && 'ffmpeg not available' }, async () => {
    const bad = path.join(dir, 'bad.wav')
    fs.writeFileSync(bad, 'not a wav file')
    const p = await prepareStemForUpload(bad)
    assert.equal(p.ext, 'wav')
    assert.equal(p.fellBack, true)
    assert.deepEqual(fs.readdirSync(dir).filter(f => f.startsWith('bad.') && f.endsWith('.ogg')), [])
  })

  describe('STEM_FORMAT=wav kill switch', () => {
    let prev: string | undefined
    before(() => { prev = process.env.STEM_FORMAT; process.env.STEM_FORMAT = 'wav' })
    after(() => { if (prev === undefined) delete process.env.STEM_FORMAT; else process.env.STEM_FORMAT = prev })
    it('skips encoding', async () => {
      const wav = path.join(dir, 'ks.wav')
      makeWav(wav, 44100, 441)
      const p = await prepareStemForUpload(wav)
      assert.deepEqual(p, { path: wav, ext: 'wav', contentType: 'audio/wav', fellBack: false })
    })
  })
})

describe('stem storage paths', () => {
  it('covers both .ogg and .wav for every stem', () => {
    const paths = stemStoragePaths('u1/sep1')
    assert.equal(paths.length, STEM_NAMES.length * 2)
    for (const n of STEM_NAMES) {
      assert.ok(paths.includes(`u1/sep1/${n}.ogg`))
      assert.ok(paths.includes(`u1/sep1/${n}.wav`))
    }
  })

  it('detects format from URL extension, ignoring query', () => {
    assert.equal(stemFormatOf('https://x.supabase.co/storage/v1/object/public/audio-stems/a/b/vocals.ogg'), 'ogg')
    assert.equal(stemFormatOf('https://x/a/b/vocals.WAV?t=1'), 'wav')
    assert.equal(stemFormatOf('https://x/a/b/vocals.mp3'), null)
  })

  it('extracts bucket path from public URL', () => {
    assert.equal(
      storagePathFromPublicUrl('https://x.supabase.co/storage/v1/object/public/audio-stems/u%201/sep/vocals.wav?x=1'),
      'u 1/sep/vocals.wav'
    )
    assert.equal(storagePathFromPublicUrl('https://example.com/vocals.wav'), null)
  })
})

/** cleanupExpiredSeparations용 최소 Supabase 가짜 */
function fakeSupabase(rows: { id: string; storage_folder: string | null }[], opts?: { failFolder?: string }) {
  const removed: string[][] = []
  const deletedIds: string[][] = []
  const client = {
    from: () => ({
      select: () => ({ lt: async () => ({ data: rows, error: null }) }),
      delete: () => ({ in: async (_c: string, ids: string[]) => { deletedIds.push(ids); return { error: null } } }),
    }),
    storage: {
      from: () => ({
        remove: async (paths: string[]) => {
          removed.push(paths)
          if (opts?.failFolder && paths[0].startsWith(opts.failFolder + '/')) return { data: null, error: { message: 'boom' } }
          return { data: [], error: null }
        },
      }),
    },
  }
  return { client: client as any, removed, deletedIds }
}

describe('cleanupExpiredSeparations', () => {
  it('removes both .ogg and .wav for each expired separation, then deletes rows', async () => {
    const f = fakeSupabase([{ id: 'a', storage_folder: 'u/a' }, { id: 'b', storage_folder: 'u/b' }])
    const n = await cleanupExpiredSeparations(f.client)
    assert.equal(n, 2)
    assert.equal(f.removed.length, 2)
    assert.ok(f.removed[0].includes('u/a/vocals.ogg') && f.removed[0].includes('u/a/vocals.wav'))
    assert.ok(f.removed[1].includes('u/b/other.ogg') && f.removed[1].includes('u/b/other.wav'))
    assert.deepEqual(f.deletedIds, [['a', 'b']])
  })

  it('keeps the DB row when storage removal fails (retry next run, no orphan files)', async () => {
    const f = fakeSupabase([{ id: 'a', storage_folder: 'u/a' }, { id: 'b', storage_folder: 'u/b' }], { failFolder: 'u/a' })
    const n = await cleanupExpiredSeparations(f.client)
    assert.equal(n, 1)
    assert.deepEqual(f.deletedIds, [['b']])
  })
})

describe('encode limiter (M2: ffmpeg 동시 실행 제한)', () => {
  it('runs at most n tasks at once, FIFO, and keeps going after failures', async () => {
    const { createLimiter } = await import('../src/services/stemEncoder')
    const limit = createLimiter(1)
    let running = 0
    let peak = 0
    const order: number[] = []
    const task = (i: number, fail = false) => limit(async () => {
      running++; peak = Math.max(peak, running)
      await new Promise(r => setTimeout(r, 5))
      order.push(i)
      running--
      if (fail) throw new Error(`fail ${i}`)
      return i
    })
    const results = await Promise.allSettled([task(0), task(1, true), task(2), task(3)])
    assert.equal(peak, 1)
    assert.deepEqual(order, [0, 1, 2, 3])
    assert.deepEqual(results.map(r => r.status), ['fulfilled', 'rejected', 'fulfilled', 'fulfilled'])
    assert.equal(limit.active, 0)
    assert.equal(limit.pending, 0)
  })

  it('allows n concurrent when configured higher', async () => {
    const { createLimiter } = await import('../src/services/stemEncoder')
    const limit = createLimiter(2)
    let running = 0
    let peak = 0
    await Promise.all([0, 1, 2, 3, 4].map(() => limit(async () => {
      running++; peak = Math.max(peak, running)
      await new Promise(r => setTimeout(r, 5))
      running--
    })))
    assert.equal(peak, 2)
  })

  it('concurrent encodeToOpus calls are serialized by the shared limiter (default 1)', { skip: !hasFfmpeg && 'ffmpeg not available' }, async () => {
    const { encodeLimiter } = await import('../src/services/stemEncoder')
    const wavs = [0, 1, 2].map(i => { const p = path.join(dir, `q${i}.wav`); makeWav(p, 44100, 44100); return p })
    let peak = 0
    const poll = setInterval(() => { peak = Math.max(peak, encodeLimiter.active) }, 1)
    try {
      await Promise.all(wavs.map((w, i) => encodeToOpus(w, path.join(dir, `q${i}.ogg`))))
    } finally {
      clearInterval(poll)
    }
    assert.equal(peak, 1)
    for (let i = 0; i < 3; i++) assert.ok(fs.statSync(path.join(dir, `q${i}.ogg`)).size > 0)
  })
})
