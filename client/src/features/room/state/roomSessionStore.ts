/**
 * 합주실 세션 상태 스토어 (React 비의존). docs/frontend-architecture.md 4절, ADR D3.
 *
 * - 상태는 슬라이스 묶음(RoomState). `update`는 바뀐 슬라이스만 새 참조로 바꾸고 구독자에게 알린다.
 * - 화면이 쓰는 파생 값(나를 합친 연주자 목록·방송 담당·관람자·표시용 연결 상태 등)은 입력 슬라이스가 바뀔 때만
 *   다시 계산해 스냅샷(`derived`)에 둔다. 선택자는 고르기만 하므로 값이 그대로면 컴포넌트를 다시 그리지 않는다.
 * - 쓰는 쪽: RoomProvider(디스패처·명령), 브라우저 미디어 훅(관람 수신·웹 믹서·녹음), 시그널링 훅, 엔진 세션 미러.
 *   읽는 쪽: useRoomSelector(useSyncExternalStore).
 */
import type { EngineStats, PeerStats } from '../../../native/engineApi'
import type { MetronomeView } from '../../../native/metronomeLogic'
import type { PerformState } from '../../../native/performSession'
import { computeViewers, selectBroadcaster } from '../../../native/broadcastLogic'
import type { JoinError } from '../../rooms'
import type { ChatMessage } from './chatModel'
import { initialPerformRequestState, type InstrumentChangeRequest, type MyRequestStatus, type PerformRequestState } from './performRequestState'
import { mergeSelfPerformer, type PeerInstrument } from './roomState'
import { shallowEqual } from './shallowEqual'
import type { Recording } from './useRecording'
import type { SignalStatus } from './useSignaling'
import type { PeerNetworkStats, RtcStatus } from './useViewerAudio'
import type { MixSettings } from './useWebAudioMixer'

export interface ConnectionSlice {
  signalStatus: SignalStatus
  clientId: string | null
  /** 같은 방의 다른 참여자 */
  peers: string[]
  currentRoomId: string | null
  /** 서버가 입장을 확인함(room-state 수신) */
  roomJoined: boolean
  joinFeedback: string
  /** 비밀번호·정원 관련 입장 실패 */
  joinError: JoinError | null
  /** 서버가 판정한 내 방장 여부 (구 서버면 joinRoom 인자) */
  isRoomHost: boolean
  nickname: string
}

export interface RequestsSlice {
  perform: PerformRequestState
  pendingInstrumentChanges: InstrumentChangeRequest[]
  myInstrumentChangeStatus: MyRequestStatus
  myNewInstrument: string | null
  /** 방장: 마지막 방장 조작(관람자로 내리기)이 거절된 안내. 없으면 null */
  hostActionError: string | null
}

export interface ViewerSlice {
  remoteAudioMap: Record<string, MediaStream>
  status: RtcStatus
  error: string | null
  networkStats: Record<string, PeerNetworkStats>
}

export interface MixerSlice {
  mixSettingsMap: Record<string, MixSettings>
  masterVolume: number
  masterMuted: boolean
  masterPan: number
}

export interface LevelsSlice {
  audioLevels: Record<string, number>
  masterLevel: number
}

export interface RecordingSlice {
  isRecording: boolean
  recordings: Recording[]
  recordingDuration: number
}

export interface LocalSlice {
  /** 연주 시작·방송 실패 등 방 화면에 보여줄 안내 */
  localError: string | null
  /** 저장된 장치가 이 PC에 없어 이번 연주를 기본 장치로 시작했다는 알림(P2-27). 닫거나 연주를 멈추면 null */
  audioFallbackNotice: string | null
  isMonitoringEnabled: boolean
  monitorGain: number
  /** 내 클릭 볼륨·뮤트(내 귀에만) */
  metronomeVolume: number
  metronomeMuted: boolean
  broadcastViewerCount: number
  /** 서버가 내 채팅을 chat-login-required로 거절했다(익명 연결, P2-14c) — 입력창 대신 로그인 안내. room-state(재입장·재연결)에서 풀린다 */
  chatLoginRequired: boolean
}

export interface RoomState {
  connection: ConnectionSlice
  /** 원격 연주자만 (나는 엔진 세션 상태에서 합쳐서 파생) */
  roster: Record<string, PeerInstrument>
  /** 엔진 세션(PerformSession) 스냅샷 미러 */
  perform: PerformState
  chat: ChatMessage[]
  requests: RequestsSlice
  viewer: ViewerSlice
  mixer: MixerSlice
  levels: LevelsSlice
  recording: RecordingSlice
  local: LocalSlice
  miniPlayer: { active: boolean }
}

export interface RoomDerived {
  running: boolean
  starting: boolean
  /** 엔진만 다시 여는 중(장치 분실 등) — 연주자 화면은 유지 */
  restarting: boolean
  isPerforming: boolean
  /** 연주 세션이 idle이 아니면 그 악기 */
  myInstrument: string | null
  /** 원격 연주자 + (연주 중이면) 나 */
  peerInstruments: Record<string, PeerInstrument>
  broadcasterId: string | null
  isBroadcaster: boolean
  /** 방송 담당이 믹스를 보낼 관람자 */
  viewers: string[]
  metronome: MetronomeView | null
  relayError: string | null
  peerEngineStats: Record<string, PeerStats>
  /** 연주 중이면 엔진 레벨, 아니면 웹 믹서 레벨 */
  audioLevels: Record<string, number>
  masterLevel: number
  rtcStatus: RtcStatus
  rtcError: string | null
}

export type RoomSnapshot = RoomState & { readonly derived: RoomDerived }

export type RoomPatch = Partial<RoomState>

export const IDLE_PERFORM: PerformState = {
  phase: 'idle',
  instrument: null,
  sessionId: null,
  error: null,
  stats: null,
  inputMuted: false,
  relayError: null,
  metronome: null,
}

export function initialRoomState(over: Partial<RoomState> = {}): RoomState {
  return {
    connection: {
      signalStatus: 'idle',
      clientId: null,
      peers: [],
      currentRoomId: null,
      roomJoined: false,
      joinFeedback: '',
      joinError: null,
      isRoomHost: false,
      nickname: '',
    },
    roster: {},
    perform: IDLE_PERFORM,
    chat: [],
    requests: {
      perform: initialPerformRequestState,
      pendingInstrumentChanges: [],
      myInstrumentChangeStatus: 'none',
      myNewInstrument: null,
      hostActionError: null,
    },
    viewer: { remoteAudioMap: {}, status: 'idle', error: null, networkStats: {} },
    mixer: { mixSettingsMap: {}, masterVolume: 1, masterMuted: false, masterPan: 0 },
    levels: { audioLevels: {}, masterLevel: 0 },
    recording: { isRecording: false, recordings: [], recordingDuration: 0 },
    local: {
      localError: null,
      audioFallbackNotice: null,
      isMonitoringEnabled: false,
      monitorGain: 1,
      metronomeVolume: 1,
      metronomeMuted: false,
      broadcastViewerCount: 0,
      chatLoginRequired: false,
    },
    miniPlayer: { active: false },
    ...over,
  }
}

/** 마지막 입력(참조 비교)이 같으면 마지막 결과를 돌려준다 */
function memoLast<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  let last: { args: A; result: R } | null = null
  return (...args: A) => {
    if (last && last.args.length === args.length && last.args.every((a, i) => Object.is(a, args[i]))) return last.result
    const result = fn(...args)
    last = { args, result }
    return result
  }
}

/** 스토어마다 따로 갖는 파생 계산기. 입력이 같으면 같은 참조를 돌려준다 */
function createDeriver(): (s: RoomState) => RoomDerived {
  const peerInstrumentsOf = memoLast(
    (roster: Record<string, PeerInstrument>, id: string | null, instrument: string | null, running: boolean, nickname: string, isHost: boolean) =>
      mergeSelfPerformer(roster, { id, instrument, running, nickname, isHost }),
  )
  const broadcasterOf = memoLast((peerInstruments: Record<string, PeerInstrument>) =>
    selectBroadcaster(Object.values(peerInstruments).map((pi) => ({ id: pi.peerId, isHost: pi.isHost }))),
  )
  const performerIdsOf = memoLast((peerInstruments: Record<string, PeerInstrument>) => Object.keys(peerInstruments))
  const viewersOf = memoLast((peers: string[], performerIds: string[], clientId: string | null) => computeViewers(peers, performerIds, clientId))
  // 통계 틱(500ms)마다 엔진이 새 객체를 주지만 값이 그대로인 피어·레벨은 이전 참조를 유지한다.
  // 그래야 그 값을 고른 구독자가 무음·정지 상태에서 틱마다 다시 그리지 않는다
  let lastPeerStats: Record<string, PeerStats> = {}
  const peerStatsOf = memoLast((stats: EngineStats | null) => {
    const map: Record<string, PeerStats> = {}
    for (const ps of stats?.peers ?? []) {
      const prev = lastPeerStats[ps.peerId]
      map[ps.peerId] = prev && shallowEqual(prev, ps) ? prev : ps
    }
    lastPeerStats = shallowEqual(lastPeerStats, map) ? lastPeerStats : map
    return lastPeerStats
  })
  let lastEngineLevels: Record<string, number> = {}
  const levelsOf = memoLast((running: boolean, stats: EngineStats | null, webLevels: Record<string, number>, clientId: string | null) => {
    if (!running) return webLevels
    const levels: Record<string, number> = {}
    for (const ps of stats?.peers ?? []) levels[ps.peerId] = Math.min(100, Math.round(ps.level * 100))
    if (clientId && stats) levels[clientId] = Math.min(100, Math.round(stats.inputLevel * 100))
    lastEngineLevels = shallowEqual(lastEngineLevels, levels) ? lastEngineLevels : levels
    return lastEngineLevels
  })

  return (s) => {
    const { perform: perf, connection: conn } = s
    const running = perf.phase === 'running'
    const starting = perf.phase === 'starting'
    // 엔진만 다시 여는 중(장치 분실 등): 서버는 계속 연주자로 안다 → 내 연주자 항목·악기는 유지하고, 엔진에 묶인 것(방송·레벨)은 꺼 둔다 (#208 리뷰 M-1)
    const restarting = perf.phase === 'restarting'
    const peerInstruments = peerInstrumentsOf(s.roster, conn.clientId, perf.instrument, running || restarting, conn.nickname, conn.isRoomHost)
    const broadcasterId = broadcasterOf(peerInstruments)
    const performError = s.local.localError ?? perf.error
    return {
      running,
      starting,
      restarting,
      isPerforming: (running || restarting) && perf.instrument !== null,
      myInstrument: perf.phase === 'idle' ? null : perf.instrument,
      peerInstruments,
      broadcasterId,
      isBroadcaster: running && broadcasterId !== null && broadcasterId === conn.clientId,
      viewers: viewersOf(conn.peers, performerIdsOf(peerInstruments), conn.clientId),
      metronome: running ? (perf.metronome ?? null) : null,
      relayError: perf.relayError ?? null,
      peerEngineStats: peerStatsOf(perf.stats),
      audioLevels: levelsOf(running, perf.stats, s.levels.audioLevels, conn.clientId),
      masterLevel: running ? Math.min(100, Math.round((perf.stats?.outputLevel ?? 0) * 100)) : s.levels.masterLevel,
      // 시그널링이 끊겨 재개를 기다리는 동안 유지한 관람 수신은 '연결 중'으로 보인다(관람자 자신의 망이 끊긴 경우가 흔해 소리가
      // 실제로 오는지 모른다, #192 리뷰 L-1). 재연결되면 다시 수신 상태 그대로
      rtcStatus: running ? 'live' : starting || restarting ? 'connecting' : performError ? 'error'
        : s.viewer.status === 'live' && conn.currentRoomId !== null && conn.signalStatus !== 'connected' ? 'connecting' : s.viewer.status,
      rtcError: performError ?? s.viewer.error,
    }
  }
}

const SLICES = [
  'connection', 'roster', 'perform', 'chat', 'requests', 'viewer', 'mixer', 'levels', 'recording', 'local', 'miniPlayer',
] as const satisfies readonly (keyof RoomState)[]

/** 슬라이스 하나의 필드들을 바꾸는 패치 (값이 모두 같으면 null) */
export function patchSlice<K extends keyof RoomState>(s: RoomState, key: K, fields: Partial<RoomState[K]>): RoomPatch | null {
  const cur = s[key] as Record<string, unknown>
  const changed = Object.entries(fields).some(([k, v]) => !Object.is(cur[k], v))
  return changed ? ({ [key]: { ...cur, ...fields } } as RoomPatch) : null
}

export class RoomSessionStore {
  private state: RoomState
  private snapshot: RoomSnapshot
  private readonly derive = createDeriver()
  private readonly listeners = new Set<() => void>()

  constructor(init: RoomState = initialRoomState()) {
    this.state = init
    this.snapshot = { ...init, derived: this.derive(init) }
  }

  readonly getSnapshot = (): RoomSnapshot => this.snapshot

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  /** 바뀐 슬라이스만 교체한다. 함수면 현재 상태로 패치를 만든다. 실제로 바뀐 것이 없으면 알리지 않는다 */
  update(patch: RoomPatch | null | ((s: RoomState) => RoomPatch | null)): void {
    const p = typeof patch === 'function' ? patch(this.state) : patch
    if (!p) return
    let next: RoomState | null = null
    for (const key of SLICES) {
      if (!(key in p) || Object.is(p[key], this.state[key])) continue
      next ??= { ...this.state }
      ;(next as unknown as Record<string, unknown>)[key] = p[key]
    }
    if (!next) return
    this.state = next
    this.snapshot = { ...next, derived: this.derive(next) }
    this.listeners.forEach((l) => l())
  }

  /** 슬라이스 하나의 필드 일부를 바꾼다 */
  patch<K extends keyof RoomState>(key: K, fields: Partial<RoomState[K]> | ((cur: RoomState[K]) => Partial<RoomState[K]>)): void {
    this.update((s) => patchSlice(s, key, typeof fields === 'function' ? fields(s[key]) : fields))
  }

  /**
   * React의 setState처럼 쓰는 필드 setter (값 또는 이전 값 → 새 값). 미디어 훅의 useState를 그대로 옮기기 위한 것.
   */
  fieldSetter<K extends keyof RoomState, F extends keyof RoomState[K]>(key: K, field: F) {
    return (v: RoomState[K][F] | ((prev: RoomState[K][F]) => RoomState[K][F])): void => {
      this.update((s) => {
        const prev = s[key][field]
        const value = typeof v === 'function' ? (v as (p: RoomState[K][F]) => RoomState[K][F])(prev) : v
        return patchSlice(s, key, { [field]: value } as unknown as Partial<RoomState[K]>)
      })
    }
  }
}
