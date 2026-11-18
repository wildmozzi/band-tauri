import { describe, expect, it } from 'vitest'
import {
  bufferFrameHint,
  formatTestDevice,
  formatTestLatency,
  formatTestResampler,
  inputChannelOptions,
  isClipping,
  levelPercent,
  parseChannels,
  parseJitterMs,
  payloadAllowed,
  peerLabel,
  resampleQualityHint,
  withLeftInputChannel,
  withRightInputChannel,
} from './audioSettingsView'
import { isPayloadAllowed } from '../../native/engineConfig'

describe('audioSettingsView', () => {
  it('levelPercent: 0~100으로 자른다', () => {
    expect(levelPercent(0.5)).toBe(50)
    expect(levelPercent(-1)).toBe(0)
    expect(levelPercent(2)).toBe(100)
  })

  it('isClipping: 90% 초과만 클립', () => {
    expect(isClipping(90)).toBe(false)
    expect(isClipping(90.1)).toBe(true)
  })

  it('inputChannelOptions: 모르면 2채널, 최소 1채널', () => {
    expect(inputChannelOptions(undefined)).toEqual([0, 1])
    expect(inputChannelOptions(0)).toEqual([0])
    expect(inputChannelOptions(4)).toEqual([0, 1, 2, 3])
  })

  it('payloadAllowed: 패치 값만 바꿔 한도를 본다', () => {
    const config = { codec: 'pcm16' as const, channels: 2 as const, frameMs: 5 as const, opusBitrate: 128000 }
    for (const frameMs of [2.5, 5, 10] as const) {
      expect(payloadAllowed(config, { frameMs })).toBe(isPayloadAllowed('pcm16', 2, frameMs, 128000))
    }
    expect(payloadAllowed(config, { codec: 'opus' })).toBe(isPayloadAllowed('opus', 2, 5, 128000))
    expect(payloadAllowed(config, {})).toBe(isPayloadAllowed('pcm16', 2, 5, 128000))
  })

  it('parseJitterMs: 빈 값·숫자 아님·음수는 0', () => {
    expect(parseJitterMs('40')).toBe(40)
    expect(parseJitterMs('')).toBe(0)
    expect(parseJitterMs('abc')).toBe(0)
    expect(parseJitterMs('-5')).toBe(0)
  })

  it('parseChannels: 2만 스테레오', () => {
    expect(parseChannels('2')).toBe(2)
    expect(parseChannels('1')).toBe(1)
    expect(parseChannels('3')).toBe(1)
  })

  it('withLeft/RightInputChannel: 한쪽만 바꾼다', () => {
    expect(withLeftInputChannel([0, 3], 2)).toEqual([2, 3])
    expect(withLeftInputChannel([0], 2)).toEqual([2, 1])
    expect(withRightInputChannel([0, 3], 5)).toEqual([0, 5])
  })

  it('peerLabel: id 앞 8자', () => {
    expect(peerLabel({ peerId: 'abcdef0123456789' })).toBe('abcdef01')
  })

  it('테스트 모니터 표시', () => {
    expect(formatTestLatency(3.14159)).toBe('3.1ms')
    expect(formatTestLatency(undefined)).toBe('--')
    expect(formatTestDevice(48000, 128)).toBe('48kHz · 버퍼 128')
  })
})

describe('bufferFrameHint (P2-17d ③)', () => {
  it('2.5ms 프레임 + 버퍼 64 초과면 64 권장 안내', () => {
    const h = bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiShared' })
    expect(h).toContain('64')
    expect(h).toContain('2.7ms')
    expect(bufferFrameHint({ bufferSize: 256, frameMs: 2.5, inputDriver: 'wasapiShared' })).toContain('5.3ms')
  })
  it('버퍼 64 이하이거나 2.5ms 프레임이 아니면 안내 없음', () => {
    expect(bufferFrameHint({ bufferSize: 64, frameMs: 2.5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 32, frameMs: 2.5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 512, frameMs: 10, inputDriver: 'wasapiShared' })).toBeNull()
  })
})

describe('bufferFrameHint 장치 레이트·ASIO (#196 리뷰 L4)', () => {
  it('48k를 지원하면 48k로, 아니면 장치 레이트로 ms를 계산', () => {
    const base = { bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiShared' } as const
    expect(bufferFrameHint(base, [44_100, 48_000])).toContain('2.7ms')
    expect(bufferFrameHint(base, [44_100])).toContain('2.9ms')
    expect(bufferFrameHint(base, [])).toContain('2.7ms')
  })
  it('ASIO면 제어판 확인 안내를 덧붙인다', () => {
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'asio' })).toContain('제어판')
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiExclusive' })).not.toContain('제어판')
  })
})

describe('P2-17e: 리샘플러 품질 안내', () => {
  it('입·출력 장치가 모두 48kHz로 열리면 영향 없음', () => {
    const h = resampleQualityHint('highQuality', true, [44_100, 48_000], [48_000])
    expect(h).toContain('영향이 없습니다')
  })
  it('48kHz가 아닌 장치를 짚고, auto면 실제 품질을 알린다', () => {
    const h = resampleQualityHint('lowLatency', true, [44_100], [48_000])
    expect(h).toContain('자동: 지금 프리셋에서는 저지연')
    expect(h).toContain('입력 장치가 48kHz가 아니라')
    expect(h).not.toContain('영향이 없습니다')
    const both = resampleQualityHint('highQuality', false, [44_100], [44_100])
    expect(both).toContain('입력·출력 장치가')
    expect(both).not.toContain('자동')
  })
  it('장치 레이트를 모르면 일반 설명', () => {
    expect(resampleQualityHint('highQuality', false)).toContain('장치가 48kHz가 아니면')
    expect(resampleQualityHint('highQuality', false, [48_000], [])).toContain('장치가 48kHz가 아니면')
  })
  it('테스트 모니터 표시', () => {
    expect(formatTestResampler(null)).toBe('없음 (48kHz)')
    expect(formatTestResampler(undefined)).toBe('없음 (48kHz)')
    expect(formatTestResampler({ quality: 'highQuality', inDelayMs: 1.44, outDelayMs: 1.32 })).toBe('고음질 · +2.8ms')
    expect(formatTestResampler({ quality: 'lowLatency', inDelayMs: 0.08, outDelayMs: 0 })).toBe('저지연 · +0.1ms')
  })
})
