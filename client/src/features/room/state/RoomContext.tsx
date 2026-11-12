/**
 * 합주실 상태. 기능별 훅을 조합만 한다.
 *
 * - 시그널링 WS: useSignaling
 * - 연주(데스크톱 앱 전용): native/performSession(useNativeSession) (UDP 풀메시) + native/mixBridge (관람자 방송)
 * - 관람(웹/앱 공통): useViewerAudio (방송 담당 연주자의 믹스를 WebRTC 수신 전용으로) + useWebAudioMixer
 * - 채팅/참여요청/녹음: useChat, usePerformRequests, useRecording
 *
 * 결정사항(docs/overhaul/native-engine-design.md): 연주자는 앱 전용, 웹은 관람만. 앱 연주자끼리는 WebRTC를 쓰지 않는다.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useAuth } from '../../auth'
import { useAudioSettings } from '../../settings'
import { useSignaling, type SignalPayload, type SignalStatus } from './useSignaling'
import { useChat, type ChatMessage } from './useChat'
import {
  usePerformRequests,
  type InstrumentChangeRequest,
  type MyRequestStatus,
  type PerformRequest,
} from './usePerformRequests'
import { useViewerAudio, type PeerNetworkStats, type RtcStatus } from './useViewerAudio'
import { useWebAudioMixer, type MixSettings } from './useWebAudioMixer'
import { useRecording, type Recording, type RecordingSource } from './useRecording'
import { mergeSelfPerformer, withPeer, type PeerInstrument } from './roomState'
import { forgetRoomPassword, joinErrorFromSignal, joinErrorStopsSession, rememberRoomPassword, type JoinError } from '../../rooms'
import { useNativeSession } from '../../../native/useNativeSession'
import {
  browserMetronomePrefsStorage,
  clampMetronomeVolume,
  loadMetronomeGain,
  metronomeErrorFromCode,
  saveMetronomeGain,
  type MetronomeView,
} from '../../../native/metronomeLogic'
import { MixBroadcaster, MixSource } from '../../../native/mixBridge'
import { isEngineDescriptorErrorCode } from '../../../native/performSession'
import { computeViewers, selectBroadcaster } from '../../../native/sessionLogic'
import {
  MAX_PERFORMERS,
  engineErrorMessage,
  engineSetMasterGain,
  engineSetMetronomeGain,
  engineSetMonitor,
  isNativeApp,
  type EngineStats,
  type PeerStats,
} from '../../../native/engineApi'
import { appUpdater } from '../../../native/updater'

export type { JoinError }
export type { ChatMessage, InstrumentChangeRequest, MyRequestStatus, PerformRequest, PeerNetworkStats, RtcStatus, MixSettings, Recording, SignalStatus, PeerInstrument }

interface ParticipantPayload {
  oderId: string
  nickname?: string
  instrument?: string
  isPerforming?: boolean
  isHost?: boolean
}

function isParticipant(v: unknown): v is ParticipantPayload {
  return !!v && typeof v === 'object' && typeof (v as ParticipantPayload).oderId === 'string'
}

const BROADCAST_RESYNC_MS = 5000

type RoomContextType = {
  // 시그널링/방
  signalStatus: SignalStatus
  clientId: string | null
  peers: string[]
  /** password: 비밀번호 방이면 입력값(세션 동안 기억) */
  joinRoom: (roomId: string, isHost?: boolean, password?: string | null) => void
  joinFeedback: string
  /** 비밀번호 관련 입장 실패 (없으면 null) */
  joinError: JoinError | null
  /** 서버가 입장을 확인함(room-state 수신). 비밀번호 화면 등에서는 false */
  roomJoined: boolean
  /** 비밀번호를 입력해 다시 입장 시도 */
  submitRoomPassword: (password: string) => void
  /** 거부된 입장(정원 초과 등)을 다시 시도 */
  retryJoin: () => void
  currentRoomId: string | null
  leaveRoom: () => void
  /** 서버가 판정한 내 방장 여부 (구 서버면 joinRoom 인자) */
  isRoomHost: boolean

  // 연주 (데스크톱 앱 전용)
  canPerform: boolean
  isPerforming: boolean
  performStarting: boolean
  /** 엔진 시작 성공 후에만 악기를 설정하고 start-performing을 보낸다. 실패하면 false */
  startPerforming: (instrument: string) => Promise<boolean>
  stopPerforming: () => Promise<void>
  myInstrument: string | null
  peerInstruments: Record<string, PeerInstrument>
  engineStats: EngineStats | null
  /** 마지막 engine_set_relay 실패 사유(없으면 null) */
  relayError: string | null
  /** 엔진 피어 통계 (peerId → PeerStats) */
  peerEngineStats: Record<string, PeerStats>
  isMonitoringEnabled: boolean
  monitorGain: number
  setMonitor: (enabled: boolean, gain: number) => void
  /** 내 입력 뮤트 (engine_set_input_muted) */
  inputMuted: boolean
  toggleInputMuted: () => void
  /** 공유 메트로놈(P2-3). 꺼져 있으면 null. leaderId=null이면 내가 기준(방장) */
  metronome: MetronomeView | null
  /** 방장: 시작(또는 BPM·박자 변경)·정지. 엔진이 돌 때만 */
  setHostMetronome: (running: boolean, bpm: number, beatsPerBar: number) => Promise<boolean>
  /** 내 클릭 볼륨(0..1)·뮤트 — 내 귀에만, 방송·녹음에는 클릭이 없다 */
  metronomeVolume: number
  metronomeMuted: boolean
  setMetronomeVolume: (volume: number, muted: boolean) => void
  /** 관람자에게 믹스를 보내는 연주자 */
  broadcasterId: string | null
  isBroadcaster: boolean
  broadcastViewerCount: number

  // 관람 수신
  rtcStatus: RtcStatus
  rtcError: string | null
  remoteAudioMap: Record<string, MediaStream>
  peerNetworkStats: Record<string, PeerNetworkStats>

  // 개인 믹서
  mixSettingsMap: Record<string, MixSettings>
  setMixVolume: (peerId: string, volume: number) => void
  setMixPan: (peerId: string, pan: number) => void
  setMixMuted: (peerId: string, muted: boolean) => void
  masterVolume: number
  setMasterVolume: (volume: number) => void
  masterMuted: boolean
  toggleMasterMute: () => void
  masterPan: number
  setMasterPan: (pan: number) => void
  audioLevels: Record<string, number>
  masterLevel: number
  resumeAllAudioContexts: () => void

  // 채팅
  chatMessages: ChatMessage[]
  sendChatMessage: (message: string) => void
  nickname: string

  // 연주 참여 요청
  pendingRequests: PerformRequest[]
  /** 방장: 새로 들어온 요청 알림 */
  requestNotice: PerformRequest | null
  dismissRequestNotice: () => void
  approveRequest: (oderId: string) => void
  rejectRequest: (oderId: string, reason?: string) => void
  myRequestStatus: MyRequestStatus
  myRequestInstrument: string | null
  /** 거절 사유 */
  myRejectReason: string | null
  /** 요청 당시 방장 접속 여부 (모르면 null) */
  hostOnline: boolean | null
  requestPerform: (instrument: string) => void
  cancelRequest: () => void
  /** 거절 확인 후 기본 상태로 */
  dismissRejection: () => void

  // 악기 변경 요청
  pendingInstrumentChanges: InstrumentChangeRequest[]
  approveInstrumentChange: (oderId: string) => void
  rejectInstrumentChange: (oderId: string, reason?: string) => void
  myInstrumentChangeStatus: MyRequestStatus
  myNewInstrument: string | null
  requestInstrumentChange: (newInstrument: string) => void
  cancelInstrumentChangeRequest: () => void

  // 녹음
  isRecording: boolean
  recordings: Recording[]
  recordingDuration: number
  startRecording: () => void
  stopRecording: () => void
  deleteRecording: (id: string) => void

  // 미니 플레이어
  isMiniPlayerMode: boolean
  enterMiniPlayerMode: () => void
  exitMiniPlayerMode: () => void
  returnToRoom: () => string | null
  clearMiniPlayerMode: () => void
}

const RoomContext = createContext<RoomContextType | null>(null)

export function RoomProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth()
  const audioSettings = useAudioSettings()
  const audioSettingsRef = useRef(audioSettings)
  audioSettingsRef.current = audioSettings

  // 게스트 닉네임은 세션 동안 고정
  const [guestSuffix] = useState(() => Math.random().toString(36).slice(2, 6))
  const nickname: string = user?.user_metadata?.nickname || `Guest${guestSuffix}`
  const nicknameRef = useRef(nickname)
  nicknameRef.current = nickname

  // --- 방 상태 ---
  const [peers, setPeers] = useState<string[]>([])
  const [joinFeedback, setJoinFeedback] = useState('')
  const [joinError, setJoinError] = useState<JoinError | null>(null)
  const [roomJoined, setRoomJoined] = useState(false)
  /** 재연결 후 보낸 rejoin의 응답(room-state 또는 입장 거부)을 기다리는 중 (PR #23 M3) */
  const rejoiningRef = useRef(false)
  /** password-busy 자동 재시도 */
  const busyRetryRef = useRef<{ count: number; timer: ReturnType<typeof setTimeout> | null }>({ count: 0, timer: null })
  const clearBusyRetry = () => {
    if (busyRetryRef.current.timer) clearTimeout(busyRetryRef.current.timer)
    busyRetryRef.current = { count: 0, timer: null }
  }
  /** 비밀번호 방 입장용 (재연결 시 다시 보낸다) */
  const roomPasswordRef = useRef<string | null>(null)
  const [currentRoomId, setCurrentRoomId] = useState<string | null>(null)
  /** 원격 연주자만 (나는 연주 세션 상태에서 합쳐서 노출) */
  const [remotePerformers, setRemotePerformers] = useState<Record<string, PeerInstrument>>({})
  const [isRoomHost, setIsRoomHost] = useState(false)
  const [localError, setLocalError] = useState<string | null>(null)
  const [isMonitoringEnabled, setIsMonitoringEnabled] = useState(false)
  const [monitorGain, setMonitorGain] = useState(1)
  // 클릭 볼륨·뮤트는 사용자별로 로컬에 저장해 방을 다시 들어와도 유지한다(PR #36 L5)
  const [initialMetronomeGain] = useState(() => loadMetronomeGain(browserMetronomePrefsStorage(), user?.id))
  const [metronomeVolume, setMetronomeVolumeState] = useState(initialMetronomeGain.volume)
  const [metronomeMuted, setMetronomeMutedState] = useState(initialMetronomeGain.muted)
  const metronomeUserIdRef = useRef(user?.id)
  metronomeUserIdRef.current = user?.id
  const [broadcastViewerCount, setBroadcastViewerCount] = useState(0)
  const [isMiniPlayerMode, setIsMiniPlayerMode] = useState(false)

  const currentRoomIdRef = useRef<string | null>(null)
  const myIsHostRef = useRef(false)
  const remotePerformersRef = useRef(remotePerformers)
  remotePerformersRef.current = remotePerformers
  const isReturningToRoomRef = useRef(false)

  // 핸들러는 ref로 넘겨 WS 재연결 없이 최신 클로저를 쓴다
  const onMessageRef = useRef<(p: SignalPayload) => void>(() => {})
  const onReadyRef = useRef<(id: string, resumed: boolean) => void>(() => {})
  const onClosedRef = useRef<() => void>(() => {})

  const buildJoinMessage = useCallback((isRejoin: boolean): SignalPayload | null => {
    const roomId = currentRoomIdRef.current
    if (!roomId) return null
    // isHost/userId는 구 서버 호환용. 새 서버는 무시하고 auth 토큰과 DB로 판정한다.
    return {
      type: 'join',
      roomId,
      nickname: nicknameRef.current,
      isHost: myIsHostRef.current,
      userId: user?.id ?? null,
      ...(roomPasswordRef.current ? { password: roomPasswordRef.current } : {}),
      ...(isRejoin ? { isRejoin: true } : {}),
    }
  }, [user?.id])
  const buildJoinMessageRef = useRef(buildJoinMessage)
  buildJoinMessageRef.current = buildJoinMessage

  const signaling = useSignaling({
    onReady: (id, resumed) => onReadyRef.current(id, resumed),
    onMessage: (p) => onMessageRef.current(p),
    onClosed: () => onClosedRef.current(),
    isInRoom: () => currentRoomIdRef.current !== null,
    getJoinMessage: () => buildJoinMessageRef.current(false),
  })
  const { send, clientIdRef, isOpen } = signaling
  const clientId = signaling.clientId
  const getSelfId = useCallback(() => clientIdRef.current, [clientIdRef])

  // --- 연주 세션 (엔진) ---
  const { session, state: perf } = useNativeSession({
    send,
    getSelfId: () => clientIdRef.current,
    getRoomId: () => currentRoomIdRef.current,
    getOtherPerformers: () => Object.keys(remotePerformersRef.current).filter((id) => id !== clientIdRef.current),
  })
  const running = perf.phase === 'running'
  const starting = perf.phase === 'starting'

  // 연주 세션이 돌면 설정 화면의 테스트를 막는다
  const { setEngineInUse } = audioSettings
  useEffect(() => {
    setEngineInUse(perf.phase !== 'idle')
  }, [perf.phase, setEngineInUse])

  // --- 기능 훅 ---
  const chat = useChat({ send, canSend: isOpen, getSelfId, nickname })

  // 승인되면 앱은 바로 엔진을 시작한다 (startPerforming은 아래에서 정의되므로 ref로)
  const startPerformingRef = useRef<(instrument: string) => Promise<boolean>>(async () => false)
  const requests = usePerformRequests({
    send,
    canSend: isOpen,
    onMyRequestApproved: (instrument) => {
      if (!instrument || !isNativeApp() || session.getState().phase !== 'idle') return
      void startPerformingRef.current(instrument)
    },
    onMyInstrumentChanged: (instrument) => session.updateInstrument(instrument),
    onPeerInstrumentChanged: (peerId, instrument) => {
      setRemotePerformers((prev) => (prev[peerId] ? { ...prev, [peerId]: { ...prev[peerId], instrument } } : prev))
    },
  })

  const viewer = useViewerAudio({
    send,
    isAllowedSender: (peerId) => peerId in remotePerformersRef.current,
  })
  const mixer = useWebAudioMixer(viewer.remoteAudioMap)

  const mixSourceRef = useRef<MixSource | null>(null)
  if (!mixSourceRef.current) mixSourceRef.current = new MixSource()
  const broadcasterRef = useRef<MixBroadcaster | null>(null)
  if (!broadcasterRef.current) {
    broadcasterRef.current = new MixBroadcaster(mixSourceRef.current, send, setBroadcastViewerCount)
  }

  /** 녹음이 엔진 믹스를 쓰는 중인지 (엔진 정지 시 녹음도 멈춘다: M4) */
  const recordingFromEngineRef = useRef(false)
  const resolveRecordingSource = useCallback(async (): Promise<RecordingSource | null> => {
    if (session.getState().phase === 'running') {
      const lease = await mixSourceRef.current!.acquire()
      recordingFromEngineRef.current = true
      return {
        stream: lease.stream,
        release: () => {
          recordingFromEngineRef.current = false
          lease.release()
        },
      }
    }
    recordingFromEngineRef.current = false
    const stream = mixer.getRecordingStream()
    return stream ? { stream } : null
  }, [mixer, session])
  const recording = useRecording(resolveRecordingSource)

  // 엔진이 멈추면(정지/거부/재시작) 믹스 구독을 무효화하고 그 믹스로 녹음 중이면 녹음을 끝낸다 (M4)
  const { stopRecording } = recording
  useEffect(() => {
    if (running) return
    if (recordingFromEngineRef.current) stopRecording()
    mixSourceRef.current!.invalidate()
  }, [running, stopRecording])

  // --- 연주 시작/중단 ---
  const startPerforming = useCallback(async (instrument: string): Promise<boolean> => {
    setLocalError(null)
    session.clearError()
    if (!isNativeApp()) {
      setLocalError('연주는 데스크톱 앱에서만 할 수 있습니다.')
      return false
    }
    const selfId = clientIdRef.current
    if (!selfId || !currentRoomIdRef.current) {
      setLocalError('합주실에 연결되지 않았습니다.')
      return false
    }
    const others = Object.keys(remotePerformersRef.current).filter((id) => id !== selfId)
    if (others.length >= MAX_PERFORMERS) {
      setLocalError(`연주자는 최대 ${MAX_PERFORMERS}명까지 참여할 수 있습니다.`)
      return false
    }
    const settings = audioSettingsRef.current
    if (settings.issues.length > 0) {
      setLocalError(`오디오 설정 오류: ${settings.issues[0].message}`)
      return false
    }
    // 테스트 모니터가 시작 중이어도 끝난 뒤 정지된다 (L8)
    await settings.stopTest()
    const ok = await session.start({
      instrument,
      config: settings.config,
      nickname: nicknameRef.current,
      isHost: myIsHostRef.current,
    })
    if (ok) {
      // 연주자는 관람 수신을 하지 않는다
      viewer.closeAll()
      requests.resetMyRequest()
    }
    return ok
  }, [clientIdRef, session, viewer, requests])
  startPerformingRef.current = startPerforming

  const stopPerforming = useCallback(async () => {
    broadcasterRef.current!.stop()
    await session.stop({ notify: true })
  }, [session])

  // --- 시그널링 이벤트 ---
  onReadyRef.current = (id, resumed) => {
    // 재연결: 이전 방에 다시 입장 (resumed면 서버는 이미 방에 있는 것으로 보고 상태만 다시 보낸다)
    const join = buildJoinMessageRef.current(true)
    rejoiningRef.current = !!join
    if (join) send(join)
    // resumed && 같은 id → 엔진 유지 + descriptor 재전송, 아니면 새 id로 엔진 재시작
    void session.welcome(id, resumed)
  }

  onClosedRef.current = () => {
    setPeers([])
    viewer.closeAll()
    // 엔진(UDP 메시)은 유지한다. 재연결/세션 재개 여부는 onReady에서 결정 (M6)
    session.connectionLost()
  }

  const upsertPerformer = (p: ParticipantPayload) => {
    if (!p.instrument || p.oderId === clientIdRef.current) return
    setRemotePerformers((prev) => ({
      ...prev,
      [p.oderId]: {
        peerId: p.oderId,
        instrument: p.instrument!,
        nickname: p.nickname || `User ${p.oderId.slice(0, 4)}`,
        isHost: p.isHost === true,
      },
    }))
  }

  const removePerformer = (peerId: string) => {
    setRemotePerformers((prev) => {
      if (!prev[peerId]) return prev
      const next = { ...prev }
      delete next[peerId]
      return next
    })
  }

  const ensurePeer = (peerId: string | null) => {
    if (!peerId) return
    setPeers((prev) => withPeer(prev, peerId, clientIdRef.current))
  }

  onMessageRef.current = (p: SignalPayload) => {
    const self = clientIdRef.current
    const from = typeof p.from === 'string' ? p.from : null
    if (from && from === self) return

    // native-descriptor / native-leave / relay-token / 연주 거부 에러(perform-not-approved, room-full-performers,
    // login-required) / ssrc-conflict·key-conflict(엔진 재시작만)·descriptor-invalid(엔진 정지·안내) — 연주 거부가 아니므로 방송은 유지
    if (session.handleSignal(p)) {
      // 연주 거부 에러만 방송을 멈춘다(descriptor 거부는 엔진만, 메트로놈 에러는 메트로놈만)
      if (p.type === 'error' && !isEngineDescriptorErrorCode(p.code) && !metronomeErrorFromCode(p.code)) broadcasterRef.current!.stop()
      return
    }
    if (chat.handleChatSignal(p)) return
    if (requests.handleRequestSignal(p)) return

    // 비밀번호 방 입장 실패
    const je = joinErrorFromSignal(p)
    if (je) {
      const roomId = currentRoomIdRef.current
      if (je.roomId && je.roomId !== roomId) return
      if (roomId && je.code === 'password-invalid') forgetRoomPassword(roomId)
      if (je.code === 'password-required' || je.code === 'password-invalid') roomPasswordRef.current = null
      const rejoining = rejoiningRef.current
      if (je.code !== 'password-busy') rejoiningRef.current = false
      // 방 밖으로 밀려났다: 엔진·방송을 멈춘다 (서버는 방 밖 연결의 연주 메시지를 거부한다)
      if (joinErrorStopsSession(je, rejoining)) {
        broadcasterRef.current!.stop()
        void session.stop({ notify: false })
      }
      setJoinError(rejoining ? { ...je, afterReconnect: true } : je)
      setJoinFeedback('')
      setRoomJoined(false)
      // 서버 혼잡/방 준비 중: 같은 비밀번호로 몇 번 자동 재시도
      if (je.code === 'password-busy' && roomId && busyRetryRef.current.count < 5) {
        const retry = busyRetryRef.current
        retry.count++
        if (retry.timer) clearTimeout(retry.timer)
        retry.timer = setTimeout(() => {
          retry.timer = null
          if (currentRoomIdRef.current !== roomId) return
          const join = buildJoinMessageRef.current(false)
          if (join && isOpen()) send(join)
        }, Math.min(Math.max(je.retryAfterMs ?? 1000, 500), 10_000))
      }
      return
    }

    switch (p.type) {
      case 'room-state': {
        rejoiningRef.current = false
        const peerIds = Array.isArray(p.peerIds) ? (p.peerIds as unknown[]).filter((x): x is string => typeof x === 'string' && x !== self) : []
        const participants = Array.isArray(p.participants) ? (p.participants as unknown[]).filter(isParticipant) : []
        if (typeof p.isHost === 'boolean') {
          myIsHostRef.current = p.isHost
          setIsRoomHost(p.isHost)
        }
        setPeers(peerIds)
        setJoinError(null)
        setRoomJoined(true)
        clearBusyRetry()
        if (currentRoomIdRef.current && roomPasswordRef.current) rememberRoomPassword(currentRoomIdRef.current, roomPasswordRef.current)
        setJoinFeedback(`룸 입장 완료 · 동시 연결 ${peerIds.length + 1}명`)
        const map: Record<string, PeerInstrument> = {}
        for (const part of participants) {
          if (part.isPerforming && part.instrument && part.oderId !== self) {
            map[part.oderId] = {
              peerId: part.oderId,
              instrument: part.instrument,
              nickname: part.nickname || `User ${part.oderId.slice(0, 4)}`,
              isHost: part.isHost === true,
            }
          }
        }
        setRemotePerformers(map)
        return
      }

      case 'participant-joined': {
        const part = p.participant
        if (!isParticipant(part)) return
        ensurePeer(part.oderId)
        if (part.isPerforming && part.instrument) {
          upsertPerformer(part)
          session.performerAppeared(part.oderId)
        }
        return
      }

      case 'participant-left': {
        if (typeof p.oderId !== 'string') return
        const id = p.oderId
        setPeers((prev) => prev.filter((x) => x !== id))
        removePerformer(id)
        session.peerGone(id)
        viewer.closeFrom(id)
        requests.dropPeer(id)
        return
      }

      case 'performer-updated': {
        if (typeof p.oderId !== 'string') return
        const id = p.oderId
        ensurePeer(id)
        if (p.isPerforming === true && typeof p.instrument === 'string') {
          upsertPerformer({ oderId: id, nickname: p.nickname as string | undefined, instrument: p.instrument, isHost: p.isHost === true })
          session.performerAppeared(id)
        } else {
          removePerformer(id)
          session.peerGone(id)
          viewer.closeFrom(id)
        }
        return
      }

      case 'offer':
        // 관람자만 받는다. 앱 연주자는 엔진으로 듣는다.
        ensurePeer(from)
        if (from && p.offer && session.getState().phase === 'idle') {
          void viewer.handleOffer(from, p.offer as RTCSessionDescriptionInit)
        }
        return

      case 'answer':
        ensurePeer(from)
        if (from && p.answer) void broadcasterRef.current!.handleAnswer(from, p.answer as RTCSessionDescriptionInit)
        return

      case 'ice-candidate': {
        if (!from || !p.candidate) return
        ensurePeer(from)
        const candidate = p.candidate as RTCIceCandidateInit
        if (broadcasterRef.current!.hasPeer(from)) void broadcasterRef.current!.handleCandidate(from, candidate)
        else void viewer.handleCandidate(from, candidate)
        return
      }

      case 'error':
        console.warn('[WS] 서버 에러:', p.code, p.message)
        return
    }
  }

  // --- 방 입장/퇴장 ---
  const joinRoom = useCallback((roomId: string, isHost?: boolean, password?: string | null) => {
    if (!isOpen()) {
      setJoinFeedback('시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.')
      return
    }
    currentRoomIdRef.current = roomId
    roomPasswordRef.current = password || null
    rejoiningRef.current = false
    setJoinError(null)
    setRoomJoined(false)
    clearBusyRetry()
    myIsHostRef.current = isHost === true
    setIsRoomHost(isHost === true)
    setCurrentRoomId(roomId)
    const join = buildJoinMessageRef.current(false)
    if (join) send(join)
    setJoinFeedback('룸 입장 시도 중...')
  }, [isOpen, send])

  const submitRoomPassword = useCallback((password: string) => {
    const roomId = currentRoomIdRef.current
    if (!roomId || !password) return
    roomPasswordRef.current = password
    setJoinError(null)
    const join = buildJoinMessageRef.current(false)
    if (join && isOpen()) send(join)
    setJoinFeedback('룸 입장 시도 중...')
  }, [isOpen, send])

  /** 정원 초과 등으로 거부된 입장을 같은 조건으로 다시 시도 (PR #23 L3) */
  const retryJoin = useCallback(() => {
    if (!currentRoomIdRef.current) return
    if (!isOpen()) {
      setJoinFeedback('시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.')
      return
    }
    setJoinError(null)
    const join = buildJoinMessageRef.current(false)
    if (join) send(join)
    setJoinFeedback('룸 입장 시도 중...')
  }, [isOpen, send])

  const leaveRoom = useCallback(() => {
    send({ type: 'leave' })
    roomPasswordRef.current = null
    setJoinError(null)
    setRoomJoined(false)
    clearBusyRetry()
    // 방 id를 먼저 비워야 시작 중인 엔진이 완료 시 "퇴장함"을 보고 스스로 내려간다 (H2)
    currentRoomIdRef.current = null
    broadcasterRef.current!.stop()
    // leave가 participant-left로 전파되므로 native-leave/stop-performing은 따로 보내지 않는다
    void session.stop({ notify: false })
    viewer.closeAll()
    mixer.reset()
    recording.stopRecording()
    chat.clearChat()
    requests.resetAll()
    myIsHostRef.current = false
    setIsRoomHost(false)
    setCurrentRoomId(null)
    setPeers([])
    setRemotePerformers({})
    setJoinFeedback('')
    setLocalError(null)
    setIsMiniPlayerMode(false)
  }, [send, session, viewer, mixer, recording, chat, requests])

  // 방에 있는 동안(엔진 시작 전 대기 포함)은 앱 업데이트 설치를 보류한다 (native/updater)
  useEffect(() => {
    appUpdater.setInRoom(currentRoomId !== null)
  }, [currentRoomId])

  // --- 연주자 목록 (나 포함) ---
  const peerInstruments = useMemo(() => mergeSelfPerformer(remotePerformers, {
    id: clientId,
    instrument: perf.instrument,
    running,
    nickname,
    isHost: isRoomHost,
  }), [remotePerformers, clientId, perf.instrument, running, nickname, isRoomHost])

  // --- 방송 담당 (관람자 브릿지) ---
  const performerIds = useMemo(() => Object.keys(peerInstruments), [peerInstruments])
  const broadcasterId = useMemo(() => selectBroadcaster(
    Object.values(peerInstruments).map((pi) => ({ id: pi.peerId, isHost: pi.isHost })),
  ), [peerInstruments])
  const viewers = useMemo(() => computeViewers(peers, performerIds, clientId), [peers, performerIds, clientId])
  const viewersRef = useRef(viewers)
  viewersRef.current = viewers
  const isBroadcaster = running && broadcasterId !== null && broadcasterId === clientId

  useEffect(() => {
    const b = broadcasterRef.current!
    if (isBroadcaster) {
      b.start(viewersRef.current).catch((err) => {
        console.error('[MixBridge] 방송 시작 실패:', err)
        setLocalError(`관람자 방송 시작 실패: ${engineErrorMessage(err)}`)
      })
    } else {
      b.stop()
    }
  }, [isBroadcaster])

  useEffect(() => {
    if (isBroadcaster) broadcasterRef.current!.syncViewers(viewers)
  }, [isBroadcaster, viewers])

  // 실패/시간 초과한 관람자 연결 재시도
  useEffect(() => {
    if (!isBroadcaster) return
    const timer = setInterval(() => broadcasterRef.current!.syncViewers(viewersRef.current), BROADCAST_RESYNC_MS)
    return () => clearInterval(timer)
  }, [isBroadcaster])

  useEffect(() => () => {
    broadcasterRef.current?.stop()
  }, [])

  // --- 엔진 믹서 적용 (연주 중) ---
  const { mixSettingsMap, masterVolume, masterMuted } = mixer
  const appliedGainRef = useRef<Record<string, string>>({})
  const enginePeerIds = useMemo(() => (perf.stats?.peers ?? []).map((p) => p.peerId).join(','), [perf.stats])
  useEffect(() => {
    if (!running) {
      appliedGainRef.current = {}
      return
    }
    const enginePeers = new Set(enginePeerIds.split(',').filter(Boolean))
    for (const [peerId, s] of Object.entries(mixSettingsMap)) {
      if (!enginePeers.has(peerId)) continue
      const key = `${s.volume}:${s.muted}:${s.pan}`
      if (appliedGainRef.current[peerId] === key) continue
      appliedGainRef.current[peerId] = key
      session.setPeerGain(peerId, s.volume, s.muted, s.pan)
    }
    // 엔진에서 빠진 피어는 다시 들어오면 재적용
    for (const peerId of Object.keys(appliedGainRef.current)) {
      if (!enginePeers.has(peerId)) delete appliedGainRef.current[peerId]
    }
  }, [running, session, mixSettingsMap, enginePeerIds])

  useEffect(() => {
    if (!running) return
    engineSetMasterGain(masterMuted ? 0 : masterVolume).catch(() => {})
  }, [running, masterVolume, masterMuted])

  // 엔진이 (재)시작되면 모니터 설정 적용
  const monitorRef = useRef({ enabled: isMonitoringEnabled, gain: monitorGain })
  monitorRef.current = { enabled: isMonitoringEnabled, gain: monitorGain }
  useEffect(() => {
    if (!running) return
    engineSetMonitor(monitorRef.current.enabled, monitorRef.current.gain).catch(() => {})
  }, [running])

  const setMonitor = useCallback((enabled: boolean, gain: number) => {
    const g = Math.max(0, Math.min(1, gain))
    setIsMonitoringEnabled(enabled)
    setMonitorGain(g)
    if (session.getState().phase === 'running') {
      engineSetMonitor(enabled, g).catch((err) => console.warn('[Native] 모니터 설정 실패:', engineErrorMessage(err)))
    }
  }, [session])

  const toggleInputMuted = useCallback(() => {
    session.setInputMuted(!session.getState().inputMuted)
  }, [session])

  const setHostMetronome = useCallback(
    (on: boolean, bpm: number, beatsPerBar: number) => session.setHostMetronome(on, bpm, beatsPerBar),
    [session],
  )

  /** 클릭 볼륨을 화면 상태와 엔진에 적용한다. 엔진 제어값은 엔진 재시작 간에도 유지되므로 연주 중이 아니어도 보낸다(앱에서만) */
  const applyMetronomeGain = useCallback((volume: number, muted: boolean) => {
    setMetronomeVolumeState(volume)
    setMetronomeMutedState(muted)
    if (isNativeApp()) {
      engineSetMetronomeGain(volume, muted).catch((err) => console.warn('[Native] 메트로놈 볼륨 실패:', engineErrorMessage(err)))
    }
  }, [])

  const setMetronomeVolume = useCallback((volume: number, muted: boolean) => {
    const v = clampMetronomeVolume(volume)
    applyMetronomeGain(v, muted)
    saveMetronomeGain(browserMetronomePrefsStorage(), metronomeUserIdRef.current, { volume: v, muted })
  }, [applyMetronomeGain])

  // 시작할 때(그리고 로그인 사용자가 바뀌면) 저장값을 화면·엔진에 한 번 맞춘다. 앱을 다시 띄워도 엔진(MixControls)과
  // 화면이 어긋나지 않게 한다(PR #36 L5)
  useEffect(() => {
    const prefs = loadMetronomeGain(browserMetronomePrefsStorage(), user?.id)
    applyMetronomeGain(prefs.volume, prefs.muted)
  }, [user?.id, applyMetronomeGain])

  // --- 표시용 파생 값 ---
  const peerEngineStats = useMemo(() => {
    const map: Record<string, PeerStats> = {}
    for (const ps of perf.stats?.peers ?? []) map[ps.peerId] = ps
    return map
  }, [perf.stats])

  const audioLevels = useMemo(() => {
    if (!running) return mixer.audioLevels
    const levels: Record<string, number> = {}
    for (const ps of perf.stats?.peers ?? []) levels[ps.peerId] = Math.min(100, Math.round(ps.level * 100))
    if (clientId && perf.stats) levels[clientId] = Math.min(100, Math.round(perf.stats.inputLevel * 100))
    return levels
  }, [running, perf.stats, mixer.audioLevels, clientId])

  const masterLevel = running
    ? Math.min(100, Math.round((perf.stats?.outputLevel ?? 0) * 100))
    : mixer.masterLevel

  const performError = localError ?? perf.error
  const rtcStatus: RtcStatus = running ? 'live'
    : starting ? 'connecting'
      : performError ? 'error'
        : viewer.status
  const rtcError = performError ?? viewer.error

  const resumeAllAudioContexts = useCallback(() => {
    mixer.resume()
    mixSourceRef.current?.resume()
  }, [mixer])

  // --- 미니 플레이어 ---
  const enterMiniPlayerMode = useCallback(() => {
    if (isReturningToRoomRef.current) {
      isReturningToRoomRef.current = false
      return
    }
    if (!currentRoomIdRef.current) return
    setIsMiniPlayerMode(true)
  }, [])

  const exitMiniPlayerMode = useCallback(() => {
    setIsMiniPlayerMode(false)
    leaveRoom()
  }, [leaveRoom])

  const returnToRoom = useCallback((): string | null => {
    const roomId = currentRoomIdRef.current
    if (!roomId) return null
    isReturningToRoomRef.current = true
    setIsMiniPlayerMode(false)
    return roomId
  }, [])

  const clearMiniPlayerMode = useCallback(() => {
    setIsMiniPlayerMode((was) => {
      if (was) isReturningToRoomRef.current = true
      return false
    })
  }, [])

  const value: RoomContextType = {
    signalStatus: signaling.status,
    clientId,
    peers,
    joinRoom,
    joinFeedback,
    joinError,
    roomJoined,
    submitRoomPassword,
    retryJoin,
    currentRoomId,
    leaveRoom,
    isRoomHost,

    canPerform: isNativeApp(),
    isPerforming: running && perf.instrument !== null,
    performStarting: starting,
    startPerforming,
    stopPerforming,
    myInstrument: perf.phase === 'idle' ? null : perf.instrument,
    peerInstruments,
    engineStats: perf.stats,
    relayError: perf.relayError ?? null,
    peerEngineStats,
    isMonitoringEnabled,
    monitorGain,
    setMonitor,
    inputMuted: perf.inputMuted,
    toggleInputMuted,
    metronome: running ? (perf.metronome ?? null) : null,
    setHostMetronome,
    metronomeVolume,
    metronomeMuted,
    setMetronomeVolume,
    broadcasterId,
    isBroadcaster,
    broadcastViewerCount,

    rtcStatus,
    rtcError,
    remoteAudioMap: viewer.remoteAudioMap,
    peerNetworkStats: viewer.networkStats,

    mixSettingsMap,
    setMixVolume: mixer.setMixVolume,
    setMixPan: mixer.setMixPan,
    setMixMuted: mixer.setMixMuted,
    masterVolume,
    setMasterVolume: mixer.setMasterVolume,
    masterMuted,
    toggleMasterMute: mixer.toggleMasterMute,
    masterPan: mixer.masterPan,
    setMasterPan: mixer.setMasterPan,
    audioLevels,
    masterLevel,
    resumeAllAudioContexts,

    chatMessages: chat.chatMessages,
    sendChatMessage: chat.sendChatMessage,
    nickname,

    pendingRequests: requests.pendingRequests,
    requestNotice: requests.requestNotice,
    dismissRequestNotice: requests.dismissRequestNotice,
    approveRequest: requests.approveRequest,
    rejectRequest: requests.rejectRequest,
    myRequestStatus: requests.myRequestStatus,
    myRequestInstrument: requests.myRequestInstrument,
    myRejectReason: requests.myRejectReason,
    hostOnline: requests.hostOnline,
    requestPerform: requests.requestPerform,
    cancelRequest: requests.cancelRequest,
    dismissRejection: requests.dismissRejection,

    pendingInstrumentChanges: requests.pendingInstrumentChanges,
    approveInstrumentChange: requests.approveInstrumentChange,
    rejectInstrumentChange: requests.rejectInstrumentChange,
    myInstrumentChangeStatus: requests.myInstrumentChangeStatus,
    myNewInstrument: requests.myNewInstrument,
    requestInstrumentChange: requests.requestInstrumentChange,
    cancelInstrumentChangeRequest: requests.cancelInstrumentChangeRequest,

    isRecording: recording.isRecording,
    recordings: recording.recordings,
    recordingDuration: recording.recordingDuration,
    startRecording: () => void recording.startRecording(),
    stopRecording: recording.stopRecording,
    deleteRecording: recording.deleteRecording,

    isMiniPlayerMode,
    enterMiniPlayerMode,
    exitMiniPlayerMode,
    returnToRoom,
    clearMiniPlayerMode,
  }

  return <RoomContext.Provider value={value}>{children}</RoomContext.Provider>
}

// eslint-disable-next-line react-refresh/only-export-components
export function useRoom() {
  const context = useContext(RoomContext)
  if (!context) throw new Error('useRoom must be used within RoomProvider')
  return context
}
