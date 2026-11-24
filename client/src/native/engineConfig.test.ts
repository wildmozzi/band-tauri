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
  resolveResampleQuality,
  accountDeviceSettings,
  pickDeviceSettings,
  toAccountStored,
  toStored,
  withDeviceLocal,
  withDeviceSettings,
  validateEngineConfig,
} from './engineConfig'
import type { DeviceInfo, EngineConfig } from './engineApi'
import { tuneOpusSdp } from '../shared/lib/webrtcAudio'

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
  it('프리셋은 프로필을 정한다: 초저지연 0, 균형 0.5, 안정 1', () => {
    expect(applyPreset(DEFAULT_ENGINE_CONFIG, 'ultraLow').jitterProfile).toBe(0)
    expect(applyPreset(DEFAULT_ENGINE_CONFIG, 'balanced').jitterProfile).toBe(0.5)
    expect(applyPreset(DEFAULT_ENGINE_CONFIG, 'stable').jitterProfile).toBe(1)
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
    expect(formatPeerJitter({ bufferMs: 5.31, targetMs: 5.25, concealPct: 0.12, corrTimePct: 8.4 })).toBe('버퍼 5.3ms / 목표 5.3ms · 은닉 0.1% · 보정 8%')
    expect(formatPeerJitter({ bufferMs: 5 })).toBe('버퍼 5.0ms')
  })
})

describe('P2-17e: 리샘플러 품질', () => {
  it('auto는 프리셋을 따른다: 초저지연(2.5ms) = 저지연, 균형·안정 = 고음질', () => {
    expect(resolveResampleQuality(applyPreset(DEFAULT_ENGINE_CONFIG, 'ultraLow'))).toBe('lowLatency')
    expect(resolveResampleQuality(applyPreset(DEFAULT_ENGINE_CONFIG, 'balanced'))).toBe('highQuality')
    expect(resolveResampleQuality(applyPreset(DEFAULT_ENGINE_CONFIG, 'stable'))).toBe('highQuality')
    expect(resolveResampleQuality({ frameMs: 5 })).toBe('highQuality')
  })
  it('사용자 선택은 프리셋보다 우선하고 프리셋을 바꿔도 유지된다', () => {
    const cfg: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, resampleQuality: 'highQuality' }
    const ultra = applyPreset(cfg, 'ultraLow')
    expect(ultra.resampleQuality).toBe('highQuality')
    expect(resolveResampleQuality(ultra)).toBe('highQuality')
    expect(resolveResampleQuality({ frameMs: 10, resampleQuality: 'lowLatency' })).toBe('lowLatency')
    expect(matchPreset(ultra)).toBe('ultraLow')
  })
  it('옛 설정(필드 없음)·모르는 값은 auto, 아는 값은 그대로', () => {
    const old: Partial<EngineConfig> = { ...DEFAULT_ENGINE_CONFIG }
    delete old.resampleQuality
    expect(parseStoredSettings({ version: 2, engine: old }).config.resampleQuality).toBe('auto')
    expect(parseStoredSettings({ version: 2, engine: { ...old, resampleQuality: 'sinc512' } }).config.resampleQuality).toBe('auto')
    expect(parseStoredSettings({ version: 2, engine: { ...old, resampleQuality: 'lowLatency' } }).config.resampleQuality).toBe('lowLatency')
    expect(migrateLegacySettings({ driver: 'ASIO' }).resampleQuality).toBe('auto')
  })
})

describe('엔진 스레드 코어 배치 정책(P1-21 2단계, 실험 옵션)', () => {
  it('기본은 둘 다 꺼짐이고, 옛 설정(필드 없음)·이상한 값도 꺼짐', () => {
    expect(DEFAULT_ENGINE_CONFIG.threadPolicy).toEqual({ highQos: false, preferPCores: false })
    const old: Partial<EngineConfig> = { ...DEFAULT_ENGINE_CONFIG }
    delete old.threadPolicy
    expect(parseStoredSettings({ version: 2, engine: old }).config.threadPolicy).toEqual({ highQos: false, preferPCores: false })
    for (const bad of [null, 'both', 1, { highQos: 'yes', preferPCores: 1 }]) {
      expect(parseStoredSettings({ version: 2, engine: { ...old, threadPolicy: bad } }).config.threadPolicy).toEqual({ highQos: false, preferPCores: false })
    }
    expect(migrateLegacySettings({ driver: 'ASIO' }).threadPolicy).toEqual({ highQos: false, preferPCores: false })
  })
  it('켠 값은 저장·복원되고 프리셋을 바꿔도 유지된다', () => {
    const on = { highQos: true, preferPCores: false }
    const stored = parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, threadPolicy: on } }).config
    expect(stored.threadPolicy).toEqual(on)
    expect(applyPreset(stored, 'ultraLow').threadPolicy).toEqual(on)
    expect(parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, threadPolicy: { preferPCores: true } } }).config.threadPolicy).toEqual({ highQos: false, preferPCores: true })
  })
})

describe('스레드 정책은 기기별 — 계정 저장 형식에서 뺀다(#245 재리뷰 L4)', () => {
  const on = { highQos: true, preferPCores: true }
  it('계정 형식에는 threadPolicy가 없고 나머지는 그대로', () => {
    const cfg: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, codec: 'pcm16', threadPolicy: on }
    const stored = toAccountStored(cfg)
    expect(stored.version).toBe(2)
    expect('threadPolicy' in stored.engine).toBe(false)
    expect(stored.engine.codec).toBe('pcm16')
    expect(cfg.threadPolicy).toEqual(on) // 원본은 건드리지 않음
    // 다시 읽으면 끔(새 PC에서 로그인한 것과 같다)
    expect(parseStoredSettings(stored).config.threadPolicy).toEqual({ highQos: false, preferPCores: false })
  })
  it('서버에 옛 값(켬)이 있어도 이 기기의 값이 이긴다', () => {
    const fromServer = parseStoredSettings({ version: 2, engine: { ...DEFAULT_ENGINE_CONFIG, threadPolicy: on } }).config
    expect(withDeviceLocal(fromServer, { highQos: false, preferPCores: false }).threadPolicy).toEqual({ highQos: false, preferPCores: false })
    expect(withDeviceLocal({ ...DEFAULT_ENGINE_CONFIG }, { highQos: false, preferPCores: true }).threadPolicy).toEqual({ highQos: false, preferPCores: true })
  })
})

describe('장치 쪽 값은 PC별 — 계정 저장 형식에서 뺀다(P2-27 후속)', () => {
  const pc: EngineConfig = {
    ...DEFAULT_ENGINE_CONFIG,
    inputDriver: 'asio',
    outputDriver: 'asio',
    inputDevice: 'asio::Focusrite USB ASIO',
    outputDevice: 'asio::Focusrite USB ASIO',
    inputChannels: [2, 3],
    channels: 2,
    bufferSize: 64,
    codec: 'pcm16',
    frameMs: 2.5,
    jitterMaxMs: 40,
  }
  it('계정 형식에는 드라이버·장치·입력 채널·버퍼가 없고 코덱·프레임·모노/스테레오·지터는 남는다', () => {
    const stored = toAccountStored(pc)
    for (const k of ['inputDriver', 'outputDriver', 'inputDevice', 'outputDevice', 'inputChannels', 'bufferSize']) {
      expect(k in stored.engine).toBe(false)
    }
    expect(stored.engine).toMatchObject({ codec: 'pcm16', frameMs: 2.5, channels: 2, jitterMaxMs: 40 })
    expect(pc.inputDevice).toBe('asio::Focusrite USB ASIO') // 원본은 건드리지 않음
    // 옛 앱이 읽어도(빠진 필드 = 기본값) 깨지지 않는다
    expect(parseStoredSettings(stored).config).toMatchObject({ inputDevice: null, inputDriver: 'wasapiShared', bufferSize: 128, codec: 'pcm16' })
  })
  it('계정 값의 장치 필드는 무시하고 이 PC의 값을 덮는다', () => {
    const fromServer = parseStoredSettings({ version: 2, engine: { ...pc, inputDevice: 'wasapi::다른 PC 마이크', bufferSize: 512 } }).config
    const here = { ...pickDeviceSettings(DEFAULT_ENGINE_CONFIG), inputDevice: 'wasapi::이 PC 마이크', inputChannels: [1] as [number] }
    const merged = withDeviceSettings(fromServer, here)
    expect(merged).toMatchObject({ inputDevice: 'wasapi::이 PC 마이크', inputDriver: 'wasapiShared', bufferSize: 128, codec: 'pcm16', frameMs: 2.5 })
    // 계정이 스테레오면 입력 채널 개수를 맞춘다(둘째 채널 채움)
    expect(merged.inputChannels).toEqual([1, 2])
    expect(withDeviceSettings({ ...fromServer, channels: 1 }, pickDeviceSettings(pc)).inputChannels).toEqual([2])
  })
  it('계정 값에 옛 장치 필드가 있을 때만 이행용 값을 돌려준다', () => {
    expect(accountDeviceSettings({ version: 2, engine: pc })).toEqual(pickDeviceSettings(pc))
    expect(accountDeviceSettings({ version: 2, engine: { inputDevice: 'wasapi::x' } })?.inputDevice).toBe('wasapi::x')
    expect(accountDeviceSettings(toAccountStored(pc))).toBeNull()
    expect(accountDeviceSettings(null)).toBeNull()
    expect(accountDeviceSettings({})).toBeNull()
    // 구형 형식(변환 대상)도 장치 값을 가져온다
    expect(accountDeviceSettings({ input_device: 'Mic', buffer_size: 256 })).not.toBeNull()
  })
})
