/**
 * 스템 오디오 포맷 판별 + 디코딩 (P3-6)
 *
 * 서버는 스템을 Opus/Ogg(.ogg)로 저장한다(이전 기록·인코딩 실패분은 .wav).
 * - Chrome/Edge/Firefox/WebView2: decodeAudioData가 Ogg Opus를 네이티브로 디코딩
 * - Safari: 버전에 따라 Ogg 컨테이너를 decodeAudioData에서 못 읽는다.
 *   버전 판별(UA 스니핑)에 기대지 않고, 네이티브 디코딩이 실패하면
 *   wasm Opus 디코더(ogg-opus-decoder)를 지연 로드해 디코딩한다.
 *   → 서버에 AAC 사본을 따로 두지 않아도 되므로 저장 용량이 늘지 않는다.
 */

export type StemFormat = 'ogg' | 'wav' | 'mp3' | 'unknown'

/** URL/파일명/data URL에서 포맷 판별 (쿼리·해시 무시) */
export function stemFormatOf(url: string): StemFormat {
  if (url.startsWith('data:')) {
    const mime = url.slice(5, url.indexOf(';') > 0 ? url.indexOf(';') : url.indexOf(',')).toLowerCase()
    if (mime.includes('ogg') || mime.includes('opus')) return 'ogg'
    if (mime.includes('wav')) return 'wav'
    if (mime.includes('mpeg') || mime.includes('mp3')) return 'mp3'
    return 'unknown'
  }
  const clean = url.split(/[?#]/)[0].toLowerCase()
  if (clean.endsWith('.ogg') || clean.endsWith('.opus')) return 'ogg'
  if (clean.endsWith('.wav')) return 'wav'
  if (clean.endsWith('.mp3')) return 'mp3'
  return 'unknown'
}

/** 다운로드 파일명용 확장자 */
export function stemFileExtension(url: string): string {
  const f = stemFormatOf(url)
  return f === 'unknown' ? 'wav' : f
}

/** 파일 매직 바이트로 Ogg 여부 확인 (URL 확장자가 없을 때 대비) */
export function looksLikeOgg(bytes: ArrayBuffer): boolean {
  if (bytes.byteLength < 4) return false
  const b = new Uint8Array(bytes, 0, 4)
  return b[0] === 0x4f && b[1] === 0x67 && b[2] === 0x67 && b[3] === 0x53 // 'OggS'
}

export interface PcmAudio {
  channelData: Float32Array[]
  sampleRate: number
}

export type PcmDecoder = (bytes: Uint8Array) => Promise<PcmAudio>

/**
 * Ogg Opus 헤더에서 pre-skip과 재생 길이(48kHz 샘플) 읽기 (RFC 7845 §4).
 * 길이 = 마지막 페이지 granule position - pre-skip. 파싱 실패 시 null.
 */
export function readOggOpusTiming(bytes: Uint8Array): { preSkip: number; samples: number } | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const tag = (o: number, s: string) => {
    for (let i = 0; i < s.length; i++) if (bytes[o + i] !== s.charCodeAt(i)) return false
    return true
  }
  let off = 0
  let preSkip = -1
  let last = -1
  while (off + 27 <= bytes.length) {
    if (!tag(off, 'OggS')) return null
    const lo = view.getUint32(off + 6, true)
    const hi = view.getInt32(off + 10, true)
    const nSeg = bytes[off + 26]
    if (off + 27 + nSeg > bytes.length) return null
    let body = 0
    for (let i = 0; i < nSeg; i++) body += bytes[off + 27 + i]
    const start = off + 27 + nSeg
    if (preSkip < 0 && tag(start, 'OpusHead')) preSkip = view.getUint16(start + 10, true)
    if (hi >= 0) last = hi * 2 ** 32 + lo // granule -1(헤더 페이지 등)은 건너뜀
    off = start + body
  }
  if (preSkip < 0 || last < preSkip) return null
  return { preSkip, samples: last - preSkip }
}

/**
 * wasm 디코더 출력에서 pre-skip(앞)과 마지막 페이지 패딩(뒤)을 잘라
 * 브라우저 네이티브 디코딩과 같은 길이로 맞춘다.
 */
export function trimOpusPcm(channelData: Float32Array[], timing: { preSkip: number; samples: number } | null): Float32Array[] {
  if (!timing) return channelData
  return channelData.map((ch) => {
    const start = Math.min(timing.preSkip, ch.length)
    return ch.subarray(start, Math.min(ch.length, start + timing.samples))
  })
}

/** wasm Ogg Opus 디코더 (필요할 때만 로드되는 별도 청크) */
export const wasmOggOpusDecoder: PcmDecoder = async (bytes) => {
  const { OggOpusDecoder } = await import('ogg-opus-decoder')
  const decoder = new OggOpusDecoder({ forceStereo: false })
  try {
    await decoder.ready
    const out = await decoder.decodeFile(bytes)
    if (!out.samplesDecoded) throw new Error('ogg-opus-decoder: no samples decoded')
    return { channelData: trimOpusPcm(out.channelData, readOggOpusTiming(bytes)), sampleRate: out.sampleRate }
  } finally {
    decoder.free()
  }
}

/** decodeAudioData와 createBuffer만 쓰므로 테스트에서 가짜로 대체 가능 */
export interface DecodeContext {
  decodeAudioData(data: ArrayBuffer): Promise<AudioBuffer>
  createBuffer(numberOfChannels: number, length: number, sampleRate: number): AudioBuffer
}

/**
 * 스템 바이트 → AudioBuffer.
 * 1) 네이티브 decodeAudioData (대부분 브라우저)
 * 2) 실패했고 Ogg면 wasm 디코더 폴백 (구버전 Safari)
 */
export async function decodeStemAudio(
  ctx: DecodeContext,
  bytes: ArrayBuffer,
  url: string,
  fallback: PcmDecoder = wasmOggOpusDecoder
): Promise<AudioBuffer> {
  // decodeAudioData는 입력 버퍼를 detach하므로 사본을 넘긴다(폴백에서 원본 필요)
  try {
    return await ctx.decodeAudioData(bytes.slice(0))
  } catch (nativeErr) {
    const isOgg = stemFormatOf(url) === 'ogg' || looksLikeOgg(bytes)
    if (!isOgg) throw nativeErr
    console.warn('[stemAudio] native Ogg Opus decode failed, using wasm decoder:', nativeErr)
    const pcm = await fallback(new Uint8Array(bytes))
    const length = pcm.channelData[0]?.length ?? 0
    if (length === 0) throw nativeErr
    const buf = ctx.createBuffer(pcm.channelData.length, length, pcm.sampleRate)
    pcm.channelData.forEach((ch, i) => buf.copyToChannel(ch as Float32Array<ArrayBuffer>, i))
    return buf
  }
}

/** URL(또는 data URL)에서 스템을 받아 디코딩 */
export async function fetchAndDecodeStem(ctx: DecodeContext, url: string, fallback?: PcmDecoder): Promise<AudioBuffer> {
  let bytes: ArrayBuffer
  if (url.startsWith('data:')) {
    const bin = atob(url.slice(url.indexOf(',') + 1))
    const u8 = new Uint8Array(bin.length)
    for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i)
    bytes = u8.buffer
  } else {
    const res = await fetch(url)
    if (!res.ok) throw new Error(`stem fetch failed: ${res.status} ${url}`)
    bytes = await res.arrayBuffer()
  }
  return decodeStemAudio(ctx, bytes, url, fallback)
}
