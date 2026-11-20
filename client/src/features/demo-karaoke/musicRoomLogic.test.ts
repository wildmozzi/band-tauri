import { describe, expect, it } from 'vitest'
import {
  allStemsEnabled,
  dataUrlToBlob,
  decodeLegacyFilename,
  formatPlaybackTime,
  hasDuplicateSeparation,
  nonEmptyStems,
  progressAt,
  stemFileNameFromPath,
  stemInfo,
  stripCopySuffix,
} from './musicRoomLogic'

/** UTF-8 바이트를 Latin-1 글자로 읽은 '깨진 이름' 만들기 */
const mangle = (s: string) => String.fromCharCode(...new TextEncoder().encode(s))

describe('musicRoomLogic', () => {
  it('stemInfo: 알려진 스템은 한글, 모르면 🎵 + 이름', () => {
    expect(stemInfo('vocals')).toEqual({ icon: '🎤', label: '보컬' })
    expect(stemInfo('synth')).toEqual({ icon: '🎵', label: 'synth' })
  })

  it('decodeLegacyFilename: 깨진 한글 이름을 되살리고, 나머지는 그대로', () => {
    expect(decodeLegacyFilename(mangle('봄날.mp3'))).toBe('봄날.mp3')
    expect(decodeLegacyFilename('봄날.mp3')).toBe('봄날.mp3')
    expect(decodeLegacyFilename('song.mp3')).toBe('song.mp3')
  })

  it('stripCopySuffix: 확장자 앞 " (n)"만 지운다', () => {
    expect(stripCopySuffix('곡 (2).mp3')).toBe('곡.mp3')
    expect(stripCopySuffix('곡(2).mp3')).toBe('곡(2).mp3')
    expect(stripCopySuffix('곡 (2)')).toBe('곡 (2)')
  })

  it('hasDuplicateSeparation: 만료 안 된 같은 이름(번호·깨진 이름 무시)만', () => {
    const now = new Date('2026-11-12T00:00:00Z')
    const live = '2026-11-13T00:00:00Z'
    const expired = '2026-11-11T00:00:00Z'
    expect(hasDuplicateSeparation([{ original_filename: '곡.mp3', expires_at: live }], '곡 (1).mp3', now)).toBe(true)
    expect(hasDuplicateSeparation([{ original_filename: mangle('곡 (3).mp3'), expires_at: live }], '곡.mp3', now)).toBe(true)
    expect(hasDuplicateSeparation([{ original_filename: '곡.mp3', expires_at: expired }], '곡.mp3', now)).toBe(false)
    expect(hasDuplicateSeparation([{ original_filename: '다른곡.mp3', expires_at: live }], '곡.mp3', now)).toBe(false)
    expect(hasDuplicateSeparation([], '곡.mp3', now)).toBe(false)
  })

  it('progressAt: 바 안 위치를 0~1로 자른다', () => {
    const rect = { left: 100, width: 200 }
    expect(progressAt(200, rect)).toBe(0.5)
    expect(progressAt(50, rect)).toBe(0)
    expect(progressAt(400, rect)).toBe(1)
  })

  it('allStemsEnabled·nonEmptyStems', () => {
    expect(allStemsEnabled(['vocals', 'bass'])).toEqual({ vocals: true, bass: true })
    expect(nonEmptyStems({ vocals: 'u1', bass: '', drums: undefined, other: null })).toEqual({ vocals: 'u1' })
  })

  it('formatPlaybackTime: M:SS', () => {
    expect(formatPlaybackTime(0)).toBe('0:00')
    expect(formatPlaybackTime(65.9)).toBe('1:05')
    expect(formatPlaybackTime(600)).toBe('10:00')
  })

  it('dataUrlToBlob: MIME과 바이트를 그대로, MIME 없으면 audio/wav', async () => {
    const blob = dataUrlToBlob(`data:audio/ogg;base64,${btoa('abc')}`)
    expect(blob.type).toBe('audio/ogg')
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(new Uint8Array([97, 98, 99]))
    expect(dataUrlToBlob(`data:;base64,${btoa('x')}`).type).toBe('audio/wav')
  })

  it('stemFileNameFromPath: 마지막 경로 조각, 없으면 <스템>.wav', () => {
    expect(stemFileNameFromPath('out/a/vocals.wav', 'vocals')).toBe('vocals.wav')
    expect(stemFileNameFromPath('C:\\out\\bass.wav', 'bass')).toBe('bass.wav')
    expect(stemFileNameFromPath('out/', 'drums')).toBe('drums.wav')
  })
})
