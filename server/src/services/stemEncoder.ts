/**
 * 음원분리 스템 압축 (WAV → Opus in Ogg)
 *
 * demucs 결과 WAV(44.1kHz/16~32bit 스테레오, 곡당 6개)를 그대로 올리면
 * Supabase Storage 용량·트래픽이 금방 찬다. 업로드 전에 ffmpeg(libopus)로
 * 48kHz 스테레오 Opus(.ogg)로 인코딩해 약 1/10로 줄인다.
 *
 * - ffmpeg 경로: FFMPEG_PATH 환경변수 또는 PATH의 `ffmpeg` (server/Dockerfile에서 apk로 설치)
 * - STEM_FORMAT=wav 로 두면 인코딩을 끄고 기존처럼 WAV를 올린다(비상 스위치)
 * - 인코딩이 실패하면 WAV로 폴백하고 로그를 남긴다 (업로드 자체는 막지 않음)
 */

import { spawn } from 'child_process'
import * as fs from 'fs'
import * as path from 'path'

export const OPUS_BITRATE = process.env.STEM_OPUS_BITRATE || '160k'
const ENCODE_TIMEOUT_MS = 5 * 60 * 1000

export type StemFormat = 'ogg' | 'wav'

export interface PreparedStem {
  /** 업로드할 로컬 파일 */
  path: string
  ext: StemFormat
  contentType: string
  /** 인코딩을 시도했다가 실패해서 WAV로 폴백했으면 true */
  fellBack: boolean
}

export const CONTENT_TYPES: Record<StemFormat, string> = {
  ogg: 'audio/ogg',
  wav: 'audio/wav',
}

export function ffmpegPath(): string {
  return process.env.FFMPEG_PATH || 'ffmpeg'
}

/**
 * WAV(또는 ffmpeg가 읽을 수 있는 임의 오디오) → Opus/Ogg 48kHz 스테레오.
 * 실패 시 reject (stderr 마지막 부분 포함).
 */
export function encodeToOpus(inPath: string, outPath: string, opts?: { bitrate?: string; ffmpeg?: string }): Promise<void> {
  const args = [
    '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
    '-i', inPath,
    '-vn', '-map_metadata', '-1',
    '-c:a', 'libopus',
    '-b:a', opts?.bitrate || OPUS_BITRATE,
    '-vbr', 'on',
    '-application', 'audio',
    '-ar', '48000',
    '-ac', '2',
    '-f', 'ogg',
    outPath,
  ]
  return new Promise((resolve, reject) => {
    let stderr = ''
    let settled = false
    const done = (err?: Error) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      if (err) {
        try { fs.rmSync(outPath, { force: true }) } catch { /* 무시 */ }
        reject(err)
      } else {
        resolve()
      }
    }
    const proc = spawn(opts?.ffmpeg || ffmpegPath(), args, { stdio: ['ignore', 'ignore', 'pipe'] })
    const timer = setTimeout(() => {
      proc.kill('SIGKILL')
      done(new Error(`ffmpeg timeout after ${ENCODE_TIMEOUT_MS}ms`))
    }, ENCODE_TIMEOUT_MS)
    proc.stderr.on('data', (d: Buffer) => {
      stderr = (stderr + d.toString()).slice(-2000)
    })
    proc.on('error', (e) => done(e))
    proc.on('close', (code) => {
      if (code !== 0) return done(new Error(`ffmpeg exited with code ${code}: ${stderr.trim()}`))
      try {
        const st = fs.statSync(outPath)
        if (st.size === 0) return done(new Error('ffmpeg produced empty output'))
      } catch (e) {
        return done(e as Error)
      }
      done()
    })
  })
}

/**
 * 업로드 직전 스템 준비: Opus로 인코딩하고, 실패하면 원본 WAV를 그대로 쓴다.
 * 성공 시 원본 WAV는 지우지 않는다(호출자가 업로드 후 둘 다 정리).
 */
export async function prepareStemForUpload(localPath: string, opts?: { ffmpeg?: string }): Promise<PreparedStem> {
  const wav: PreparedStem = { path: localPath, ext: 'wav', contentType: CONTENT_TYPES.wav, fellBack: false }
  if ((process.env.STEM_FORMAT || '').toLowerCase() === 'wav') return wav

  const outPath = path.join(
    path.dirname(localPath),
    `${path.basename(localPath, path.extname(localPath))}.${process.pid}.${Date.now()}.ogg`
  )
  try {
    await encodeToOpus(localPath, outPath, opts)
    return { path: outPath, ext: 'ogg', contentType: CONTENT_TYPES.ogg, fellBack: false }
  } catch (err) {
    console.warn(`[StemEncoder] Opus 인코딩 실패 → WAV 폴백: ${path.basename(localPath)}:`, err instanceof Error ? err.message : err)
    return { ...wav, fellBack: true }
  }
}

// ---------------------------------------------------------------------------
// Ogg Opus 길이 계산 (테스트·마이그레이션 검증용, 외부 도구 없이)
// ---------------------------------------------------------------------------

/**
 * Ogg Opus 파일의 재생 샘플 수(48kHz 기준) = 마지막 granule position - pre-skip.
 * RFC 7845 §4: 디코더는 pre-skip만큼 앞을 버리고 마지막 페이지 granule에서 끝을 자른다.
 */
export function oggOpusSampleCount(buf: Buffer): { samples: number; preSkip: number; channels: number } {
  let off = 0
  let preSkip = -1
  let channels = 0
  let lastGranule = -1n
  while (off + 27 <= buf.length) {
    if (buf.toString('latin1', off, off + 4) !== 'OggS') throw new Error(`bad Ogg page at ${off}`)
    const granule = buf.readBigInt64LE(off + 6)
    const nSeg = buf[off + 26]
    let bodyLen = 0
    for (let i = 0; i < nSeg; i++) bodyLen += buf[off + 27 + i]
    const bodyStart = off + 27 + nSeg
    if (preSkip < 0 && buf.toString('latin1', bodyStart, bodyStart + 8) === 'OpusHead') {
      channels = buf[bodyStart + 9]
      preSkip = buf.readUInt16LE(bodyStart + 10)
    }
    if (granule >= 0n) lastGranule = granule
    off = bodyStart + bodyLen
  }
  if (preSkip < 0) throw new Error('OpusHead not found')
  if (lastGranule < 0n) throw new Error('no granule position')
  return { samples: Number(lastGranule) - preSkip, preSkip, channels }
}

export function oggOpusDurationSec(buf: Buffer): number {
  return oggOpusSampleCount(buf).samples / 48000
}

/** RIFF/WAVE 헤더로 길이(초) 계산. 청크를 순회하므로 LIST 등 부가 청크가 있어도 된다. */
export function wavDurationSec(buf: Buffer): number {
  if (buf.toString('latin1', 0, 4) !== 'RIFF' || buf.toString('latin1', 8, 12) !== 'WAVE') throw new Error('not a WAV file')
  let off = 12
  let byteRate = 0
  while (off + 8 <= buf.length) {
    const id = buf.toString('latin1', off, off + 4)
    const size = buf.readUInt32LE(off + 4)
    if (id === 'fmt ') byteRate = buf.readUInt32LE(off + 16)
    if (id === 'data') {
      if (!byteRate) throw new Error('WAV data before fmt chunk')
      // 스트리밍으로 쓴 WAV는 size가 0xFFFFFFFF일 수 있다 → 실제 남은 바이트로 제한
      return Math.min(size, buf.length - off - 8) / byteRate
    }
    off += 8 + size + (size & 1)
  }
  throw new Error('WAV data chunk not found')
}
