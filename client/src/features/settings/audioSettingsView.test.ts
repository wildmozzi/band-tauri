import { describe, expect, it } from 'vitest'
import {
  formatTestDevice,
  formatTestLatency,
  inputChannelOptions,
  isClipping,
  levelPercent,
  parseChannels,
  parseJitterMs,
  payloadAllowed,
  peerLabel,
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
