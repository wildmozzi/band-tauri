import { describe, it, expect, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import {
  stemFormatOf,
  stemFileExtension,
  looksLikeOgg,
  decodeStemAudio,
  fetchAndDecodeStem,
  readOggOpusTiming,
  wasmOggOpusDecoder,
  fetchStemForDownload,
  type DecodeContext,
} from './stemAudio'

const OGG_URL = 'https://x.supabase.co/storage/v1/object/public/audio-stems/u/sep/vocals.ogg'
const WAV_URL = 'https://x.supabase.co/storage/v1/object/public/audio-stems/u/sep/vocals.wav'

function oggBytes(): ArrayBuffer {
  return new Uint8Array([0x4f, 0x67, 0x67, 0x53, 0, 2, 0, 0]).buffer
}

/** AudioBuffer 최소 가짜 */
function fakeBuffer(channels: number, length: number, sampleRate: number) {
  const data = Array.from({ length: channels }, () => new Float32Array(length))
  return {
    numberOfChannels: channels,
    length,
    sampleRate,
    duration: length / sampleRate,
    data,
    copyToChannel: (src: Float32Array, ch: number) => data[ch].set(src),
  } as unknown as AudioBuffer & { data: Float32Array[] }
}

function ctx(native: (b: ArrayBuffer) => Promise<AudioBuffer>): DecodeContext {
  return { decodeAudioData: native, createBuffer: (c, l, sr) => fakeBuffer(c, l, sr) }
}

describe('stemFormatOf', () => {
  it('detects format from URL extension, ignoring query/hash and case', () => {
    expect(stemFormatOf(OGG_URL)).toBe('ogg')
    expect(stemFormatOf(WAV_URL + '?v=1')).toBe('wav')
    expect(stemFormatOf('https://x/a/VOCALS.OGG#t')).toBe('ogg')
    expect(stemFormatOf('song_vocals.mp3')).toBe('mp3')
    expect(stemFormatOf('https://x/a/stem')).toBe('unknown')
  })
  it('detects format from data URL mime', () => {
    expect(stemFormatOf('data:audio/ogg;base64,AAAA')).toBe('ogg')
    expect(stemFormatOf('data:audio/wav;base64,AAAA')).toBe('wav')
    expect(stemFormatOf('data:audio/mpeg;base64,AAAA')).toBe('mp3')
  })
  it('download extension follows the stored format (legacy wav stays wav)', () => {
    expect(stemFileExtension(OGG_URL)).toBe('ogg')
    expect(stemFileExtension(WAV_URL)).toBe('wav')
    expect(stemFileExtension('https://x/a/stem')).toBe('wav')
  })
  it('sniffs Ogg magic bytes', () => {
    expect(looksLikeOgg(oggBytes())).toBe(true)
    expect(looksLikeOgg(new TextEncoder().encode('RIFF....').buffer)).toBe(false)
  })
})

describe('decodeStemAudio', () => {
  it('uses native decodeAudioData when it works (Chrome/Edge/Firefox/WebView2)', async () => {
    const native = fakeBuffer(2, 10, 48000)
    const fallback = vi.fn()
    const out = await decodeStemAudio(ctx(async () => native), oggBytes(), OGG_URL, fallback)
    expect(out).toBe(native)
    expect(fallback).not.toHaveBeenCalled()
  })

  it('falls back to wasm decoder for Ogg when native decode fails (old Safari)', async () => {
    const pcm = { channelData: [new Float32Array([0.1, 0.2, 0.3]), new Float32Array([-0.1, -0.2, -0.3])], sampleRate: 48000 }
    const fallback = vi.fn(async () => pcm)
    const out = (await decodeStemAudio(
      ctx(async () => { throw new DOMException('Unable to decode audio data', 'EncodingError') }),
      oggBytes(),
      OGG_URL,
      fallback
    )) as AudioBuffer & { data: Float32Array[] }
    expect(fallback).toHaveBeenCalledOnce()
    expect(out.numberOfChannels).toBe(2)
    expect(out.length).toBe(3)
    expect(out.sampleRate).toBe(48000)
    expect(Array.from(out.data[1])).toEqual(Array.from(pcm.channelData[1]))
  })

  it('falls back on Ogg magic bytes even when URL has no extension', async () => {
    const fallback = vi.fn(async () => ({ channelData: [new Float32Array(4)], sampleRate: 48000 }))
    await decodeStemAudio(ctx(async () => { throw new Error('nope') }), oggBytes(), 'https://x/a/stem', fallback)
    expect(fallback).toHaveBeenCalledOnce()
  })

  it('does not use the Opus fallback for WAV — rethrows the native error', async () => {
    const fallback = vi.fn()
    const err = new Error('bad wav')
    await expect(
      decodeStemAudio(ctx(async () => { throw err }), new TextEncoder().encode('RIFF....').buffer, WAV_URL, fallback)
    ).rejects.toBe(err)
    expect(fallback).not.toHaveBeenCalled()
  })

  it('passes a copy to decodeAudioData so the fallback still sees the bytes', async () => {
    const src = oggBytes()
    let seen: ArrayBuffer | null = null
    const fallback = vi.fn(async (b: Uint8Array) => {
      expect(b[0]).toBe(0x4f)
      return { channelData: [new Float32Array(1)], sampleRate: 48000 }
    })
    await decodeStemAudio(ctx(async (b) => { seen = b; throw new Error('x') }), src, OGG_URL, fallback)
    expect(seen).not.toBe(src)
  })
})

describe('fetchAndDecodeStem', () => {
  it('fetches http URLs and reports HTTP errors', async () => {
    const fetchMock = vi.fn(async () => new Response('nope', { status: 404 }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(fetchAndDecodeStem(ctx(async () => fakeBuffer(1, 1, 48000)), OGG_URL)).rejects.toThrow(/404/)
    expect(fetchMock).toHaveBeenCalledWith(OGG_URL)
    vi.unstubAllGlobals()
  })

  it('decodes data URLs without fetching', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    let got = 0
    await fetchAndDecodeStem(ctx(async (b) => { got = b.byteLength; return fakeBuffer(1, 1, 48000) }), 'data:audio/wav;base64,UklGRg==')
    expect(got).toBe(4)
    expect(fetchMock).not.toHaveBeenCalled()
    vi.unstubAllGlobals()
  })
})

describe('wasm Ogg Opus fallback on a real server-encoded fixture', () => {
  // __fixtures__/stem-0.5s.ogg: 0.5s 44.1kHz 스테레오 사인파 WAV를 서버 encodeToOpus(ffmpeg libopus 160k)로 인코딩한 것
  const fixture = () => new Uint8Array(readFileSync(new URL('./__fixtures__/stem-0.5s.ogg', import.meta.url)))

  it('reads pre-skip and playable length from the Ogg pages', () => {
    expect(readOggOpusTiming(fixture())).toEqual({ preSkip: 312, samples: 24000 })
  })

  it('decodes to exactly the encoded length (pre-skip trimmed, same as native)', async () => {
    const pcm = await wasmOggOpusDecoder(fixture())
    expect(pcm.sampleRate).toBe(48000)
    expect(pcm.channelData).toHaveLength(2)
    expect(pcm.channelData[0].length).toBe(24000) // 0.5s @ 48kHz
    const peak = pcm.channelData[0].reduce((m, v) => Math.max(m, Math.abs(v)), 0)
    expect(peak).toBeGreaterThan(0.05) // 무음이 아님 (lavfi sine 진폭 1/8)
  })

  it('returns null timing for non-Ogg data', () => {
    expect(readOggOpusTiming(new TextEncoder().encode('RIFF....WAVE'))).toBeNull()
  })
})

describe('wasm fallback serialization (iOS memory)', () => {
  it('decodes fallback stems one at a time, and a failure does not block the next', async () => {
    let running = 0
    let peak = 0
    const fallback = vi.fn(async () => {
      running++; peak = Math.max(peak, running)
      await new Promise(r => setTimeout(r, 5))
      running--
      return { channelData: [new Float32Array(2)], sampleRate: 48000 }
    })
    const failing = vi.fn(async () => { throw new Error('wasm boom') })
    const c = ctx(async () => { throw new Error('native fail') })
    const results = await Promise.allSettled([
      decodeStemAudio(c, oggBytes(), OGG_URL, fallback),
      decodeStemAudio(c, oggBytes(), OGG_URL, failing),
      decodeStemAudio(c, oggBytes(), OGG_URL, fallback),
      decodeStemAudio(c, oggBytes(), OGG_URL, fallback),
    ])
    expect(results.map(r => r.status)).toEqual(['fulfilled', 'rejected', 'fulfilled', 'fulfilled'])
    expect(peak).toBe(1)
  })
})

describe('fetchStemForDownload', () => {
  it('fetches the cross-origin URL as a blob and names it by stored format', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(new Uint8Array([1, 2, 3]), { status: 200 })))
    const ogg = await fetchStemForDownload(OGG_URL, 'vocals')
    expect(ogg.fileName).toBe('vocals.ogg')
    expect(ogg.blob.size).toBe(3)
    expect((await fetchStemForDownload(WAV_URL, 'drums')).fileName).toBe('drums.wav')
    vi.unstubAllGlobals()
  })
  it('rejects on HTTP error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('', { status: 403 })))
    await expect(fetchStemForDownload(OGG_URL, 'vocals')).rejects.toThrow(/403/)
    vi.unstubAllGlobals()
  })
})
