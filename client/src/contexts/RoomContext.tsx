/**
 * 합주실 상태. 기능별 훅을 조합만 한다.
 *
 * - 시그널링 WS: room/useSignaling
 * - 연주(데스크톱 앱 전용): native/useNativeSession (UDP 풀메시) + native/mixBridge (관람자 방송)
 * - 관람(웹/앱 공통): room/useViewerAudio (방송 담당 연주자의 믹스를 WebRTC 수신 전용으로) + room/useWebAudioMixer
 * - 채팅/참여요청/녹음: room/useChat, room/usePerformRequests, room/useRecording
 *
 * 결정사항(docs/overhaul/native-engine-design.md): 연주자는 앱 전용, 웹은 관람만. 앱 연주자끼리는 WebRTC를 쓰지 않는다.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useAuth } from './AuthContext'
import { useAudioSettings } from './AudioSettingsContext'
import { useSignaling, type SignalPayload, type SignalStatus } from './room/useSignaling'
import { useChat, type ChatMessage } from './room/useChat'
import {
  usePerformRequests,
  type InstrumentChangeRequest,
  type MyRequestStatus,
  type PerformRequest,
} from './room/usePerformRequests'
import { useViewerAudio, type PeerNetworkStats, type RtcStatus } from './room/useViewerAudio'
import { useWebAudioMixer, type MixSettings } from './room/useWebAudioMixer'
import { useRecording, type Recording, type RecordingSource } from './room/useRecording'
import { useNativeSession } from '../native/useNativeSession'
import { MixBroadcaster, MixSource } from '../native/mixBridge'
import { computeViewers, selectBroadcaster } from '../native/sessionLogic'
import {
  MAX_PERFORMERS,
  engineErrorMessage,
  engineSetMasterGain,
  engineSetMonitor,
  isNativeApp,
  type EngineStats,
  type PeerStats,
} from '../native/engineApi'

export type { ChatMessage, InstrumentChangeRequest, MyRequestStatus, PerformRequest, PeerNetworkStats, RtcStatus, MixSettings, Recording, SignalStatus }

export interface PeerInstrument {
  peerId: string
  instrument: string
  nickname: string
  isHost: boolean
}

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
  joinRoom: (roomId: string, isHost?: boolean) => void
  joinFeedback: string
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
  /** 엔진 피어 통계 (peerId → PeerStats) */
  peerEngineStats: Record<string, PeerStats>
  isMonitoringEnabled: boolean
  monitorGain: number
  setMonitor: (enabled: boolean, gain: number) => void
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
  approveRequest: (oderId: string) => void
  rejectRequest: (oderId: string, reason?: string) => void
  myRequestStatus: MyRequestStatus
  myRequestInstrument: string | null
  requestPerform: (instrument: string) => void
  cancelRequest: () => void

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
  const [currentRoomId, setCurrentRoomId] = useState<string | null>(null)
  const [peerInstruments, setPeerInstruments] = useState<Record<string, PeerInstrument>>({})
  const [myInstrument, setMyInstrumentState] = useState<string | null>(null)
  const [isRoomHost, setIsRoomHost] = useState(false)
  const [performError, setPerformError] = useState<string | null>(null)
  const [isMonitoringEnabled, setIsMonitoringEnabled] = useState(false)
  const [monitorGain, setMonitorGain] = useState(1)
  const [broadcastViewerCount, setBroadcastViewerCount] = useState(0)
  const [isMiniPlayerMode, setIsMiniPlayerMode] = useState(false)

  const currentRoomIdRef = useRef<string | null>(null)
  const myIsHostRef = useRef(false)
  const myInstrumentRef = useRef<string | null>(null)
  const peerInstrumentsRef = useRef(peerInstruments)
  peerInstrumentsRef.current = peerInstruments
  /** WS 재연결 후 연주를 다시 시작할 악기 */
  const resumeInstrumentRef = useRef<string | null>(null)
  const isReturningToRoomRef = useRef(false)

  // 핸들러는 ref로 넘겨 WS 재연결 없이 최신 클로저를 쓴다
  const onMessageRef = useRef<(p: SignalPayload) => void>(() => {})
  const onWelcomeRef = useRef<(id: string) => void>(() => {})
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
      ...(isRejoin ? { isRejoin: true } : {}),
    }
  }, [user?.id])
  const buildJoinMessageRef = useRef(buildJoinMessage)
  buildJoinMessageRef.current = buildJoinMessage

  const signaling = useSignaling({
    onWelcome: (id) => onWelcomeRef.current(id),
    onMessage: (p) => onMessageRef.current(p),
    onClosed: () => onClosedRef.current(),
    isInRoom: () => currentRoomIdRef.current !== null,
    getJoinMessage: () => buildJoinMessageRef.current(false),
  })
  const { send, clientIdRef, isOpen } = signaling
  const clientId = signaling.clientId
  const getSelfId = useCallback(() => clientIdRef.current, [clientIdRef])

  // --- 기능 훅 ---
  const native = useNativeSession(send)
  const nativeRef = useRef(native)
  nativeRef.current = native

  const chat = useChat({ send, canSend: isOpen, getSelfId, nickname })

  const requests = usePerformRequests({
    send,
    canSend: isOpen,
    onMyInstrumentChanged: (instrument) => {
      myInstrumentRef.current = instrument
      setMyInstrumentState(instrument)
      const me = clientIdRef.current
      if (me) {
        setPeerInstruments((prev) => (prev[me] ? { ...prev, [me]: { ...prev[me], instrument } } : prev))
      }
    },
    onPeerInstrumentChanged: (peerId, instrument) => {
      setPeerInstruments((prev) => (prev[peerId] ? { ...prev, [peerId]: { ...prev[peerId], instrument } } : prev))
    },
  })

  const viewer = useViewerAudio({
    send,
    isAllowedSender: (peerId) => peerId in peerInstrumentsRef.current,
  })
  const mixer = useWebAudioMixer(viewer.remoteAudioMap)

  const mixSourceRef = useRef<MixSource | null>(null)
  if (!mixSourceRef.current) mixSourceRef.current = new MixSource()
  const broadcasterRef = useRef<MixBroadcaster | null>(null)
  if (!broadcasterRef.current) {
    broadcasterRef.current = new MixBroadcaster(mixSourceRef.current, send, setBroadcastViewerCount)
  }

  const resolveRecordingSource = useCallback(async (): Promise<RecordingSource | null> => {
    const source = mixSourceRef.current!
    if (nativeRef.current.running) {
      const stream = await source.acquire()
      return { stream, release: () => source.release() }
    }
    const stream = mixer.getRecordingStream()
    return stream ? { stream } : null
  }, [mixer])
  const recording = useRecording(resolveRecordingSource)

  // --- 연주 시작/중단 ---
  const removeSelfInstrument = useCallback(() => {
    myInstrumentRef.current = null
    setMyInstrumentState(null)
    const me = clientIdRef.current
    if (me) {
      setPeerInstruments((prev) => {
        if (!prev[me]) return prev
        const next = { ...prev }
        delete next[me]
        return next
      })
    }
  }, [clientIdRef])

  const stopPerformingInternal = useCallback(async (notify: boolean) => {
    broadcasterRef.current!.stop()
    const wasRunning = nativeRef.current.running
    await nativeRef.current.stop(notify)
    if (notify && wasRunning && isOpen()) send({ type: 'stop-performing' })
    removeSelfInstrument()
    audioSettingsRef.current.setEngineInUse(false)
  }, [isOpen, send, removeSelfInstrument])

  const stopPerforming = useCallback(() => stopPerformingInternal(true), [stopPerformingInternal])

  const startPerforming = useCallback(async (instrument: string): Promise<boolean> => {
    setPerformError(null)
    if (!isNativeApp()) {
      setPerformError('연주는 데스크톱 앱에서만 할 수 있습니다.')
      return false
    }
    const selfId = clientIdRef.current
    if (!selfId || !currentRoomIdRef.current) {
      setPerformError('합주실에 연결되지 않았습니다.')
      return false
    }
    if (nativeRef.current.running) return true
    const others = Object.keys(peerInstrumentsRef.current).filter((id) => id !== selfId)
    if (others.length >= MAX_PERFORMERS) {
      setPerformError(`연주자는 최대 ${MAX_PERFORMERS}명까지 참여할 수 있습니다.`)
      return false
    }
    const settings = audioSettingsRef.current
    if (settings.issues.length > 0) {
      setPerformError(`오디오 설정 오류: ${settings.issues[0].message}`)
      return false
    }

    await settings.stopTest()
    settings.setEngineInUse(true)
    try {
      await nativeRef.current.start(settings.config, selfId)
    } catch (err) {
      settings.setEngineInUse(false)
      setPerformError(`오디오 엔진 시작 실패: ${engineErrorMessage(err)}`)
      return false
    }

    // 엔진이 뜬 뒤에 악기 설정 + 연주 시작 알림 (서버가 연주자로 인정해야 native-descriptor를 중계함)
    myInstrumentRef.current = instrument
    setMyInstrumentState(instrument)
    send({ type: 'start-performing', instrument, nickname: nicknameRef.current, isHost: myIsHostRef.current })
    setPeerInstruments((prev) => ({
      ...prev,
      [selfId]: { peerId: selfId, instrument, nickname: nicknameRef.current, isHost: myIsHostRef.current },
    }))
    nativeRef.current.announce(others)

    // 연주자는 관람 수신을 하지 않는다
    viewer.closeAll()
    requests.resetMyRequest()
    engineSetMonitor(isMonitoringEnabled, monitorGain).catch(() => {})
    return true
  }, [clientIdRef, send, viewer, requests, isMonitoringEnabled, monitorGain])

  const startPerformingRef = useRef(startPerforming)
  startPerformingRef.current = startPerforming

  // --- 시그널링 이벤트 ---
  onWelcomeRef.current = () => {
    // 재연결: 이전 방에 다시 입장 (auth 완료 후 전송됨)
    const join = buildJoinMessageRef.current(true)
    if (join) send(join)
  }

  onClosedRef.current = () => {
    setPeers([])
    viewer.closeAll()
    // 새 연결은 clientId가 바뀌므로 엔진을 새 id로 다시 시작해야 한다
    if (nativeRef.current.running) {
      resumeInstrumentRef.current = currentRoomIdRef.current ? myInstrumentRef.current : null
      void stopPerformingInternal(false)
    }
  }

  const upsertPerformer = (p: ParticipantPayload) => {
    if (!p.instrument) return
    setPeerInstruments((prev) => ({
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
    setPeerInstruments((prev) => {
      if (!prev[peerId]) return prev
      const next = { ...prev }
      delete next[peerId]
      return next
    })
  }

  onMessageRef.current = (p: SignalPayload) => {
    const self = clientIdRef.current
    const from = typeof p.from === 'string' ? p.from : null
    if (from && from === self) return

    if (nativeRef.current.handleSignal(p)) return
    if (chat.handleChatSignal(p)) return
    if (requests.handleRequestSignal(p)) return

    switch (p.type) {
      case 'room-state': {
        const peerIds = Array.isArray(p.peerIds) ? (p.peerIds as unknown[]).filter((x): x is string => typeof x === 'string') : []
        const participants = Array.isArray(p.participants) ? (p.participants as unknown[]).filter(isParticipant) : []
        if (typeof p.isHost === 'boolean') {
          myIsHostRef.current = p.isHost
          setIsRoomHost(p.isHost)
        }
        setPeers(peerIds)
        setJoinFeedback(`룸 입장 완료 · 동시 연결 ${peerIds.length + 1}명`)
        const map: Record<string, PeerInstrument> = {}
        for (const part of participants) {
          if (part.isPerforming && part.instrument) {
            map[part.oderId] = {
              peerId: part.oderId,
              instrument: part.instrument,
              nickname: part.nickname || `User ${part.oderId.slice(0, 4)}`,
              isHost: part.isHost === true,
            }
          }
        }
        setPeerInstruments(map)
        const resume = resumeInstrumentRef.current
        resumeInstrumentRef.current = null
        if (resume) {
          // 상태 반영 후 재시작
          setTimeout(() => void startPerformingRef.current(resume), 0)
        }
        return
      }

      case 'participant-joined': {
        const part = p.participant
        if (!isParticipant(part)) return
        setPeers((prev) => (prev.includes(part.oderId) ? prev : [...prev, part.oderId]))
        if (part.isPerforming && part.instrument) {
          upsertPerformer(part)
          nativeRef.current.performerAppeared(part.oderId)
        }
        return
      }

      case 'participant-left': {
        if (typeof p.oderId !== 'string') return
        const id = p.oderId
        setPeers((prev) => prev.filter((x) => x !== id))
        removePerformer(id)
        nativeRef.current.peerGone(id)
        viewer.closeFrom(id)
        requests.dropPeer(id)
        return
      }

      case 'performer-updated': {
        if (typeof p.oderId !== 'string') return
        const id = p.oderId
        if (p.isPerforming === true && typeof p.instrument === 'string') {
          upsertPerformer({ oderId: id, nickname: p.nickname as string | undefined, instrument: p.instrument, isHost: p.isHost === true })
          nativeRef.current.performerAppeared(id)
        } else {
          removePerformer(id)
          nativeRef.current.peerGone(id)
          viewer.closeFrom(id)
        }
        return
      }

      case 'offer':
        // 관람자만 받는다. 앱 연주자는 엔진으로 듣는다.
        if (from && p.offer && !nativeRef.current.running) {
          void viewer.handleOffer(from, p.offer as RTCSessionDescriptionInit)
        }
        return

      case 'answer':
        if (from && p.answer) void broadcasterRef.current!.handleAnswer(from, p.answer as RTCSessionDescriptionInit)
        return

      case 'ice-candidate': {
        if (!from || !p.candidate) return
        const candidate = p.candidate as RTCIceCandidateInit
        if (broadcasterRef.current!.hasPeer(from)) void broadcasterRef.current!.handleCandidate(from, candidate)
        else void viewer.handleCandidate(from, candidate)
        return
      }

      case 'error':
        if (p.code === 'login-required') {
          setPerformError(typeof p.message === 'string' ? p.message : '로그인이 필요합니다.')
          if (nativeRef.current.running) void stopPerformingInternal(false)
        } else {
          console.warn('[WS] 서버 에러:', p.code, p.message)
        }
        return
    }
  }

  // --- 방 입장/퇴장 ---
  const joinRoom = useCallback((roomId: string, isHost?: boolean) => {
    if (!isOpen()) {
      setJoinFeedback('시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.')
      return
    }
    currentRoomIdRef.current = roomId
    myIsHostRef.current = isHost === true
    setIsRoomHost(isHost === true)
    setCurrentRoomId(roomId)
    const join = buildJoinMessageRef.current(false)
    if (join) send(join)
    setJoinFeedback('룸 입장 시도 중...')
  }, [isOpen, send])

  const leaveRoom = useCallback(() => {
    send({ type: 'leave' })
    // leave가 participant-left로 전파되므로 native-leave/stop-performing은 따로 보내지 않는다
    void stopPerformingInternal(false)
    resumeInstrumentRef.current = null
    viewer.closeAll()
    mixer.reset()
    recording.stopRecording()
    chat.clearChat()
    requests.resetAll()
    currentRoomIdRef.current = null
    myIsHostRef.current = false
    setIsRoomHost(false)
    setCurrentRoomId(null)
    setPeers([])
    setPeerInstruments({})
    setJoinFeedback('')
    setPerformError(null)
    setIsMiniPlayerMode(false)
  }, [send, stopPerformingInternal, viewer, mixer, recording, chat, requests])

  // --- 방송 담당 (관람자 브릿지) ---
  const performerIds = useMemo(() => Object.keys(peerInstruments), [peerInstruments])
  const broadcasterId = useMemo(() => selectBroadcaster(
    Object.values(peerInstruments).map((pi) => ({ id: pi.peerId, isHost: pi.peerId === clientId ? isRoomHost : pi.isHost })),
  ), [peerInstruments, clientId, isRoomHost])
  const viewers = useMemo(() => computeViewers(peers, performerIds, clientId), [peers, performerIds, clientId])
  const viewersRef = useRef(viewers)
  viewersRef.current = viewers
  const isBroadcaster = native.running && broadcasterId !== null && broadcasterId === clientId

  useEffect(() => {
    const b = broadcasterRef.current!
    if (isBroadcaster) {
      b.start(viewersRef.current).catch((err) => {
        console.error('[MixBridge] 방송 시작 실패:', err)
        setPerformError(`관람자 방송 시작 실패: ${engineErrorMessage(err)}`)
      })
    } else {
      b.stop()
    }
  }, [isBroadcaster])

  useEffect(() => {
    if (isBroadcaster) broadcasterRef.current!.syncViewers(viewers)
  }, [isBroadcaster, viewers])

  // 실패한 관람자 연결 재시도
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
  const { running: nativeRunning, setPeerGain } = native
  const enginePeerIds = useMemo(() => (native.stats?.peers ?? []).map((p) => p.peerId).join(','), [native.stats])
  useEffect(() => {
    if (!nativeRunning) {
      appliedGainRef.current = {}
      return
    }
    const enginePeers = new Set(enginePeerIds.split(',').filter(Boolean))
    for (const [peerId, s] of Object.entries(mixSettingsMap)) {
      if (!enginePeers.has(peerId)) continue
      const key = `${s.volume}:${s.muted}`
      if (appliedGainRef.current[peerId] === key) continue
      appliedGainRef.current[peerId] = key
      setPeerGain(peerId, s.volume, s.muted)
    }
    // 엔진에서 빠진 피어는 다시 들어오면 재적용
    for (const peerId of Object.keys(appliedGainRef.current)) {
      if (!enginePeers.has(peerId)) delete appliedGainRef.current[peerId]
    }
  }, [nativeRunning, setPeerGain, mixSettingsMap, enginePeerIds])

  useEffect(() => {
    if (!native.running) return
    engineSetMasterGain(masterMuted ? 0 : masterVolume).catch(() => {})
  }, [native.running, masterVolume, masterMuted])

  const setMonitor = useCallback((enabled: boolean, gain: number) => {
    const g = Math.max(0, Math.min(1, gain))
    setIsMonitoringEnabled(enabled)
    setMonitorGain(g)
    if (nativeRef.current.running) {
      engineSetMonitor(enabled, g).catch((err) => console.warn('[Native] 모니터 설정 실패:', engineErrorMessage(err)))
    }
  }, [])

  // --- 표시용 파생 값 ---
  const peerEngineStats = useMemo(() => {
    const map: Record<string, PeerStats> = {}
    for (const ps of native.stats?.peers ?? []) map[ps.peerId] = ps
    return map
  }, [native.stats])

  const audioLevels = useMemo(() => {
    if (!native.running) return mixer.audioLevels
    const levels: Record<string, number> = {}
    for (const ps of native.stats?.peers ?? []) levels[ps.peerId] = Math.min(100, Math.round(ps.level * 100))
    if (clientId && native.stats) levels[clientId] = Math.min(100, Math.round(native.stats.inputLevel * 100))
    return levels
  }, [native.running, native.stats, mixer.audioLevels, clientId])

  const masterLevel = native.running
    ? Math.min(100, Math.round((native.stats?.outputLevel ?? 0) * 100))
    : mixer.masterLevel

  const rtcStatus: RtcStatus = native.running ? 'live'
    : native.starting ? 'connecting'
      : performError || native.error ? 'error'
        : viewer.status
  const rtcError = performError ?? native.error ?? viewer.error

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
    currentRoomId,
    leaveRoom,
    isRoomHost,

    canPerform: isNativeApp(),
    isPerforming: native.running && myInstrument !== null,
    performStarting: native.starting,
    startPerforming,
    stopPerforming,
    myInstrument,
    peerInstruments,
    engineStats: native.stats,
    peerEngineStats,
    isMonitoringEnabled,
    monitorGain,
    setMonitor,
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
    approveRequest: requests.approveRequest,
    rejectRequest: requests.rejectRequest,
    myRequestStatus: requests.myRequestStatus,
    myRequestInstrument: requests.myRequestInstrument,
    requestPerform: requests.requestPerform,
    cancelRequest: requests.cancelRequest,

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
