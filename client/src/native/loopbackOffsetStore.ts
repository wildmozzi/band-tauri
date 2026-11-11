/**
 * 메트로놈 출력 지연 보정용 루프백 보정값 저장·재적용(P2-3c, 설계 1.4절 "값의 수명").
 *
 * - 엔진은 루프백 측정이 `ok`면 보정값을 장치 구성 키에 묶어 기억하지만, 기억은 엔진 프로세스(앱) 수명뿐이다.
 * - 그래서 앱이 같은 구성(드라이버·실제 백엔드·장치·버퍼·레이트)별로 localStorage에 저장해 두고,
 *   엔진이 시작될 때(테스트 모드·합주) 같은 구성이면 `engine_set_loopback_offset`으로 다시 넣는다.
 * - 기본 장치(null)는 그때의 OS 기본 장치 id로 풀어서 키에 넣는다(엔진 키도 실제로 연 장치 기준이라,
 *   기본 장치가 바뀌면 다른 키 — 다른 장치에 옛 값을 넣지 않는다).
 */
import {
  engineListDevices,
  engineSetLoopbackOffset,
  engineStats,
  type DeviceInfo,
  type EngineConfig,
  type EngineStats,
  type LoopbackReport,
  type MetronomeStats,
} from './engineApi'

/** engine_set_loopback_offset이 받는 범위(설계 4절). 밖이면 엔진도 기억하지 않는다 */
export const LOOPBACK_OFFSET_MIN_MS = -50
export const LOOPBACK_OFFSET_MAX_MS = 200

const KEY_PREFIX = 'band.loopbackOffset.'

export type OffsetStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

export interface StoredLoopbackOffset {
  offsetMs: number
  /** 측정 시각(ISO) */
  at: string
}

/** 장치 구성 키 재료. 장치 id는 기본 장치를 푼 값(못 풀면 null) */
export interface LoopbackDeviceKeyParts {
  inputDriver: EngineConfig['inputDriver']
  outputDriver: EngineConfig['outputDriver']
  inputDevice: string | null
  outputDevice: string | null
  bufferSize: number
  /** 실제로 열린 백엔드(`EngineStats.backend` / `LoopbackReport.backend`) */
  backend: string
  sampleRate: number
}

export function loopbackDeviceKey(p: LoopbackDeviceKeyParts): string {
  return [p.inputDriver, p.outputDriver, p.backend, p.inputDevice ?? 'default', p.outputDevice ?? 'default', p.bufferSize, p.sampleRate].join('|')
}

/** 설정의 장치 id(null = 기본 장치)를 장치 목록의 현재 기본 장치로 푼다. 목록에 없으면 null */
export function resolveDeviceIds(
  config: Pick<EngineConfig, 'inputDriver' | 'outputDriver' | 'inputDevice' | 'outputDevice'>,
  devices: DeviceInfo[] | null,
): { inputDevice: string | null; outputDevice: string | null } {
  const pick = (id: string | null, isInput: boolean, driver: EngineConfig['inputDriver']) => {
    if (id) return id
    return devices?.find((d) => d.isInput === isInput && d.driver === driver && d.isDefault)?.id ?? null
  }
  return {
    inputDevice: pick(config.inputDevice, true, config.inputDriver),
    outputDevice: pick(config.outputDevice, false, config.outputDriver),
  }
}

export function isValidOffsetMs(v: unknown): v is number {
  return typeof v === 'number' && Number.isFinite(v) && v >= LOOPBACK_OFFSET_MIN_MS && v <= LOOPBACK_OFFSET_MAX_MS
}

/** 엔진이 기억하는 결과인지(판정 ok + 범위 안) — 같은 조건일 때만 앱도 저장한다 */
export function storableOffset(report: Pick<LoopbackReport, 'verdict' | 'offsetMs'>): number | null {
  return report.verdict === 'ok' && isValidOffsetMs(report.offsetMs) ? report.offsetMs : null
}

export function loadLoopbackOffset(storage: OffsetStorage | null, key: string): StoredLoopbackOffset | null {
  if (!storage) return null
  try {
    const raw = storage.getItem(KEY_PREFIX + key)
    if (!raw) return null
    const v = JSON.parse(raw) as unknown
    if (!v || typeof v !== 'object' || Array.isArray(v)) return null
    const { offsetMs, at } = v as Record<string, unknown>
    if (!isValidOffsetMs(offsetMs)) return null
    return { offsetMs, at: typeof at === 'string' ? at : '' }
  } catch {
    return null
  }
}

export function saveLoopbackOffset(storage: OffsetStorage | null, key: string, value: StoredLoopbackOffset): boolean {
  if (!storage || !isValidOffsetMs(value.offsetMs)) return false
  try {
    storage.setItem(KEY_PREFIX + key, JSON.stringify(value))
    return true
  } catch {
    return false
  }
}

export function clearLoopbackOffset(storage: OffsetStorage | null, key: string): void {
  if (!storage) return
  try {
    storage.removeItem(KEY_PREFIX + key)
  } catch {
    // 무시
  }
}

export function browserOffsetStorage(): OffsetStorage | null {
  try {
    return typeof window !== 'undefined' ? window.localStorage : null
  } catch {
    return null
  }
}

/** 엔진 커맨드 주입(테스트용) */
export interface LoopbackOffsetDeps {
  listDevices: () => Promise<DeviceInfo[]>
  stats: () => Promise<Pick<EngineStats, 'backend' | 'sampleRate'>>
  setOffset: (offsetMs: number | null) => Promise<unknown>
  storage: OffsetStorage | null
}

/** 실제 Tauri 엔진 + 브라우저 localStorage */
export function tauriLoopbackOffsetDeps(): LoopbackOffsetDeps {
  return { listDevices: engineListDevices, stats: engineStats, setOffset: engineSetLoopbackOffset, storage: browserOffsetStorage() }
}

async function listOrNull(deps: LoopbackOffsetDeps, config: EngineConfig): Promise<DeviceInfo[] | null> {
  if (config.inputDevice && config.outputDevice) return null
  try {
    return await deps.listDevices()
  } catch {
    return null
  }
}

/** 지금 실행 중인 엔진의 장치 구성 키. 백엔드를 모르면(정지·옛 엔진) null */
export async function currentDeviceKey(config: EngineConfig, deps: LoopbackOffsetDeps): Promise<string | null> {
  // 저장·재적용 모두 engine_stats 값으로 키를 만든다(같은 출처라 어긋나지 않게)
  const { backend, sampleRate } = await deps.stats()
  if (!backend || !sampleRate) return null
  const ids = resolveDeviceIds(config, await listOrNull(deps, config))
  return loopbackDeviceKey({ inputDriver: config.inputDriver, outputDriver: config.outputDriver, ...ids, bufferSize: config.bufferSize, backend, sampleRate })
}

/** 루프백 측정 결과 저장(ok + 범위 안일 때만). 저장한 키를 돌려준다 */
export async function rememberLoopbackOffset(config: EngineConfig, report: LoopbackReport, at: string, deps: LoopbackOffsetDeps): Promise<string | null> {
  const offsetMs = storableOffset(report)
  if (offsetMs === null) return null
  try {
    const key = await currentDeviceKey(config, deps)
    if (!key) return null
    return saveLoopbackOffset(deps.storage, key, { offsetMs, at }) ? key : null
  } catch {
    return null
  }
}

/** 지금 장치 구성의 저장값(측정 화면 표시용) */
export async function storedOffsetForCurrent(config: EngineConfig, deps: LoopbackOffsetDeps): Promise<{ key: string; stored: StoredLoopbackOffset | null } | null> {
  try {
    const key = await currentDeviceKey(config, deps)
    return key ? { key, stored: loadLoopbackOffset(deps.storage, key) } : null
  } catch {
    return null
  }
}

/**
 * 엔진 시작 직후 호출: 같은 장치 구성의 저장값이 있으면 엔진에 다시 넣는다. 넣은 값(ms)을 돌려준다.
 * 실패(옛 엔진·엔진 정지 경합)는 조용히 무시한다 — 보정이 안 되면 메트로놈 패널이 "보정 없음"으로 알린다.
 */
export async function reapplyLoopbackOffset(config: EngineConfig, deps: LoopbackOffsetDeps): Promise<number | null> {
  try {
    const key = await currentDeviceKey(config, deps)
    if (!key) return null
    const stored = loadLoopbackOffset(deps.storage, key)
    if (!stored) return null
    await deps.setOffset(stored.offsetMs)
    return stored.offsetMs
  } catch {
    return null
  }
}

/**
 * 엔진 시작 직후(테스트 모드·합주) 저장값 재적용 — 시작 흐름을 늦추지 않게 기다리지 않고 부른다.
 * 엔진이 같은 구성 값을 이미 기억하고 있어도 같은 값을 다시 넣을 뿐이라 무해하다.
 */
export function reapplyLoopbackOffsetInBackground(config: EngineConfig): void {
  try {
    void reapplyLoopbackOffset(config, tauriLoopbackOffsetDeps())
  } catch {
    // 무시
  }
}

/** 메트로놈 패널의 출력 지연 보정 표시 */
export interface OutputLatencyView {
  source: 'driver' | 'loopback' | null
  text: string
  /** 보정 없음: 루프백 측정 안내를 띄운다 */
  needsMeasure: boolean
}

export function outputLatencyView(m: Pick<MetronomeStats, 'outputLatencyMs' | 'outputLatencySource'> | null | undefined): OutputLatencyView | null {
  if (!m) return null
  const ms = m.outputLatencyMs
  const val = typeof ms === 'number' && Number.isFinite(ms) ? ` ${ms.toFixed(1)}ms` : ''
  if (m.outputLatencySource === 'driver') return { source: 'driver', text: `출력 지연 보정: 드라이버 보고${val}`, needsMeasure: false }
  if (m.outputLatencySource === 'loopback') return { source: 'loopback', text: `출력 지연 보정: 루프백 측정${val}`, needsMeasure: false }
  return { source: null, text: '출력 지연 보정: 없음', needsMeasure: true }
}

/** 메트로놈 패널 → 오디오 설정 모달의 지연 측정 절을 연다(Navigation이 듣는다) */
export const OPEN_LATENCY_MEASURE_EVENT = 'band:open-latency-measure'
export const LATENCY_MEASURE_SECTION_ID = 'latency-measure'
