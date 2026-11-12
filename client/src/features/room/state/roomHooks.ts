/**
 * 합주실 상태를 읽는 얇은 선택자 훅들 (docs/frontend-architecture.md 4절, ADR D3).
 * 각 훅은 스토어에서 자기 조각만 구독하고(useRoomSelector), 명령은 RoomProvider가 한 번 만든 안정 객체에서 꺼낸다.
 * 조각이 그대로면 그 훅을 쓰는 컴포넌트는 다시 그리지 않는다.
 */
import { createContext, useContext, useMemo } from 'react'
import { isNativeApp, type EngineStats, type PeerStats } from '../../../native/engineApi'
import type { RoomSessionStore, RoomSnapshot } from './roomSessionStore'
import { selectReconnectBanner } from './reconnectBanner'
import { useRoomSelector } from './useRoomSelector'
import type { PeerNetworkStats } from './useViewerAudio'

export type { JoinError } from '../../rooms'
export type { ChatMessage } from './chatModel'
export type { InstrumentChangeRequest, MyRequestStatus, PerformRequest } from './performRequestState'
export type { PeerInstrument } from './roomState'
export type { ReconnectBanner, ReconnectBannerKind } from './reconnectBanner'
export type { Recording } from './useRecording'
export type { SignalStatus } from './useSignaling'
export type { PeerNetworkStats, RtcStatus } from './useViewerAudio'
export type { MixSettings } from './useWebAudioMixer'

/** RoomProvider가 한 번 만들어 끝까지 쓰는 명령들 (참조가 바뀌지 않는다) */
export interface RoomActions {
  /** password: 비밀번호 방이면 입력값(세션 동안 기억) */
  joinRoom: (roomId: string, isHost?: boolean, password?: string | null) => void
  /** 비밀번호를 입력해 다시 입장 시도 */
  submitRoomPassword: (password: string) => void
  /** 거부된 입장(정원 초과 등)을 다시 시도 */
  retryJoin: () => void
  leaveRoom: () => void
  /** 재연결 포기(gave-up)·다른 창에 밀려남(replaced) 뒤 새로고침 없이 시그널링을 다시 열고 같은 방에 새로 입장 */
  reenterRoom: () => void
  /** 방 밖에서 시그널링 연결이 없으면 다시 연다 (방에 들어가려는데 끊겨 있을 때) */
  ensureSignaling: () => void

  /** 엔진 시작 성공 후에만 악기를 설정하고 start-performing을 보낸다. 실패하면 false */
  startPerforming: (instrument: string) => Promise<boolean>
  stopPerforming: () => Promise<void>
  setMonitor: (enabled: boolean, gain: number) => void
  /** 내 입력 뮤트 (engine_set_input_muted) */
  toggleInputMuted: () => void

  /** 방장: 시작(또는 BPM·박자 변경)·정지. 엔진이 돌 때만 */
  setHostMetronome: (running: boolean, bpm: number, beatsPerBar: number) => Promise<boolean>
  /** 내 클릭 볼륨(0..1)·뮤트 — 내 귀에만, 방송·녹음에는 클릭이 없다 */
  setMetronomeVolume: (volume: number, muted: boolean) => void

  setMixVolume: (peerId: string, volume: number) => void
  setMixPan: (peerId: string, pan: number) => void
  setMixMuted: (peerId: string, muted: boolean) => void
  setMasterVolume: (volume: number) => void
  toggleMasterMute: () => void
  setMasterPan: (pan: number) => void
  resumeAllAudioContexts: () => void

  sendChatMessage: (message: string) => void

  dismissRequestNotice: () => void
  approveRequest: (oderId: string) => void
  rejectRequest: (oderId: string, reason?: string) => void
  requestPerform: (instrument: string) => void
  cancelRequest: () => void
  /** 거절 확인 후 기본 상태로 */
  dismissRejection: () => void
  approveInstrumentChange: (oderId: string) => void
  rejectInstrumentChange: (oderId: string, reason?: string) => void
  requestInstrumentChange: (newInstrument: string) => void
  cancelInstrumentChangeRequest: () => void

  startRecording: () => void
  stopRecording: () => void
  deleteRecording: (id: string) => void

  enterMiniPlayerMode: () => void
  exitMiniPlayerMode: () => void
  returnToRoom: () => string | null
  clearMiniPlayerMode: () => void
}

export interface RoomRuntime {
  store: RoomSessionStore
  actions: RoomActions
}

/** 값(store·actions)은 Provider 수명 동안 바뀌지 않는다 — 이 컨텍스트로는 리렌더가 퍼지지 않는다 */
export const RoomRuntimeContext = createContext<RoomRuntime | null>(null)

function useRoomRuntime(): RoomRuntime {
  const runtime = useContext(RoomRuntimeContext)
  if (!runtime) throw new Error('합주실 상태 훅은 RoomProvider 안에서만 쓸 수 있습니다')
  return runtime
}

// --- 선택자 (모듈 수준 순수 함수) ---

const selectConnection = (s: RoomSnapshot) => s.connection

const selectPerform = (s: RoomSnapshot) => ({
  canPerform: isNativeApp(),
  isPerforming: s.derived.isPerforming,
  performStarting: s.derived.starting,
  myInstrument: s.derived.myInstrument,
  peerInstruments: s.derived.peerInstruments,
  /** 마지막 engine_set_relay 실패 사유(없으면 null) */
  relayError: s.derived.relayError,
  isMonitoringEnabled: s.local.isMonitoringEnabled,
  monitorGain: s.local.monitorGain,
  inputMuted: s.perform.inputMuted,
  /** 관람자에게 믹스를 보내는 연주자 */
  broadcasterId: s.derived.broadcasterId,
  isBroadcaster: s.derived.isBroadcaster,
  broadcastViewerCount: s.local.broadcastViewerCount,
})

const selectMetronome = (s: RoomSnapshot) => ({
  /** 공유 메트로놈(P2-3). 꺼져 있으면 null. leaderId=null이면 내가 기준(방장) */
  metronome: s.derived.metronome,
  metronomeVolume: s.local.metronomeVolume,
  metronomeMuted: s.local.metronomeMuted,
})

export interface RoomTelemetry {
  engineStats: EngineStats | null
  /** 엔진 피어 통계 (peerId → PeerStats) */
  peerEngineStats: Record<string, PeerStats>
  audioLevels: Record<string, number>
  masterLevel: number
  /** 관람 수신 회선 통계 */
  peerNetworkStats: Record<string, PeerNetworkStats>
}

const telemetryOf = (s: RoomSnapshot): RoomTelemetry => ({
  engineStats: s.perform.stats,
  peerEngineStats: s.derived.peerEngineStats,
  audioLevels: s.derived.audioLevels,
  masterLevel: s.derived.masterLevel,
  peerNetworkStats: s.viewer.networkStats,
})

const selectMixer = (s: RoomSnapshot) => ({
  rtcStatus: s.derived.rtcStatus,
  rtcError: s.derived.rtcError,
  remoteAudioMap: s.viewer.remoteAudioMap,
  mixSettingsMap: s.mixer.mixSettingsMap,
  masterVolume: s.mixer.masterVolume,
  masterMuted: s.mixer.masterMuted,
  masterPan: s.mixer.masterPan,
})

const selectChat = (s: RoomSnapshot) => ({ chatMessages: s.chat })

const selectRequests = (s: RoomSnapshot) => {
  const r = s.requests
  return {
    pendingRequests: r.perform.pendingRequests,
    /** 방장: 새로 들어온 요청 알림 */
    requestNotice: r.perform.notice,
    myRequestStatus: r.perform.myStatus,
    myRequestInstrument: r.perform.myInstrument,
    /** 거절 사유 */
    myRejectReason: r.perform.myRejectReason,
    /** 요청 당시 방장 접속 여부 (모르면 null) */
    hostOnline: r.perform.hostOnline,
    pendingInstrumentChanges: r.pendingInstrumentChanges,
    myInstrumentChangeStatus: r.myInstrumentChangeStatus,
    myNewInstrument: r.myNewInstrument,
  }
}

const selectRecording = (s: RoomSnapshot) => s.recording

const selectRecordings = (s: RoomSnapshot) => s.recording.recordings

const selectMiniPlayer = (s: RoomSnapshot) => s.miniPlayer.active

// --- 훅 ---

/** 시그널링·입장: 연결 상태, clientId, 피어, 현재 방, 입장 결과, 방장 여부, 닉네임 */
export function useRoomConnection() {
  const { store, actions } = useRoomRuntime()
  const c = useRoomSelector(store, selectConnection)
  return {
    ...c,
    joinRoom: actions.joinRoom,
    submitRoomPassword: actions.submitRoomPassword,
    retryJoin: actions.retryJoin,
    leaveRoom: actions.leaveRoom,
    ensureSignaling: actions.ensureSignaling,
  }
}

/** 연주(데스크톱 앱 전용): 연주 상태, 연주자 목록, 모니터·입력 뮤트, 방송 담당 */
export function useRoomPerform() {
  const { store, actions } = useRoomRuntime()
  const p = useRoomSelector(store, selectPerform)
  return {
    ...p,
    startPerforming: actions.startPerforming,
    stopPerforming: actions.stopPerforming,
    setMonitor: actions.setMonitor,
    toggleInputMuted: actions.toggleInputMuted,
  }
}

/** 공유 메트로놈과 내 클릭 볼륨 */
export function useRoomMetronome() {
  const { store, actions } = useRoomRuntime()
  const m = useRoomSelector(store, selectMetronome)
  return { ...m, setHostMetronome: actions.setHostMetronome, setMetronomeVolume: actions.setMetronomeVolume }
}

/**
 * 주기적으로 바뀌는 측정값(엔진 통계 500ms, 레벨 미터 수십 ms, 회선 통계 2s).
 * 필요한 키만 넘기면 그 값이 바뀔 때만 다시 그린다. 키를 안 넘기면 전부.
 */
export function useRoomTelemetry<K extends keyof RoomTelemetry>(...keys: K[]): Pick<RoomTelemetry, K> {
  const { store } = useRoomRuntime()
  // 키 목록이 같으면 같은 선택자를 쓴다(useRoomSelector의 캐시 키). 키가 바뀌면 새 선택자로 다시 고른다
  const keyList = keys.join(',')
  const selector = useMemo(() => {
    const picked = keyList ? (keyList.split(',') as K[]) : []
    return (s: RoomSnapshot): Pick<RoomTelemetry, K> => {
      const all = telemetryOf(s)
      if (picked.length === 0) return all
      const out = {} as Pick<RoomTelemetry, K>
      for (const k of picked) out[k] = all[k]
      return out
    }
  }, [keyList])
  return useRoomSelector(store, selector)
}

/** 관람 수신·개인 믹서: 표시용 연결 상태, 수신 스트림, 믹서 설정·마스터 */
export function useRoomMixer() {
  const { store, actions } = useRoomRuntime()
  const m = useRoomSelector(store, selectMixer)
  return {
    ...m,
    setMixVolume: actions.setMixVolume,
    setMixPan: actions.setMixPan,
    setMixMuted: actions.setMixMuted,
    setMasterVolume: actions.setMasterVolume,
    toggleMasterMute: actions.toggleMasterMute,
    setMasterPan: actions.setMasterPan,
    resumeAllAudioContexts: actions.resumeAllAudioContexts,
  }
}

export function useRoomChat() {
  const { store, actions } = useRoomRuntime()
  const c = useRoomSelector(store, selectChat)
  return { ...c, sendChatMessage: actions.sendChatMessage }
}

/** 연주 참여·악기 변경 요청 (방장 승인 흐름) */
export function useRoomRequests() {
  const { store, actions } = useRoomRuntime()
  const r = useRoomSelector(store, selectRequests)
  return {
    ...r,
    dismissRequestNotice: actions.dismissRequestNotice,
    approveRequest: actions.approveRequest,
    rejectRequest: actions.rejectRequest,
    requestPerform: actions.requestPerform,
    cancelRequest: actions.cancelRequest,
    dismissRejection: actions.dismissRejection,
    approveInstrumentChange: actions.approveInstrumentChange,
    rejectInstrumentChange: actions.rejectInstrumentChange,
    requestInstrumentChange: actions.requestInstrumentChange,
    cancelInstrumentChangeRequest: actions.cancelInstrumentChangeRequest,
  }
}

/** 브라우저 녹음 목록과 삭제만. 녹음 시간(1s 틱)이 바뀌어도 다시 그리지 않는다 */
export function useRoomRecordings() {
  const { store, actions } = useRoomRuntime()
  const recordings = useRoomSelector(store, selectRecordings)
  return { recordings, deleteRecording: actions.deleteRecording }
}

/** 브라우저 녹음(MediaRecorder): 상태·목록·시간. 녹음 중에는 1초마다 다시 그린다 — 목록만 필요하면 useRoomRecordings */
export function useRoomRecording() {
  const { store, actions } = useRoomRuntime()
  const r = useRoomSelector(store, selectRecording)
  return { ...r, startRecording: actions.startRecording, stopRecording: actions.stopRecording, deleteRecording: actions.deleteRecording }
}

/**
 * 시그널링 재연결 안내 띠 (P1-15b). 연결 상태·방 id·엔진 단계에서 고른 값 두 개만 구독하므로 통계·레벨 틱에는 다시 그리지 않고,
 * 띠 상태가 바뀔 때만 다시 그린다.
 */
export function useReconnectBanner() {
  const { store, actions } = useRoomRuntime()
  const banner = useRoomSelector(store, selectReconnectBanner)
  return { ...banner, reenterRoom: actions.reenterRoom }
}

/** 미니 플레이어 (합주실에서 다른 화면으로 이동했을 때) */
export function useMiniPlayer() {
  const { store, actions } = useRoomRuntime()
  const isMiniPlayerMode = useRoomSelector(store, selectMiniPlayer)
  return {
    isMiniPlayerMode,
    enterMiniPlayerMode: actions.enterMiniPlayerMode,
    exitMiniPlayerMode: actions.exitMiniPlayerMode,
    returnToRoom: actions.returnToRoom,
    clearMiniPlayerMode: actions.clearMiniPlayerMode,
  }
}
