/**
 * EngineConfig 기본값/프리셋/검증/구형 설정 마이그레이션 (순수 함수, 테스트 대상).
 */
import {
  ENGINE_SAMPLE_RATE,
  MAX_PAYLOAD_BYTES,
  type CodecKind,
  type DeviceInfo,
  type DriverKind,
  type EngineConfig,
  type FrameMs,
} from './engineApi'

export const DRIVER_OPTIONS: { id: DriverKind; label: string; desc: string }[] = [
  { id: 'asio', label: 'ASIO', desc: '오디오 인터페이스 전용 드라이버 (최저 지연)' },
  { id: 'wasapiExclusive', label: 'WASAPI 독점', desc: '낮은 지연, 다른 앱 소리는 차단됨' },
  { id: 'wasapiShared', label: 'WASAPI 공유', desc: '일반 사용, 다른 앱과 장치 공유' },
]

export const FRAME_OPTIONS: FrameMs[] = [2.5, 5, 10]
export const BUFFER_OPTIONS = [32, 64, 128, 256, 512, 1024]
export const BUFFER_MIN = 32
export const BUFFER_MAX = 1024
export const JITTER_MAX_LIMIT_MS = 500
export const OPUS_BITRATE_MIN = 6_000
export const OPUS_BITRATE_MAX = 510_000

/** Opus 비트레이트: 정수이고 6k..510k일 때만 유효 */
export function isValidOpusBitrate(v: unknown): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= OPUS_BITRATE_MIN && v <= OPUS_BITRATE_MAX
}

export const DEFAULT_ENGINE_CONFIG: EngineConfig = {
  inputDriver: 'wasapiShared',
  outputDriver: 'wasapiShared',
  inputDevice: null,
  outputDevice: null,
  bufferSize: 128,
  frameMs: 5,
  codec: 'opus',
  channels: 1,
  inputChannels: [0],
  jitterMinMs: 0,
  jitterMaxMs: 80,
}

export type EnginePresetId = 'ultraLow' | 'balanced' | 'stable'

type PresetFields = Pick<EngineConfig, 'bufferSize' | 'frameMs' | 'codec' | 'channels' | 'jitterMinMs' | 'jitterMaxMs'>

export const ENGINE_PRESETS: Record<EnginePresetId, { label: string; desc: string; fields: PresetFields }> = {
  ultraLow: {
    label: '초저지연',
    desc: '버퍼 64 · 2.5ms · PCM16 · 모노 (LAN/유선 권장)',
    fields: { bufferSize: 64, frameMs: 2.5, codec: 'pcm16', channels: 1, jitterMinMs: 0, jitterMaxMs: 40 },
  },
  balanced: {
    label: '균형',
    desc: '버퍼 128 · 5ms · Opus · 모노',
    fields: { bufferSize: 128, frameMs: 5, codec: 'opus', channels: 1, jitterMinMs: 0, jitterMaxMs: 80 },
  },
  stable: {
    label: '안정',
    desc: '버퍼 256 · 10ms · Opus · 스테레오 (불안정한 회선)',
    fields: { bufferSize: 256, frameMs: 10, codec: 'opus', channels: 2, jitterMinMs: 20, jitterMaxMs: 150 },
  },
}

/** 프레임당 샘플 수 (48kHz) */
export function frameSamples(frameMs: FrameMs): number {
  return Math.round((ENGINE_SAMPLE_RATE * frameMs) / 1000)
}

/**
 * 패킷당 오디오 페이로드(바이트) 추정. PCM16은 정확, Opus는 비트레이트 상한 기준.
 */
export function estimatePayloadBytes(codec: CodecKind, channels: 1 | 2, frameMs: FrameMs, opusBitrate?: number): number {
  if (codec === 'pcm16') return frameSamples(frameMs) * channels * 2
  const bitrate = opusBitrate ?? defaultOpusBitrate(channels)
  return Math.ceil((bitrate * frameMs) / 1000 / 8)
}

export function defaultOpusBitrate(channels: 1 | 2): number {
  return channels === 2 ? 160_000 : 96_000
}

/** 1200B를 넘는 코덱/채널/프레임 조합인지 */
export function isPayloadAllowed(codec: CodecKind, channels: 1 | 2, frameMs: FrameMs, opusBitrate?: number): boolean {
  return estimatePayloadBytes(codec, channels, frameMs, opusBitrate) <= MAX_PAYLOAD_BYTES
}

/** 입력 채널 배열을 channels 수와 장치 채널 수에 맞춘다 */
export function normalizeInputChannels(
  channels: 1 | 2,
  current: number[] | undefined,
  deviceChannels?: number,
): [number] | [number, number] {
  const max = deviceChannels && deviceChannels > 0 ? deviceChannels : Number.POSITIVE_INFINITY
  const clamp = (n: number) => (Number.isInteger(n) && n >= 0 && n < max ? n : 0)
  const first = clamp(current?.[0] ?? 0)
  if (channels === 1) return [first]
  const fallbackSecond = max > 1 ? (first + 1 < max ? first + 1 : 0) : 0
  const second = current && current.length > 1 ? clamp(current[1]) : fallbackSecond
  return [first, second]
}

export interface ConfigIssue {
  field: keyof EngineConfig
  message: string
}

/** UI/엔진 시작 전 검증. 빈 배열이면 통과 */
export function validateEngineConfig(config: EngineConfig, inputDevice?: DeviceInfo | null): ConfigIssue[] {
  const issues: ConfigIssue[] = []
  if (!Number.isInteger(config.bufferSize) || config.bufferSize < BUFFER_MIN || config.bufferSize > BUFFER_MAX) {
    issues.push({ field: 'bufferSize', message: `버퍼 크기는 ${BUFFER_MIN}~${BUFFER_MAX} 사이여야 합니다.` })
  }
  if (!FRAME_OPTIONS.includes(config.frameMs)) {
    issues.push({ field: 'frameMs', message: '프레임은 2.5 / 5 / 10ms 중 하나여야 합니다.' })
  }
  if (config.channels !== 1 && config.channels !== 2) {
    issues.push({ field: 'channels', message: '채널은 모노 또는 스테레오여야 합니다.' })
  }
  if (config.inputChannels.length !== config.channels) {
    issues.push({ field: 'inputChannels', message: '입력 채널 수가 모노/스테레오 설정과 맞지 않습니다.' })
  }
  if (inputDevice && config.inputChannels.some((ch) => ch >= inputDevice.channels)) {
    issues.push({ field: 'inputChannels', message: `입력 장치는 ${inputDevice.channels}채널까지만 지원합니다.` })
  }
  if (config.opusBitrate !== undefined && !isValidOpusBitrate(config.opusBitrate)) {
    issues.push({ field: 'opusBitrate', message: `Opus 비트레이트는 ${OPUS_BITRATE_MIN}~${OPUS_BITRATE_MAX} 사이 정수여야 합니다.` })
  }
  if (!isPayloadAllowed(config.codec, config.channels, config.frameMs, config.opusBitrate)) {
    const bytes = estimatePayloadBytes(config.codec, config.channels, config.frameMs, config.opusBitrate)
    issues.push({
      field: 'codec',
      message: `패킷이 ${bytes}B로 ${MAX_PAYLOAD_BYTES}B를 넘습니다. PCM16 스테레오는 프레임 5ms 이하만 가능합니다.`,
    })
  }
  const jMin = config.jitterMinMs ?? 0
  const jMax = config.jitterMaxMs ?? 80
  if (jMin < 0 || jMax < 0 || jMax > JITTER_MAX_LIMIT_MS) {
    issues.push({ field: 'jitterMaxMs', message: `지터 버퍼는 0~${JITTER_MAX_LIMIT_MS}ms 사이여야 합니다.` })
  } else if (jMin > 0 && jMin > jMax) {
    issues.push({ field: 'jitterMinMs', message: '지터 최소값이 최대값보다 큽니다.' })
  }
  return issues
}

/** 어떤 프리셋과 일치하는지 (없으면 null = 사용자 정의) */
export function matchPreset(config: EngineConfig): EnginePresetId | null {
  for (const [id, preset] of Object.entries(ENGINE_PRESETS) as [EnginePresetId, (typeof ENGINE_PRESETS)[EnginePresetId]][]) {
    const f = preset.fields
    if (
      config.bufferSize === f.bufferSize &&
      config.frameMs === f.frameMs &&
      config.codec === f.codec &&
      config.channels === f.channels &&
      (config.jitterMinMs ?? 0) === f.jitterMinMs &&
      (config.jitterMaxMs ?? 80) === f.jitterMaxMs
    ) {
      return id
    }
  }
  return null
}

export function applyPreset(config: EngineConfig, preset: EnginePresetId, inputDevice?: DeviceInfo | null): EngineConfig {
  const fields = ENGINE_PRESETS[preset].fields
  return {
    ...config,
    ...fields,
    inputChannels: normalizeInputChannels(fields.channels, config.inputChannels, inputDevice?.channels),
  }
}

// ============================================================================
// DB(profiles.audio_settings) 저장 형식과 마이그레이션
// ============================================================================

export const STORED_SETTINGS_VERSION = 2

export interface StoredAudioSettings {
  version: typeof STORED_SETTINGS_VERSION
  engine: EngineConfig
}

export function toStored(config: EngineConfig): StoredAudioSettings {
  return { version: STORED_SETTINGS_VERSION, engine: config }
}

const DRIVERS: DriverKind[] = ['asio', 'wasapiShared', 'wasapiExclusive']
const LEGACY_DRIVER_MAP: Record<string, DriverKind> = {
  Asio: 'asio',
  ASIO: 'asio',
  WasapiExclusive: 'wasapiExclusive',
  WasapiShared: 'wasapiShared',
  WASAPI: 'wasapiShared',
}

/** 구형(lib/tauri.ts AudioSettings / AudioSettingsContext v2) 필드 */
const LEGACY_KEYS = [
  'driver', 'input_driver', 'output_driver', 'input_device', 'output_device', 'sample_rate', 'buffer_size',
  'compression', 'mono_channel', 'stereo_swap', 'hostId', 'inputDeviceId', 'echoCancellation',
]

export function isLegacySettings(raw: unknown): boolean {
  if (!raw || typeof raw !== 'object') return false
  const obj = raw as Record<string, unknown>
  if (obj.version === STORED_SETTINGS_VERSION) return false
  return LEGACY_KEYS.some((k) => k in obj)
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === 'object' && !Array.isArray(v)
}

/** 신형 EngineConfig 필드를 하나씩 검사해 받아들인다 (모르는 값은 기본값) */
function sanitizeEngineConfig(raw: Record<string, unknown>): EngineConfig {
  const d = DEFAULT_ENGINE_CONFIG
  const driver = (v: unknown, fb: DriverKind) => (DRIVERS.includes(v as DriverKind) ? (v as DriverKind) : fb)
  const str = (v: unknown) => (typeof v === 'string' && v.length > 0 ? v : null)
  const num = (v: unknown, fb: number) => (typeof v === 'number' && Number.isFinite(v) ? v : fb)

  const channels: 1 | 2 = raw.channels === 2 ? 2 : 1
  const frameMs = FRAME_OPTIONS.includes(raw.frameMs as FrameMs) ? (raw.frameMs as FrameMs) : d.frameMs
  const codec: CodecKind = raw.codec === 'pcm16' ? 'pcm16' : 'opus'
  const bufferSize = Math.min(BUFFER_MAX, Math.max(BUFFER_MIN, Math.round(num(raw.bufferSize, d.bufferSize))))
  const inputChannels = normalizeInputChannels(
    channels,
    Array.isArray(raw.inputChannels) ? (raw.inputChannels as number[]) : undefined,
  )

  const config: EngineConfig = {
    inputDriver: driver(raw.inputDriver, d.inputDriver),
    outputDriver: driver(raw.outputDriver, d.outputDriver),
    inputDevice: str(raw.inputDevice),
    outputDevice: str(raw.outputDevice),
    bufferSize,
    frameMs,
    codec,
    channels,
    inputChannels,
    jitterMinMs: clampJitter(num(raw.jitterMinMs, d.jitterMinMs ?? 0)),
    jitterMaxMs: clampJitter(num(raw.jitterMaxMs, d.jitterMaxMs ?? 80)),
  }
  if (isValidOpusBitrate(raw.opusBitrate)) config.opusBitrate = raw.opusBitrate
  if (typeof raw.udpPort === 'number' && raw.udpPort > 0 && raw.udpPort < 65536) config.udpPort = raw.udpPort
  return fixPayload(config)
}

function clampJitter(v: number): number {
  return Math.min(JITTER_MAX_LIMIT_MS, Math.max(0, Math.round(v)))
}

/** 1200B 초과 조합이면 프레임을 줄여서 맞춘다 */
function fixPayload(config: EngineConfig): EngineConfig {
  if (isPayloadAllowed(config.codec, config.channels, config.frameMs, config.opusBitrate)) return config
  const frame = [...FRAME_OPTIONS].reverse().find((f) => isPayloadAllowed(config.codec, config.channels, f, config.opusBitrate))
  return { ...config, frameMs: frame ?? 2.5 }
}

/**
 * 구형 설정 → EngineConfig. 의미가 같은 값(드라이버, 버퍼, 압축, 채널)만 옮기고 나머지는 기본값.
 * 구형 장치 값은 장치 "이름"이었고 신형은 DeviceInfo.id라서 옮기지 않는다(기본 장치).
 */
export function migrateLegacySettings(raw: Record<string, unknown>): EngineConfig {
  const d = DEFAULT_ENGINE_CONFIG
  const baseDriver = LEGACY_DRIVER_MAP[String(raw.driver ?? raw.hostId ?? '')] ?? d.inputDriver
  const inputDriver = LEGACY_DRIVER_MAP[String(raw.input_driver ?? '')] ?? baseDriver
  const outputDriver = LEGACY_DRIVER_MAP[String(raw.output_driver ?? '')] ?? baseDriver
  const legacyBuffer = typeof raw.buffer_size === 'number' ? raw.buffer_size
    : typeof raw.bufferSize === 'number' ? raw.bufferSize : d.bufferSize
  const channels: 1 | 2 = raw.channels === 'Stereo' ? 2 : 1
  const codec: CodecKind = raw.compression === 'Uncompressed' ? 'pcm16' : 'opus'
  const mono = raw.mono_channel === 'Ch2' ? 1 : 0
  const inputChannels: [number] | [number, number] = channels === 1
    ? [mono]
    : raw.stereo_swap === true ? [1, 0] : [0, 1]

  return sanitizeEngineConfig({
    ...d,
    inputDriver,
    outputDriver,
    bufferSize: legacyBuffer,
    codec,
    channels,
    inputChannels,
  })
}

/** DB/로컬 저장값(어떤 형식이든) → EngineConfig */
export function parseStoredSettings(raw: unknown): { config: EngineConfig; migrated: boolean } {
  if (isRecord(raw) && raw.version === STORED_SETTINGS_VERSION && isRecord(raw.engine)) {
    return { config: sanitizeEngineConfig(raw.engine), migrated: false }
  }
  if (isRecord(raw) && isLegacySettings(raw)) {
    return { config: migrateLegacySettings(raw), migrated: true }
  }
  // 모르는 형식(빈 객체, 미래 버전 등)은 기본값으로 쓰되 '변환됨'으로 보고하지 않는다 → DB를 덮어쓰지 않음
  return { config: { ...DEFAULT_ENGINE_CONFIG }, migrated: false }
}

/** 버퍼 크기 → ms (48kHz 기준 표시용) */
export function bufferMs(bufferSize: number, sampleRate = ENGINE_SAMPLE_RATE): number {
  return Math.round((bufferSize / sampleRate) * 10000) / 10
}
