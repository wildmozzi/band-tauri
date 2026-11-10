import { describe, expect, it } from 'vitest'
import {
  DEFAULT_ENGINE_CONFIG,
  ENGINE_PRESETS,
  JITTER_PROFILE_DEFAULT,
  applyPreset,
  clampJitterProfile,
  estimatePayloadBytes,
  formatPeerJitter,
  jitterProfileLabel,
  isPayloadAllowed,
  matchPreset,
  migrateLegacySettings,
  normalizeInputChannels,
  parseStoredSettings,
  toStored,
  validateEngineConfig,
} from './engineConfig'
import type { DeviceInfo, EngineConfig } from './engineApi'
import { tuneOpusSdp } from '../lib/webrtcAudio'

describe('패킷 크기 제한 (1200B)', () => {
  it('PCM16 바이트 수', () => {
    expect(estimatePayloadBytes('pcm16', 2, 10)).toBe(1920)
    expect(estimatePayloadBytes('pcm16', 1, 10)).toBe(960)
    expect(estimatePayloadBytes('pcm16', 2, 5)).toBe(960)
    expect(estimatePayloadBytes('pcm16', 1, 2.5)).toBe(240)
  })
  it('PCM16 스테레오 10ms만 금지', () => {
    expect(isPayloadAllowed('pcm16', 2, 10)).toBe(false)
    expect(isPayloadAllowed('pcm16', 2, 5)).toBe(true)
    expect(isPayloadAllowed('pcm16', 1, 10)).toBe(true)
    expect(isPayloadAllowed('opus', 2, 10)).toBe(true)
  })
  it('검증이 금지 조합을 잡는다', () => {
    const bad: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, codec: 'pcm16', channels: 2, frameMs: 10, inputChannels: [0, 1] }
    expect(validateEngineConfig(bad).map((i) => i.field)).toContain('codec')
    expect(validateEngineConfig(DEFAULT_ENGINE_CONFIG)).toEqual([])
  })
})

describe('validateEngineConfig', () => {
  const device: DeviceInfo = { id: 'd', name: 'IF', driver: 'asio', isInput: true, isDefault: true, channels: 2, sampleRates: [48000] }
  it('버퍼 범위', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, bufferSize: 16 })[0].field).toBe('bufferSize')
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, bufferSize: 2048 })[0].field).toBe('bufferSize')
  })
  it('입력 채널이 장치 범위를 넘으면 거부', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, inputChannels: [3] }, device)[0].field).toBe('inputChannels')
  })
  it('채널 수와 입력 채널 수 불일치', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, channels: 2, inputChannels: [0] })[0].field).toBe('inputChannels')
  })
  it('지터 최소 > 최대', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, jitterMinMs: 100, jitterMaxMs: 50 })[0].field).toBe('jitterMinMs')
  })
})

describe('normalizeInputChannels', () => {
  it('모노/스테레오와 장치 채널 수에 맞춘다', () => {
    expect(normalizeInputChannels(1, [1, 0])).toEqual([1])
    expect(normalizeInputChannels(2, [0])).toEqual([0, 1])
    expect(normalizeInputChannels(2, [5, 6], 2)).toEqual([0, 0])
    expect(normalizeInputChannels(2, [1], 2)).toEqual([1, 0])
    expect(normalizeInputChannels(1, undefined, 1)).toEqual([0])
  })
})

describe('프리셋', () => {
  it('모든 프리셋은 유효하고 matchPreset으로 다시 찾힌다', () => {
    for (const id of Object.keys(ENGINE_PRESETS) as (keyof typeof ENGINE_PRESETS)[]) {
      const cfg = applyPreset(DEFAULT_ENGINE_CONFIG, id)
      expect(validateEngineConfig(cfg)).toEqual([])
      expect(matchPreset(cfg)).toBe(id)
    }
  })
  it('프리셋은 장치/드라이버를 바꾸지 않는다', () => {
    const base: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio', inputDevice: 'asio:Focusrite' }
    const cfg = applyPreset(base, 'stable')
    expect(cfg.inputDriver).toBe('asio')
    expect(cfg.inputDevice).toBe('asio:Focusrite')
    expect(cfg.inputChannels).toHaveLength(2)
  })
})

describe('저장 형식 / 마이그레이션', () => {
  it('신형 형식 왕복', () => {
    const cfg: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, codec: 'pcm16', frameMs: 2.5, bufferSize: 64, inputDevice: 'x' }
    expect(parseStoredSettings(toStored(cfg))).toEqual({ config: cfg, migrated: false })
  })

  it('구형(lib/tauri.ts AudioSettings) 형식을 변환한다', () => {
    const legacy = {
      driver: 'Asio',
      input_device: 'Focusrite USB ASIO',
      output_device: null,
      sample_rate: 48000,
      buffer_size: 64,
      compression: 'Uncompressed',
      channels: 'Mono',
      mono_channel: 'Ch2',
    }
    const { config, migrated } = parseStoredSettings(legacy)
    expect(migrated).toBe(true)
    expect(config.inputDriver).toBe('asio')
    expect(config.outputDriver).toBe('asio')
    expect(config.bufferSize).toBe(64)
    expect(config.codec).toBe('pcm16')
    expect(config.channels).toBe(1)
    expect(config.inputChannels).toEqual([1])
    // 구형 장치 값은 이름이라 새 id 체계로 옮기지 않는다
    expect(config.inputDevice).toBeNull()
    expect(validateEngineConfig(config)).toEqual([])
  })

  it('구형 분리 드라이버 + 스테레오 스왑', () => {
    const config = migrateLegacySettings({
      driver: 'WasapiShared',
      input_driver: 'Asio',
      output_driver: 'WasapiExclusive',
      buffer_size: 256,
      compression: 'Opus',
      channels: 'Stereo',
      stereo_swap: true,
    })
    expect(config.inputDriver).toBe('asio')
    expect(config.outputDriver).toBe('wasapiExclusive')
    expect(config.channels).toBe(2)
    expect(config.inputChannels).toEqual([1, 0])
    expect(config.codec).toBe('opus')
  })

  it('구형 PCM 스테레오는 1200B 이하가 되도록 프레임을 맞춘다', () => {
    const config = migrateLegacySettings({ driver: 'Asio', compression: 'Uncompressed', channels: 'Stereo', buffer_size: 128 })
    expect(isPayloadAllowed(config.codec, config.channels, config.frameMs)).toBe(true)
  })

  it('AudioSettingsContext v2(웹) 형식도 구형으로 감지', () => {
    const { config, migrated } = parseStoredSettings({ inputDeviceId: 'abc', sampleRate: 48000, hostId: 'ASIO', bufferSize: 512, echoCancellation: false })
    expect(migrated).toBe(true)
    expect(config.inputDriver).toBe('asio')
    expect(config.bufferSize).toBe(512)
  })

  it('알 수 없는 값/범위 밖 값은 기본값으로', () => {
    const { config } = parseStoredSettings({ version: 2, engine: { bufferSize: 99999, frameMs: 7, codec: 'mp3', inputDriver: 'alsa', channels: 3 } })
    expect(config.bufferSize).toBe(1024)
    expect(config.frameMs).toBe(DEFAULT_ENGINE_CONFIG.frameMs)
    expect(config.codec).toBe('opus')
    expect(config.inputDriver).toBe(DEFAULT_ENGINE_CONFIG.inputDriver)
    expect(config.channels).toBe(1)
  })

  it('비어 있으면 기본값', () => {
    expect(parseStoredSettings(null)).toEqual({ config: DEFAULT_ENGINE_CONFIG, migrated: false })
  })
})

describe('tuneOpusSdp', () => {
  const sdp = [
    'v=0',
    'm=audio 9 UDP/TLS/RTP/SAVPF 109 0',
    'a=rtpmap:109 opus/48000/2',
    'a=fmtp:109 minptime=10;useinbandfec=0;stereo=0',
    'a=rtpmap:0 PCMU/8000',
    '',
  ].join('\r\n')

  it('Opus payload type을 찾아 stereo/FEC를 켠다', () => {
    const out = tuneOpusSdp(sdp, { stereo: true, maxBitrate: 128000 })
    expect(out).toContain('a=fmtp:109 minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1;maxaveragebitrate=128000')
  })

  it('fmtp가 없으면 추가한다', () => {
    const noFmtp = sdp.replace('a=fmtp:109 minptime=10;useinbandfec=0;stereo=0\r\n', '')
    const out = tuneOpusSdp(noFmtp, { stereo: true })
    expect(out).toContain('a=rtpmap:109 opus/48000/2\r\na=fmtp:109 stereo=1;sprop-stereo=1;useinbandfec=1\r\n')
  })

  it('Opus가 없으면 그대로', () => {
    expect(tuneOpusSdp('v=0\r\n', { stereo: true })).toBe('v=0\r\n')
  })
})

describe('L3: 저장값 정리', () => {
  it('opusBitrate는 6k..510k 정수만 받아들인다', () => {
    for (const bad of [Number.NaN, -1, 1e9, 64000.5, '96000']) {
      const { config } = parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, opusBitrate: bad } })
      expect(config.opusBitrate).toBeUndefined()
    }
    const { config } = parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, opusBitrate: 128000 } })
    expect(config.opusBitrate).toBe(128000)
  })

  it('검증도 잘못된 비트레이트를 잡는다', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, opusBitrate: 1e9 }).map((i) => i.field)).toContain('opusBitrate')
  })

  it('지터 값은 0..500으로 클램프', () => {
    const { config } = parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, jitterMinMs: -5, jitterMaxMs: 99999 } })
    expect(config.jitterMinMs).toBe(0)
    expect(config.jitterMaxMs).toBe(500)
    expect(validateEngineConfig(config)).toEqual([])
  })

  it('모르는 형식(빈 객체, 미래 버전)은 기본값이지만 migrated=false → DB를 덮어쓰지 않음', () => {
    expect(parseStoredSettings({}).migrated).toBe(false)
    expect(parseStoredSettings({ version: 3, engine: { codec: 'flac' } }).migrated).toBe(false)
    expect(parseStoredSettings('garbage').migrated).toBe(false)
  })
})

describe('P1-5: 지터 프로필(저지연 ↔ 안정)', () => {
  it('기본값은 균형 0.5', () => {
    expect(DEFAULT_ENGINE_CONFIG.jitterProfile).toBe(JITTER_PROFILE_DEFAULT)
    expect(JITTER_PROFILE_DEFAULT).toBe(0.5)
  })
  it('0..1로 클램프하고 0.05 단위로 맞춘다. 숫자가 아니면 기본값', () => {
    expect(clampJitterProfile(-1)).toBe(0)
    expect(clampJitterProfile(2)).toBe(1)
    expect(clampJitterProfile(0.33)).toBe(0.35)
    expect(clampJitterProfile(Number.NaN)).toBe(0.5)
    expect(clampJitterProfile('1')).toBe(0.5)
  })
  it('저장값 파싱: 없거나 잘못되면 균형, 범위 밖은 클램프', () => {
    expect(parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, jitterProfile: undefined } }).config.jitterProfile).toBe(0.5)
    expect(parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, jitterProfile: 7 } }).config.jitterProfile).toBe(1)
    expect(parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, jitterProfile: 0.2 } }).config.jitterProfile).toBe(0.2)
  })
  it('검증: 범위 밖이면 jitterProfile 이슈', () => {
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, jitterProfile: 1.5 }).map((i) => i.field)).toContain('jitterProfile')
    expect(validateEngineConfig({ ...DEFAULT_ENGINE_CONFIG, jitterProfile: 0 })).toEqual([])
  })
  it('프로필은 프리셋 일치 판정에 영향을 주지 않는다', () => {
    const c = applyPreset(DEFAULT_ENGINE_CONFIG, 'balanced')
    expect(matchPreset({ ...c, jitterProfile: 0.9 })).toBe('balanced')
  })
  it('이름과 피어 상태 한 줄', () => {
    expect(jitterProfileLabel(0)).toBe('저지연')
    expect(jitterProfileLabel(0.5)).toBe('균형')
    expect(jitterProfileLabel(1)).toBe('안정')
    expect(formatPeerJitter({ bufferMs: 5.31, targetMs: 5.25, concealPct: 0.12 })).toBe('버퍼 5.3ms / 목표 5.3ms · 은닉 0.1%')
    expect(formatPeerJitter({ bufferMs: 5 })).toBe('버퍼 5.0ms')
  })
})
