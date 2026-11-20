/**
 * 저장된 장치가 이 PC에 없을 때 "이번 실행에 쓸 설정"을 정한다(P2-27, 순수 함수).
 *
 * 오디오 설정(드라이버·장치 id·버퍼)은 계정으로 동기화된다. 다른 PC에서 로그인하거나 인터페이스를 뽑은 채 시작하면
 * 저장된 장치 id가 이 PC에 없는데, 엔진은 이름으로 찾다 못 찾으면 기본 장치로 넘어가지 않고 "입력 장치를 찾을 수 없습니다"로
 * 시작에 실패한다(engine/device/cpal.rs `find_device`, wasapi/enumerate.rs `find_device`). 그래서 엔진 시작 직전에 여기서
 * 고친 설정을 넘긴다.
 *
 * 불변식: 이 결과는 **이번 실행에만** 쓴다. 저장된 설정(계정·로컬 캐시)에는 쓰지 않는다 — 원래 PC로 돌아가면 그대로 동작해야 한다.
 * 저장된 값은 사용자가 설정 화면에서 직접 바꿀 때만 바뀐다.
 */
import type { DeviceInfo, DriverKind, EngineConfig } from './engineApi'
import { BUFFER_MAX, BUFFER_MIN, DEFAULT_ENGINE_CONFIG, DRIVER_OPTIONS, normalizeInputChannels } from './engineConfig'

/** 어떤 오디오 설정 화면이든 열어 달라는 이벤트(Navigation이 듣고 모달을 연다) */
export const OPEN_AUDIO_SETTINGS_EVENT = 'band:open-audio-settings'

/** 지원 안 하는 드라이버를 대신할 순서: 기본값(공유) → 독점 → ASIO */
const DRIVER_FALLBACK_ORDER: DriverKind[] = ['wasapiShared', 'wasapiExclusive', 'asio']

/** 엔진 장치 id(`wasapi::이름`, `asio::이름`)에서 사람이 읽을 이름. 접두사가 없으면 전체(엔진 `device_name_from_id`와 같은 규칙) */
export function deviceNameFromId(id: string): string {
  const i = id.indexOf('::')
  return i >= 0 ? id.slice(i + 2) : id
}

/** 목적격 조사: 받침 있으면 을, 없으면(라틴 문자 포함) 를 */
function objectJosa(word: string): string {
  const c = word.charCodeAt(word.length - 1)
  return c >= 0xac00 && c <= 0xd7a3 && (c - 0xac00) % 28 !== 0 ? '을' : '를'
}

function driverLabel(d: DriverKind): string {
  return DRIVER_OPTIONS.find((o) => o.id === d)?.label ?? d
}

export type RunFallback =
  | { kind: 'driver'; dir: 'input' | 'output'; from: DriverKind; to: DriverKind }
  | { kind: 'device'; dir: 'input' | 'output'; savedId: string }
  | { kind: 'bufferSize'; from: number; to: number }
  | { kind: 'inputChannels'; from: number[]; to: number[] }

export interface RunConfig {
  /** 엔진에 넘길 설정. 고칠 것이 없으면 저장된 설정 객체 그대로 */
  config: EngineConfig
  fallbacks: RunFallback[]
  /** 사용자에게 보일 한 줄(폴백이 없으면 null) */
  notice: string | null
  /** 이 드라이버로 찾은 장치가 하나도 없는 방향(장치 목록을 모르면 빈 배열) — 시작해도 실패한다 */
  noDevices: ('input' | 'output')[]
}

function isSupported(supported: readonly DriverKind[] | null, d: DriverKind): boolean {
  return supported === null || supported.includes(d)
}

function devicesOf(devices: readonly DeviceInfo[], input: boolean, driver: DriverKind): DeviceInfo[] {
  return devices.filter((d) => d.isInput === input && d.driver === driver)
}

/** 이번 실행의 입력 장치(고른 것, 없으면 그 드라이버의 기본 장치). 모르면 null */
export function runInputDevice(config: EngineConfig, devices: readonly DeviceInfo[] | null): DeviceInfo | null {
  if (!devices) return null
  const list = devicesOf(devices, true, config.inputDriver)
  return config.inputDevice ? (list.find((d) => d.id === config.inputDevice) ?? null) : (list.find((d) => d.isDefault) ?? null)
}

/**
 * 저장된 장치·드라이버가 지금 목록에 없는지(엔진 시작 전에 목록을 새로 받아 볼지 판단).
 * 목록을 모르면(null) false.
 */
export function needsDeviceRecheck(config: EngineConfig, devices: readonly DeviceInfo[] | null): boolean {
  if (!devices) return false
  const missing = (input: boolean, driver: DriverKind, id: string | null) => {
    const list = devicesOf(devices, input, driver)
    return list.length === 0 || (id !== null && !list.some((d) => d.id === id))
  }
  return missing(true, config.inputDriver, config.inputDevice) || missing(false, config.outputDriver, config.outputDevice)
}

/**
 * 저장된 설정 → 이번 실행 설정.
 * - 드라이버를 이 배포본이 지원하지 않으면(예: ASIO 설정인데 ASIO 없는 빌드) 지원 드라이버(WASAPI 공유 우선)로, 장치는 기본 장치로.
 *   `supported`가 null이면(옛 엔진·아직 모름) 드라이버는 그대로.
 * - 저장된 장치 id가 그 드라이버의 장치 목록에 없으면 기본 장치(null)로. `devices`가 null이면(목록 조회 실패·아직 모름) 그대로.
 * - 입력 장치가 바뀌었으면 입력 채널을 새 장치 채널 수에 맞춘다(예: 4채널 인터페이스의 Ch3 → 내장 마이크 Ch1).
 * - 버퍼가 범위(32..1024 정수) 밖이면 기본값. 엔진에는 요청값이고 장치가 지원하는 가장 가까운 값으로 열리므로(설계 4절)
 *   범위 안의 값은 장치가 바뀌어도 그대로 둔다(장치 목록이 버퍼 범위를 알려 주지 않는다).
 */
export function resolveRunConfig(
  saved: EngineConfig,
  devices: readonly DeviceInfo[] | null,
  supported: readonly DriverKind[] | null,
): RunConfig {
  const fallbacks: RunFallback[] = []
  const next: EngineConfig = { ...saved }
  const alt = DRIVER_FALLBACK_ORDER.find((d) => isSupported(supported, d)) ?? null

  for (const dir of ['input', 'output'] as const) {
    const driverKey = dir === 'input' ? 'inputDriver' : 'outputDriver'
    const deviceKey = dir === 'input' ? 'inputDevice' : 'outputDevice'
    if (alt && !isSupported(supported, next[driverKey])) {
      fallbacks.push({ kind: 'driver', dir, from: next[driverKey], to: alt })
      next[driverKey] = alt
      // 다른 드라이버의 장치 id는 쓸 수 없다 → 그 드라이버의 기본 장치
      next[deviceKey] = null
      continue
    }
    const id = next[deviceKey]
    if (id && devices && !devicesOf(devices, dir === 'input', next[driverKey]).some((d) => d.id === id)) {
      fallbacks.push({ kind: 'device', dir, savedId: id })
      next[deviceKey] = null
    }
  }

  const inputChanged = fallbacks.some((f) => (f.kind === 'driver' || f.kind === 'device') && f.dir === 'input')
  if (inputChanged) {
    const dev = runInputDevice(next, devices)
    const fixed = normalizeInputChannels(next.channels, next.inputChannels, dev?.channels)
    if (fixed.length !== next.inputChannels.length || fixed.some((c, i) => c !== next.inputChannels[i])) {
      fallbacks.push({ kind: 'inputChannels', from: [...next.inputChannels], to: [...fixed] })
      next.inputChannels = fixed
    }
  }

  if (!Number.isInteger(next.bufferSize) || next.bufferSize < BUFFER_MIN || next.bufferSize > BUFFER_MAX) {
    fallbacks.push({ kind: 'bufferSize', from: next.bufferSize, to: DEFAULT_ENGINE_CONFIG.bufferSize })
    next.bufferSize = DEFAULT_ENGINE_CONFIG.bufferSize
  }

  const noDevices: ('input' | 'output')[] = []
  if (devices) {
    if (devicesOf(devices, true, next.inputDriver).length === 0) noDevices.push('input')
    if (devicesOf(devices, false, next.outputDriver).length === 0) noDevices.push('output')
  }

  if (fallbacks.length === 0) return { config: saved, fallbacks, notice: null, noDevices }
  return { config: next, fallbacks, notice: fallbackNotice(fallbacks), noDevices }
}

/**
 * "저장된 장치 'X'를 찾을 수 없어 기본 장치로 시작했습니다" 같은 한 줄.
 * `tense`: 시작한 뒤 알림은 past, 설정 화면처럼 시작 전 안내는 future("…시작합니다").
 */
export function fallbackNotice(fallbacks: readonly RunFallback[], tense: 'past' | 'future' = 'past'): string | null {
  const start = tense === 'past' ? '시작했습니다' : '시작합니다'
  const parts: string[] = []
  const drivers = fallbacks.filter((f): f is Extract<RunFallback, { kind: 'driver' }> => f.kind === 'driver')
  if (drivers.length > 0) {
    const from = [...new Set(drivers.map((f) => driverLabel(f.from)))].join(', ')
    parts.push(`저장된 드라이버(${from})는 이 배포본에서 쓸 수 없어 ${driverLabel(drivers[0].to)} 기본 장치로 ${start}.`)
  }
  const devices = fallbacks.filter((f): f is Extract<RunFallback, { kind: 'device' }> => f.kind === 'device')
  if (devices.length > 0) {
    const names = [...new Set(devices.map((f) => deviceNameFromId(f.savedId)))]
    const last = names[names.length - 1]
    parts.push(`저장된 장치 ${names.map((n) => `'${n}'`).join(', ')}${objectJosa(last)} 찾을 수 없어 기본 장치로 ${start}.`)
  }
  const ch = fallbacks.find((f): f is Extract<RunFallback, { kind: 'inputChannels' }> => f.kind === 'inputChannels')
  if (ch) parts.push(`입력 채널은 ${ch.to.map((c) => `Ch${c + 1}`).join('·')}로 맞춥니다.`)
  const buf = fallbacks.find((f): f is Extract<RunFallback, { kind: 'bufferSize' }> => f.kind === 'bufferSize')
  if (buf) parts.push(`버퍼 크기는 기본값 ${buf.to}로 ${start}.`)
  if (parts.length === 0) return null
  return `${parts.join(' ')} 저장된 설정은 그대로 둡니다(원래 장치에서는 그대로 동작).`
}

/** 장치가 하나도 없을 때 시작 전에 보일 문구(원인과 할 일). 없으면 null */
export function noDeviceMessage(noDevices: readonly ('input' | 'output')[], config: Pick<EngineConfig, 'inputDriver' | 'outputDriver'>): string | null {
  if (noDevices.length === 0) return null
  const what = noDevices.map((d) => (d === 'input' ? `입력 장치(${driverLabel(config.inputDriver)})` : `출력 장치(${driverLabel(config.outputDriver)})`)).join('와 ')
  return `이 PC에서 사용할 수 있는 ${what}를 찾지 못했습니다. 마이크·오디오 인터페이스(또는 스피커·헤드폰)를 연결한 뒤 오디오 설정에서 '장치 새로고침'을 누르고 다시 시도하세요.`
}
