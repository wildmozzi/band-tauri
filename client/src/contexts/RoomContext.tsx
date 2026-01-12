import { createContext, useContext, useEffect, useRef, useState, useCallback, type ReactNode } from 'react'
import { useAudioSettings, ActualAudioSettings } from './AudioSettingsContext'
import { useAuth } from './AuthContext'
import { supabase } from '../lib/supabaseClient'
import {
  isTauri,
  initNetwork,
  shutdownNetwork,
  startAudio,
  stopAudio,
  connectToPeer,
  setMonitoring,
  getMonitoring,
  getNetworkInfo,
  type NetworkInfo,
  type AudioSettings as TauriAudioSettings,
  DEFAULT_AUDIO_SETTINGS,
} from '../lib/tauri'

// 자동 IP 감지: 'auto'면 현재 접속한 호스트 사용
function getSignalingUrl(): string {
  const envUrl = import.meta.env.VITE_SIGNALING_URL
  if (!envUrl || envUrl === 'auto') {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const host = window.location.hostname
    return `${protocol}//${host}:8080`
  }
  return envUrl
}
const SIGNALING_URL = getSignalingUrl()
const ICE_SERVERS: RTCIceServer[] = [{ urls: 'stun:stun.l.google.com:19302' }]

/**
 * SDP를 수정하여 저지연 오디오 최적화 적용
 * - Opus ptime을 10ms로 설정 (기본 20ms → 10ms)
 * - stereo 비활성화 (모노로 전송)
 * - FEC 비활성화 (지연 감소, 패킷 손실에 취약해짐)
 * - DTX 비활성화 (무음 구간에도 패킷 전송)
 */
function optimizeSdpForLowLatency(sdp: string): string {
  let optimizedSdp = sdp

  // Opus 코덱의 fmtp 라인 찾아서 수정
  // a=fmtp:111 ... 형식
  optimizedSdp = optimizedSdp.replace(
    /(a=fmtp:111 .+)/g,
    (match) => {
      let newFmtp = match
      // ptime이 없으면 추가 (10ms 패킷)
      if (!newFmtp.includes('ptime=')) {
        newFmtp += ';ptime=10'
      }
      // maxptime이 없으면 추가
      if (!newFmtp.includes('maxptime=')) {
        newFmtp += ';maxptime=10'
      }
      // stereo 비활성화
      if (!newFmtp.includes('stereo=')) {
        newFmtp += ';stereo=0'
      }
      // FEC 비활성화 (레이턴시 감소)
      if (!newFmtp.includes('useinbandfec=')) {
        newFmtp += ';useinbandfec=0'
      }
      // DTX 비활성화 (무음에서도 패킷 전송)
      if (!newFmtp.includes('usedtx=')) {
        newFmtp += ';usedtx=0'
      }
      return newFmtp
    }
  )

  // a=ptime 라인이 있으면 10으로 수정
  optimizedSdp = optimizedSdp.replace(/a=ptime:\d+/g, 'a=ptime:10')

  // a=maxptime 라인이 있으면 10으로 수정
  optimizedSdp = optimizedSdp.replace(/a=maxptime:\d+/g, 'a=maxptime:10')

  console.log('[RTC] SDP optimized for low latency')
  return optimizedSdp
}

type RtcStatus = 'idle' | 'connecting' | 'live' | 'error'
type SignalStatus = 'idle' | 'connecting' | 'connected' | 'error'

// 개인 믹서 설정
export interface MixSettings {
  volume: number  // 0 ~ 1
  pan: number     // -1 (좌) ~ 1 (우)
  muted: boolean
}

// 채팅 메시지
export interface ChatMessage {
  id: string
  peerId: string
  nickname: string
  message: string
  timestamp: number
}

// 연주자 악기 정보
export interface PeerInstrument {
  peerId: string
  instrument: string
  nickname: string
  isHost: boolean
}

// 피어 네트워크 상태
export interface PeerNetworkStats {
  audioLatency: number | null   // 실제 오디오 지연 (RTT/2 + jitterBuffer) in ms
  jitter: number | null         // 지터 in ms
  packetsLost: number           // 패킷 손실 수
  packetLossRate: number        // 패킷 손실률 (%)
  quality: 'excellent' | 'good' | 'fair' | 'poor' | 'unknown'
}

// 연주 참여 요청
export interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

// 악기 변경 요청
export interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

// 내 요청 상태
export type MyRequestStatus = 'none' | 'pending' | 'approved' | 'rejected'

// 녹음 파일 정보
export interface Recording {
  id: string
  blob: Blob
  url: string
  timestamp: number
  duration: number
  mimeType: string
}

// 네트워크 품질 판정 함수 (오디오 지연 기준)
function getNetworkQuality(audioLatency: number | null, packetLossRate: number): PeerNetworkStats['quality'] {
  if (audioLatency === null) return 'unknown'
  // 오디오 지연 기준: 합주 가능 수준 판정
  // 30ms 이하: 거의 실시간, 50ms 이하: 합주 가능, 100ms 이하: 약간 지연 느낌
  if (audioLatency < 30 && packetLossRate < 1) return 'excellent'
  if (audioLatency < 50 && packetLossRate < 3) return 'good'
  if (audioLatency < 100 && packetLossRate < 5) return 'fair'
  return 'poor'
}

type RoomContextType = {
  // WebSocket
  signalStatus: SignalStatus
  clientId: string | null
  peers: string[]
  joinRoom: (roomId: string, isHost?: boolean) => void
  joinFeedback: string

  // WebRTC
  rtcStatus: RtcStatus
  rtcError: string | null
  localStream: MediaStream | null
  remoteAudioMap: Record<string, MediaStream>
  startLocalMic: () => Promise<void>
  stopLocalMic: () => void
  localMuted: boolean
  toggleLocalMute: () => void
  currentRoomId: string | null
  leaveRoom: () => void

  // 실제 적용된 오디오 설정
  actualStreamSettings: ActualAudioSettings | null

  // 개인 믹서
  mixSettingsMap: Record<string, MixSettings>
  setMixVolume: (oderId: string, volume: number) => void
  setMixPan: (oderId: string, pan: number) => void
  setMixMuted: (oderId: string, muted: boolean) => void
  masterVolume: number
  setMasterVolume: (volume: number) => void
  masterMuted: boolean
  toggleMasterMute: () => void
  masterPan: number
  setMasterPan: (pan: number) => void
  // 오디오 레벨
  audioLevels: Record<string, number>  // 각 피어별 오디오 레벨 (0-100)
  masterLevel: number                   // 마스터 오디오 레벨 (0-100)
  resumeAllAudioContexts: () => void    // AudioContext resume (사용자 상호작용 후)
  // 오디오 요소 등록
  registerAudioStream: (peerId: string, stream: MediaStream) => void
  unregisterAudioStream: (peerId: string) => void

  // 채팅
  chatMessages: ChatMessage[]
  sendChatMessage: (message: string) => void
  nickname: string

  // 연주자 악기 정보
  peerInstruments: Record<string, PeerInstrument>
  myInstrument: string | null
  setMyInstrument: (instrument: string, isHost?: boolean) => void

  // 네트워크 상태
  peerNetworkStats: Record<string, PeerNetworkStats>

  // 연주 참여 요청 (방장용)
  pendingRequests: PerformRequest[]
  approveRequest: (oderId: string) => void
  rejectRequest: (oderId: string, reason?: string) => void

  // 연주 참여 요청 (요청자용)
  myRequestStatus: MyRequestStatus
  myRequestInstrument: string | null
  requestPerform: (instrument: string) => void
  cancelRequest: () => void

  // 악기 변경 요청 (방장용)
  pendingInstrumentChanges: InstrumentChangeRequest[]
  approveInstrumentChange: (oderId: string) => void
  rejectInstrumentChange: (oderId: string, reason?: string) => void

  // 악기 변경 요청 (연주자용)
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

  // 미니 플레이어 모드
  isMiniPlayerMode: boolean
  enterMiniPlayerMode: () => void
  exitMiniPlayerMode: () => void
  returnToRoom: () => string | null
  clearMiniPlayerMode: () => void

  // Tauri 네이티브 오디오
  usingNativeAudio: boolean
  nativeNetworkInfo: NetworkInfo | null
  isMonitoringEnabled: boolean
  toggleMonitoring: () => Promise<void>
}

const RoomContext = createContext<RoomContextType | null>(null)

export function RoomProvider({ children }: { children: ReactNode }) {
  // 인증된 사용자 정보 가져오기
  const { user } = useAuth()

  // 오디오 설정 가져오기
  const { settings: audioSettings, isInputTesting } = useAudioSettings()

  const [signalStatus, setSignalStatus] = useState<SignalStatus>('idle')
  const [clientId, setClientId] = useState<string | null>(null)
  const [peers, setPeers] = useState<string[]>([])
  const [joinFeedback, setJoinFeedback] = useState('')
  const [currentRoomId, setCurrentRoomId] = useState<string | null>(null)

  const [rtcStatus, setRtcStatus] = useState<RtcStatus>('idle')
  const [rtcError, setRtcError] = useState<string | null>(null)
  const [localStream, setLocalStream] = useState<MediaStream | null>(null)
  const [remoteAudioMap, setRemoteAudioMap] = useState<Record<string, MediaStream>>({})
  const [actualStreamSettings, setActualStreamSettings] = useState<ActualAudioSettings | null>(null)
  const [localMuted, setLocalMuted] = useState(false)

  // Tauri 네이티브 오디오 상태
  const [usingNativeAudio, setUsingNativeAudio] = useState(false)
  const [nativeNetworkInfo, setNativeNetworkInfo] = useState<NetworkInfo | null>(null)
  const [isMonitoringEnabled, setIsMonitoringEnabled] = useState(false)
  const nativeAudioActiveRef = useRef(false)  // 동기적 체크용
  const nativeNetworkInfoRef = useRef<NetworkInfo | null>(null)  // WebSocket 콜백용
  const tauriPeersRef = useRef<Set<string>>(new Set())  // Tauri P2P로 연결된 피어 추적
  const p2pConnectingRef = useRef<Set<string>>(new Set())  // P2P 연결 시도 중인 피어 (중복 방지)
  const isInputTestingRef = useRef(isInputTesting)  // 입력 테스트 중 여부 (콜백용)
  const prevLocalMutedRef = useRef(false)  // 입력 테스트 전 로컬 뮤트 상태 저장

  // 개인 믹서 상태
  const [mixSettingsMap, setMixSettingsMap] = useState<Record<string, MixSettings>>({})
  const [masterVolume, setMasterVolume] = useState(1)
  const [masterMuted, setMasterMuted] = useState(false)
  const [masterPan, setMasterPan] = useState(0)

  // Web Audio API 노드 저장 (MediaStreamAudioSourceNode 사용)
  const audioNodesRef = useRef<Map<string, {
    source: MediaStreamAudioSourceNode
    gain: GainNode
    panner: StereoPannerNode
    analyser: AnalyserNode
  }>>(new Map())

  // 숨겨진 audio 요소 저장 (MediaStream 소비용)
  const hiddenAudioElementsRef = useRef<Map<string, HTMLAudioElement>>(new Map())

  // 미니 플레이어 모드 ref (unregisterAudioStream에서 사용)
  const isMiniPlayerModeRef = useRef(false)

  // 공유 AudioContext (한 번만 생성)
  const audioContextRef = useRef<AudioContext | null>(null)

  const getAudioContext = (): AudioContext => {
    if (!audioContextRef.current || audioContextRef.current.state === 'closed') {
      // latencyHint: 'interactive'로 최소 레이턴시 설정
      audioContextRef.current = new AudioContext({
        latencyHint: 'interactive',  // 최소 레이턴시 모드
        sampleRate: 48000,           // 고정 샘플레이트
      })
      console.log('[AUDIO] Created new AudioContext with interactive latency hint')
      console.log('[AUDIO] Base latency:', audioContextRef.current.baseLatency, 's')
      console.log('[AUDIO] Output latency:', audioContextRef.current.outputLatency, 's')
    }
    return audioContextRef.current
  }

  // 녹음용 destination 노드 가져오기
  const getRecordingDestination = (): MediaStreamAudioDestinationNode => {
    if (!recordingDestinationRef.current) {
      const context = getAudioContext()
      recordingDestinationRef.current = context.createMediaStreamDestination()
      console.log('[RECORDING] Created MediaStreamAudioDestinationNode for recording')
    }
    return recordingDestinationRef.current
  }

  // MediaStream을 Web Audio API에 연결
  const connectToWebAudio = (peerId: string, stream: MediaStream): boolean => {
    // 이미 이 peerId로 노드가 있으면 스킵
    if (audioNodesRef.current.has(peerId)) {
      console.log('[AUDIO] Already has nodes for:', peerId.slice(0, 8))
      return true
    }

    try {
      const context = getAudioContext()
      console.log('[AUDIO] Creating Web Audio nodes for:', peerId.slice(0, 8), 'context state:', context.state)

      // AudioContext가 suspended 상태면 resume 시도
      if (context.state === 'suspended') {
        context.resume().then(() => {
          console.log('[AUDIO] AudioContext resumed in connectToWebAudio')
        }).catch(err => {
          console.log('[AUDIO] Could not resume context:', err.message)
        })
      }

      // MediaStreamAudioSourceNode 사용 (재연결 가능)
      const source = context.createMediaStreamSource(stream)
      const analyser = context.createAnalyser()
      const gain = context.createGain()
      const panner = context.createStereoPanner()

      analyser.fftSize = 256
      analyser.smoothingTimeConstant = 0.8

      // 현재 믹서 설정 적용
      const settings = mixSettingsMap[peerId]
      const volume = settings?.volume ?? 1
      const pan = settings?.pan ?? 0
      const muted = settings?.muted ?? false

      gain.gain.value = (masterMuted || muted) ? 0 : volume * masterVolume
      panner.pan.value = Math.max(-1, Math.min(1, pan + masterPan))

      // 연결: source -> analyser -> gain -> panner -> destination
      source.connect(analyser)
      analyser.connect(gain)
      gain.connect(panner)
      panner.connect(context.destination)

      audioNodesRef.current.set(peerId, { source, gain, panner, analyser })

      console.log('[AUDIO] Successfully connected to Web Audio:', peerId.slice(0, 8),
        'gain:', gain.gain.value, 'pan:', panner.pan.value,
        'totalNodes:', audioNodesRef.current.size)
      return true
    } catch (err) {
      console.error('[AUDIO] Failed to connect:', err)
      return false
    }
  }

  // audio 스트림 등록 (RoomDetail에서 호출) - useCallback으로 안정적인 참조 유지
  const registerAudioStream = useCallback((peerId: string, stream: MediaStream) => {
    connectToWebAudio(peerId, stream)
  }, [])

  // 오디오 노드 해제 - useCallback으로 안정적인 참조 유지
  // 주의: RemoteAudio cleanup에서는 호출하지 않음 (미니플레이어 모드 대응)
  // remoteAudioMap에서 스트림이 제거될 때만 호출됨
  const unregisterAudioStream = useCallback((peerId: string) => {
    const nodes = audioNodesRef.current.get(peerId)
    if (nodes) {
      nodes.source.disconnect()
      nodes.analyser.disconnect()
      nodes.gain.disconnect()
      nodes.panner.disconnect()
      audioNodesRef.current.delete(peerId)
      console.log('[AUDIO] Disconnected and removed nodes:', peerId.slice(0, 8))
    }
  }, [])

  // 오디오 레벨 상태
  const [audioLevels, setAudioLevels] = useState<Record<string, number>>({})
  const [masterLevel, setMasterLevel] = useState(0)
  const levelAnimationRef = useRef<number | null>(null)

  // AudioContext resume (사용자 상호작용 후 호출)
  const resumeAllAudioContexts = () => {
    const context = audioContextRef.current
    if (context && context.state === 'suspended') {
      context.resume().then(() => {
        console.log('[AUDIO] AudioContext resumed')

        // Resume 후 기존 스트림들 재연결 (suspended 상태에서 연결된 노드들 복구)
        Object.entries(remoteAudioMap).forEach(([peerId, stream]) => {
          // 기존 노드 제거 후 재연결
          const existingNodes = audioNodesRef.current.get(peerId)
          if (existingNodes) {
            try {
              existingNodes.source.disconnect()
              existingNodes.analyser.disconnect()
              existingNodes.gain.disconnect()
              existingNodes.panner.disconnect()
              audioNodesRef.current.delete(peerId)
              console.log('[AUDIO] Removed old nodes for reconnection:', peerId.slice(0, 8))
            } catch (e) {
              // 이미 disconnect된 경우 무시
            }
          }
          // 새로 연결
          connectToWebAudio(peerId, stream)
          console.log('[AUDIO] Reconnected after resume:', peerId.slice(0, 8))
        })
      })
    }
  }

  // 스트림 자동 연결 (미니 플레이어 모드 포함)
  // RemoteAudio 컴포넌트가 없어도 Web Audio에 연결하여 소리가 들리도록 함
  // 중요: 이 useEffect는 레벨 측정 useEffect보다 먼저 실행되어야 함 (노드 생성 후 레벨 측정)
  useEffect(() => {
    const currentPeerIds = Object.keys(remoteAudioMap)

    currentPeerIds.forEach(peerId => {
      const stream = remoteAudioMap[peerId]
      if (!stream) return

      // Web Audio에 연결되어 있지 않으면 연결
      if (!audioNodesRef.current.has(peerId)) {
        console.log('[AUDIO] Auto-connecting stream for:', peerId.slice(0, 8), 'miniPlayerMode:', isMiniPlayerModeRef.current)
        connectToWebAudio(peerId, stream)
      }

      // 미니 플레이어 모드에서 숨겨진 audio 요소로 스트림 소비 (브라우저가 스트림을 활성 상태로 유지)
      if (isMiniPlayerModeRef.current && !hiddenAudioElementsRef.current.has(peerId)) {
        const audio = document.createElement('audio')
        audio.srcObject = stream
        audio.muted = true // Web Audio가 실제 출력 담당
        audio.setAttribute('playsinline', 'true')
        audio.play().catch(err => {
          console.log('[AUDIO] Hidden audio play failed:', err.message)
        })
        hiddenAudioElementsRef.current.set(peerId, audio)
        console.log('[AUDIO] Created hidden audio element for mini player:', peerId.slice(0, 8))
      }
    })

    // 제거된 스트림의 오디오 노드 및 숨겨진 요소 정리
    audioNodesRef.current.forEach((_, peerId) => {
      if (!remoteAudioMap[peerId]) {
        unregisterAudioStream(peerId)
      }
    })

    hiddenAudioElementsRef.current.forEach((audio, peerId) => {
      if (!remoteAudioMap[peerId]) {
        audio.srcObject = null
        audio.remove()
        hiddenAudioElementsRef.current.delete(peerId)
        console.log('[AUDIO] Removed hidden audio element:', peerId.slice(0, 8))
      }
    })
  }, [remoteAudioMap])

  // 오디오 레벨 측정 (AnalyserNode 사용)
  // 중요: 이 useEffect는 자동 연결 useEffect 이후에 실행되어야 함 (노드 생성 후 레벨 측정)
  useEffect(() => {
    const measureLevels = () => {
      const newLevels: Record<string, number> = {}
      let maxLevel = 0

      audioNodesRef.current.forEach((nodes, peerId) => {
        const analyser = nodes.analyser
        const dataArray = new Uint8Array(analyser.frequencyBinCount)
        analyser.getByteFrequencyData(dataArray)

        // 평균 볼륨 계산 (0-100)
        const sum = dataArray.reduce((a, b) => a + b, 0)
        const avg = sum / dataArray.length
        const level = Math.min(100, Math.round((avg / 256) * 100 * 1.5)) // 1.5배 증폭

        newLevels[peerId] = level
        maxLevel = Math.max(maxLevel, level)
      })

      setAudioLevels(newLevels)
      setMasterLevel(maxLevel)

      levelAnimationRef.current = requestAnimationFrame(measureLevels)
    }

    // 오디오 노드가 있을 때만 측정 시작
    if (audioNodesRef.current.size > 0) {
      measureLevels()
    }

    return () => {
      if (levelAnimationRef.current) {
        cancelAnimationFrame(levelAnimationRef.current)
        levelAnimationRef.current = null
      }
    }
  }, [remoteAudioMap]) // remoteAudioMap이 변경될 때 재시작

  // 채팅 상태
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([])

  // 닉네임: 로그인한 사용자의 닉네임 사용, 없으면 기본값
  const nickname = user?.user_metadata?.nickname || `Guest${Math.random().toString(36).slice(2, 6)}`

  // 연주자 악기 정보 상태
  const [peerInstruments, setPeerInstruments] = useState<Record<string, PeerInstrument>>({})
  const [myInstrument, setMyInstrumentState] = useState<string | null>(null)

  // 네트워크 상태
  const [peerNetworkStats, setPeerNetworkStats] = useState<Record<string, PeerNetworkStats>>({})

  // 연주 참여 요청 상태 (방장용)
  const [pendingRequests, setPendingRequests] = useState<PerformRequest[]>([])

  // 내 요청 상태 (요청자용)
  const [myRequestStatus, setMyRequestStatus] = useState<MyRequestStatus>('none')
  const [myRequestInstrument, setMyRequestInstrument] = useState<string | null>(null)

  // 악기 변경 요청 상태 (방장용)
  const [pendingInstrumentChanges, setPendingInstrumentChanges] = useState<InstrumentChangeRequest[]>([])

  // 악기 변경 요청 상태 (연주자용)
  const [myInstrumentChangeStatus, setMyInstrumentChangeStatus] = useState<MyRequestStatus>('none')
  const [myNewInstrument, setMyNewInstrument] = useState<string | null>(null)

  // 녹음 상태
  const [isRecording, setIsRecording] = useState(false)
  const [recordings, setRecordings] = useState<Recording[]>([])
  const [recordingDuration, setRecordingDuration] = useState(0)
  const recordingDestinationRef = useRef<MediaStreamAudioDestinationNode | null>(null)
  const mediaRecorderRef = useRef<MediaRecorder | null>(null)
  const recordingChunksRef = useRef<Blob[]>([])
  const recordingStartTimeRef = useRef<number>(0)
  const recordingTimerRef = useRef<number | null>(null)
  const recordingMimeTypeRef = useRef<string>('')

  // 미니 플레이어 모드 상태
  const [isMiniPlayerMode, setIsMiniPlayerMode] = useState(false)
  // 미니 플레이어에서 방으로 돌아가는 중인지 (cleanup에서 다시 미니 모드로 전환 방지)
  const isReturningToRoomRef = useRef(false)

  const wsRef = useRef<WebSocket | null>(null)
  const peerConnections = useRef<Map<string, RTCPeerConnection>>(new Map())
  // ICE candidate 큐 (remote description 설정 전에 도착한 candidate 저장)
  const pendingIceCandidates = useRef<Map<string, RTCIceCandidateInit[]>>(new Map())

  // 최신 상태 참조용 ref (useEffect 클로저 내부에서 사용)
  const myInstrumentRef = useRef<string | null>(null)
  const nicknameRef = useRef<string>(nickname)
  const localStreamRef = useRef<MediaStream | null>(null)
  const clientIdRef = useRef<string | null>(null)
  const peersRef = useRef<string[]>([])

  // 재연결용 ref (WebSocket 끊어졌을 때 다시 join하기 위한 정보)
  const currentRoomIdRef = useRef<string | null>(null)
  const isHostRef = useRef<boolean>(false)

  const sendSignalMessage = (payload: Record<string, unknown>) => {
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(payload))
    }
  }

  const attachLocalTracks = (pc: RTCPeerConnection, stream?: MediaStream | null) => {
    // ref를 사용하여 항상 최신 localStream 사용
    const streamToUse = stream !== undefined ? stream : localStreamRef.current
    if (!streamToUse) {
      console.log('[RTC] attachLocalTracks: No stream available!')
      return
    }
    console.log('[RTC] attachLocalTracks: Adding', streamToUse.getTracks().length, 'tracks')
    streamToUse.getTracks().forEach((track) => {
      const already = pc.getSenders().some((sender) => sender.track === track)
      if (!already) {
        pc.addTrack(track, streamToUse)
        console.log('[RTC] Track added:', track.kind)
      }
    })
  }

  const closePeerConnection = (peerId: string) => {
    const pc = peerConnections.current.get(peerId)
    if (pc) {
      pc.onicecandidate = null
      pc.ontrack = null
      pc.onconnectionstatechange = null
      pc.close()
      peerConnections.current.delete(peerId)
    }
    // pending ICE candidates 정리
    pendingIceCandidates.current.delete(peerId)
    setRemoteAudioMap((prev) => {
      if (!(peerId in prev)) return prev
      const next = { ...prev }
      delete next[peerId]
      return next
    })
    // 모든 연결이 닫히고 localStream도 없으면 idle로 전환
    if (peerConnections.current.size === 0 && !localStreamRef.current) {
      setRtcStatus('idle')
    }
  }

  const createPeerConnection = (peerId: string) => {
    const pc = new RTCPeerConnection({ iceServers: ICE_SERVERS })
    pc.onicecandidate = (event) => {
      if (event.candidate) {
        console.log('[RTC] ICE candidate generated for:', peerId.slice(0, 8))
        sendSignalMessage({ type: 'ice-candidate', to: peerId, candidate: event.candidate })
      } else {
        console.log('[RTC] ICE gathering complete for:', peerId.slice(0, 8))
      }
    }
    pc.ontrack = (event) => {
      const stream = event.streams[0] ?? new MediaStream([event.track])
      console.log('[RTC] ontrack received from:', peerId.slice(0, 8), {
        trackKind: event.track.kind,
        trackEnabled: event.track.enabled,
        trackReadyState: event.track.readyState,
        streamId: stream.id,
        streamActive: stream.active
      })
      setRemoteAudioMap((prev) => ({ ...prev, [peerId]: stream }))
    }
    pc.onconnectionstatechange = () => {
      console.log(`[RTC] Connection state changed: ${pc.connectionState} (peer: ${peerId.slice(0, 8)})`)
      if (pc.connectionState === 'connected') {
        console.log('[RTC] Setting rtcStatus to LIVE!')
        setRtcStatus('live')
      } else if (pc.connectionState === 'failed') {
        setRtcStatus('error')
        closePeerConnection(peerId)
      } else if (pc.connectionState === 'disconnected' || pc.connectionState === 'closed') {
        closePeerConnection(peerId)
      }
    }
    pc.oniceconnectionstatechange = () => {
      console.log(`[RTC] ICE state changed: ${pc.iceConnectionState} (peer: ${peerId.slice(0, 8)})`)
    }
    attachLocalTracks(pc)
    peerConnections.current.set(peerId, pc)
    return pc
  }

  const ensurePeerConnection = (peerId: string) =>
    peerConnections.current.get(peerId) ?? createPeerConnection(peerId)

  const teardownPeerConnections = () => {
    peerConnections.current.forEach((_, peerId) => closePeerConnection(peerId))
    setRemoteAudioMap({})

    // 숨겨진 audio 요소들 정리
    hiddenAudioElementsRef.current.forEach((audio, peerId) => {
      audio.srcObject = null
      audio.remove()
      console.log('[AUDIO] Teardown - removed hidden audio for:', peerId.slice(0, 8))
    })
    hiddenAudioElementsRef.current.clear()

    // rtcStatus는 피어 연결이 모두 닫힌 후 자연스럽게 idle이 됨
    // StrictMode cleanup 시 불필요한 상태 변경 방지
  }

  const createOfferForPeer = async (peerId: string) => {
    console.log('[RTC] createOfferForPeer called for:', peerId.slice(0, 8))

    // Tauri 앱이면 WebRTC 완전 스킵 (P2P UDP만 사용)
    // 합주자끼리는 P2P UDP, 관람자에게는 추후 WebRTC 브릿지 구현 예정
    if (nativeAudioActiveRef.current) {
      console.log('[RTC] Skipping WebRTC - Tauri uses P2P UDP only:', peerId.slice(0, 8))
      setRtcStatus('live')  // P2P로 연결됨
      // 피어에게 P2P discovery 전송 (아직 안 했으면)
      if (!tauriPeersRef.current.has(peerId)) {
        sendSignalMessage({ type: 'p2p-discovery', to: peerId })
      }
      return
    }

    try {
      const pc = ensurePeerConnection(peerId)
      // 이미 connected 상태면 connecting으로 바꾸지 않음
      if (pc.connectionState !== 'connected') {
        setRtcStatus('connecting')
      }
      const offer = await pc.createOffer()

      // SDP 최적화 적용 (저지연)
      const optimizedOffer = {
        ...offer,
        sdp: optimizeSdpForLowLatency(offer.sdp || '')
      }

      await pc.setLocalDescription(optimizedOffer)
      console.log('[RTC] Offer created (optimized) and sent to:', peerId.slice(0, 8))
      sendSignalMessage({ type: 'offer', to: peerId, offer: optimizedOffer })
    } catch (error) {
      console.error('[RTC] createOfferForPeer error:', error)
      setRtcStatus('error')
      setRtcError('WebRTC offer 생성에 실패했습니다.')
    }
  }

  // pending ICE candidates 적용
  const applyPendingCandidates = async (peerId: string, pc: RTCPeerConnection) => {
    const pending = pendingIceCandidates.current.get(peerId)
    if (pending && pending.length > 0) {
      console.log('[RTC] Applying', pending.length, 'pending ICE candidates for:', peerId.slice(0, 8))
      for (const candidate of pending) {
        try {
          // null이나 빈 candidate는 건너뛰기
          if (candidate && candidate.candidate) {
            await pc.addIceCandidate(new RTCIceCandidate(candidate))
          }
        } catch (err) {
          // ICE candidate 에러는 무시 (이미 연결된 경우 발생할 수 있음)
          console.warn('[RTC] Failed to add pending ICE candidate (may be stale):', err)
        }
      }
      pendingIceCandidates.current.delete(peerId)
    }
  }

  const handleRemoteOffer = async (peerId: string, offer: RTCSessionDescriptionInit) => {
    console.log('[RTC] handleRemoteOffer from:', peerId.slice(0, 8), 'signalingState:', peerConnections.current.get(peerId)?.signalingState || 'none')

    // Tauri 앱이면 WebRTC offer 무시 (P2P UDP만 사용)
    if (nativeAudioActiveRef.current) {
      console.log('[RTC] Ignoring WebRTC offer - Tauri uses P2P UDP only:', peerId.slice(0, 8))
      // 피어에게 P2P discovery 전송
      if (!tauriPeersRef.current.has(peerId)) {
        sendSignalMessage({ type: 'p2p-discovery', to: peerId })
      }
      return
    }

    try {
      const pc = ensurePeerConnection(peerId)

      // Glare 처리: 양쪽에서 동시에 offer를 보낸 경우
      if (pc.signalingState === 'have-local-offer') {
        // clientId가 더 큰 쪽이 자신의 offer를 유지 (polite peer 패턴)
        const myId = clientIdRef.current
        if (myId && myId > peerId) {
          console.log('[RTC] Glare detected: ignoring incoming offer (we have priority)')
          return
        }
        // 상대방이 우선순위가 높으면 내 offer를 롤백
        console.log('[RTC] Glare detected: rolling back local offer')
        await pc.setLocalDescription({ type: 'rollback' })
      }

      await pc.setRemoteDescription(offer)
      // pending ICE candidates 적용
      await applyPendingCandidates(peerId, pc)
      attachLocalTracks(pc)
      const answer = await pc.createAnswer()

      // SDP 최적화 적용 (저지연)
      const optimizedAnswer = {
        ...answer,
        sdp: optimizeSdpForLowLatency(answer.sdp || '')
      }

      await pc.setLocalDescription(optimizedAnswer)
      console.log('[RTC] Answer created (optimized) and sent to:', peerId.slice(0, 8), 'iceGatheringState:', pc.iceGatheringState)
      sendSignalMessage({ type: 'answer', to: peerId, answer: optimizedAnswer })
      // 이미 connected 상태면 connecting으로 바꾸지 않음
      if (pc.connectionState !== 'connected') {
        setRtcStatus('connecting')
      }
    } catch (error) {
      console.error('[RTC] handleRemoteOffer error:', error)
      setRtcStatus('error')
      setRtcError('원격 offer 처리 중 오류가 발생했습니다.')
    }
  }

  const handleRemoteAnswer = async (peerId: string, answer: RTCSessionDescriptionInit) => {
    console.log('[RTC] handleRemoteAnswer from:', peerId.slice(0, 8), 'signalingState:', peerConnections.current.get(peerId)?.signalingState)
    const pc = peerConnections.current.get(peerId)
    if (!pc) return

    // answer는 have-local-offer 상태에서만 적용 가능
    // stable 또는 다른 상태에서는 무시 (Glare로 인해 stale answer가 도착한 경우)
    if (pc.signalingState !== 'have-local-offer') {
      console.log('[RTC] Ignoring answer - signalingState is not have-local-offer:', pc.signalingState)
      return
    }

    try {
      await pc.setRemoteDescription(answer)
      // pending ICE candidates 적용
      await applyPendingCandidates(peerId, pc)
      console.log('[RTC] Remote answer set, connectionState:', pc.connectionState)
    } catch (error) {
      // Glare로 인한 stale answer 에러는 무시
      console.warn('[RTC] handleRemoteAnswer error (may be stale due to glare):', error)
    }
  }

  const handleRemoteCandidate = async (peerId: string, candidate: RTCIceCandidateInit) => {
    // null이나 빈 candidate는 무시 (ICE gathering 완료 신호)
    if (!candidate || !candidate.candidate) {
      return
    }

    const pc = peerConnections.current.get(peerId)
    if (!pc) {
      return
    }

    // 연결이 이미 완료되었거나 실패한 경우 무시
    if (pc.iceConnectionState === 'connected' || pc.iceConnectionState === 'completed' ||
        pc.iceConnectionState === 'failed' || pc.iceConnectionState === 'closed') {
      return
    }

    // remote description이 설정되지 않은 경우 큐에 저장
    if (!pc.remoteDescription) {
      if (!pendingIceCandidates.current.has(peerId)) {
        pendingIceCandidates.current.set(peerId, [])
      }
      pendingIceCandidates.current.get(peerId)!.push(candidate)
      return
    }

    try {
      await pc.addIceCandidate(new RTCIceCandidate(candidate))
    } catch {
      // 이미 연결된 경우나 stale candidate는 조용히 무시
    }
  }

  const startLocalMic = async () => {
    // 이미 로컬 스트림이 있거나 네이티브 오디오가 활성화된 경우 return
    if (localStream || nativeAudioActiveRef.current) return

    // Tauri 앱에서 실행 중이면 네이티브 오디오 사용
    if (isTauri()) {
      try {
        console.log('[Tauri] Starting native audio engine...')
        setRtcStatus('connecting')

        // 클라이언트 ID 확인
        const myId = clientIdRef.current
        if (!myId) {
          throw new Error('Client ID not set - cannot start P2P network')
        }

        // 1. 네트워크 초기화 (UDP 소켓 바인딩)
        const networkInfo = await initNetwork(myId)
        console.log('[Tauri] Network initialized:', networkInfo)

        setNativeNetworkInfo(networkInfo)
        nativeNetworkInfoRef.current = networkInfo

        // 2. 오디오 엔진 시작 (설정 적용)
        // Supabase DB에서 오디오 설정 로드
        let tauriAudioSettings: TauriAudioSettings = DEFAULT_AUDIO_SETTINGS

        if (user?.id && supabase) {
          try {
            const { data, error } = await supabase
              .from('profiles')
              .select('audio_settings')
              .eq('id', user.id)
              .single()

            if (!error && data?.audio_settings) {
              tauriAudioSettings = { ...DEFAULT_AUDIO_SETTINGS, ...data.audio_settings }
              console.log('[Tauri] Audio settings loaded from DB:', tauriAudioSettings)
            }
          } catch (e) {
            console.warn('[Tauri] Failed to load audio settings from DB, using defaults:', e)
          }
        }

        await startAudio(tauriAudioSettings)
        console.log('[Tauri] Audio started with settings:', tauriAudioSettings)

        setUsingNativeAudio(true)
        nativeAudioActiveRef.current = true
        setRtcStatus('live')
        setRtcError(null)

        // 백엔드의 모니터링 상태 동기화
        try {
          const monitoringState = await getMonitoring()
          setIsMonitoringEnabled(monitoringState)
          console.log('[Tauri] Monitoring state synced:', monitoringState)
        } catch (e) {
          console.warn('[Tauri] Failed to get monitoring state:', e)
        }

        // 방에 있는 피어들과 P2P 연결 시도
        const currentPeers = peersRef.current
        console.log('[Tauri] Connecting to', currentPeers.length, 'peers via P2P')

        for (const peerId of currentPeers) {
          // 시그널링으로 피어의 public address 요청 필요
          sendSignalMessage({ type: 'p2p-discovery', to: peerId })
        }

        return
      } catch (err) {
        console.error('[Tauri] Native audio failed:', err)
        setRtcError('네이티브 오디오 시작 실패. 웹 오디오로 전환합니다.')
        // 실패 시 웹 오디오로 fallback
      }
    }

    // 웹 브라우저: WebRTC 사용
    if (typeof navigator === 'undefined' || !navigator.mediaDevices) {
      setRtcError('이 브라우저는 오디오 공유를 지원하지 않습니다.')
      setRtcStatus('error')
      return
    }
    try {
      // 오디오 설정에서 선택한 장치와 설정 사용
      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: audioSettings.inputDeviceId ? { exact: audioSettings.inputDeviceId } : undefined,
          sampleRate: audioSettings.sampleRate,
          channelCount: audioSettings.channelCount,
          echoCancellation: audioSettings.echoCancellation,
          noiseSuppression: audioSettings.noiseSuppression,
          autoGainControl: audioSettings.autoGainControl,
        },
      }
      const rawStream = await navigator.mediaDevices.getUserMedia(constraints)

      // 스테레오 입력을 모노로 합치는 처리 (channelCount가 2이고 stereoMode가 'mono'인 경우)
      let stream = rawStream
      const audioTrack = rawStream.getAudioTracks()[0]
      const trackSettings = audioTrack?.getSettings()

      if (trackSettings?.channelCount === 2 && audioSettings.stereoMode === 'mono') {
        // Web Audio API로 스테레오 → 모노 변환 (최소 레이턴시)
        const audioContext = new AudioContext({
          sampleRate: audioSettings.sampleRate,
          latencyHint: 'interactive'
        })
        const source = audioContext.createMediaStreamSource(rawStream)

        // 스테레오를 모노로 믹싱하는 노드 생성
        const merger = audioContext.createChannelMerger(1)
        const splitter = audioContext.createChannelSplitter(2)
        const gainL = audioContext.createGain()
        const gainR = audioContext.createGain()

        // L/R 각각 0.5 볼륨으로 믹싱 (클리핑 방지)
        gainL.gain.value = 0.5
        gainR.gain.value = 0.5

        source.connect(splitter)
        splitter.connect(gainL, 0)  // Left channel
        splitter.connect(gainR, 1)  // Right channel
        gainL.connect(merger, 0, 0)
        gainR.connect(merger, 0, 0)

        // 모노 스트림 생성
        const destination = audioContext.createMediaStreamDestination()
        merger.connect(destination)

        stream = destination.stream
        console.log('[RTC] Stereo input converted to mono')
      }

      setLocalStream(stream)
      // ref도 즉시 업데이트 (createOfferForPeer에서 사용)
      localStreamRef.current = stream

      // 실제 적용된 설정 가져오기 (원본 스트림 기준)
      const finalAudioTrack = stream.getAudioTracks()[0]
      if (finalAudioTrack) {
        // MediaTrackSettings 타입에 latency가 표준에는 없지만 일부 브라우저에서 지원
        const finalTrackSettings = finalAudioTrack.getSettings() as MediaTrackSettings & { latency?: number }
        setActualStreamSettings({
          deviceId: trackSettings?.deviceId ?? null,  // 원본 rawStream의 설정 사용
          sampleRate: finalTrackSettings.sampleRate ?? null,
          channelCount: finalTrackSettings.channelCount ?? null,
          echoCancellation: trackSettings?.echoCancellation ?? null,
          noiseSuppression: trackSettings?.noiseSuppression ?? null,
          autoGainControl: trackSettings?.autoGainControl ?? null,
          latency: (trackSettings as MediaTrackSettings & { latency?: number })?.latency ?? null,
        })
      }

      setRtcError(null)

      // 방에 있는 모든 피어에게 offer 전송 (연주자가 되면 관람자들도 내 소리를 들어야 함)
      const currentPeers = peersRef.current
      console.log('[RTC] Starting mic, will send offers to', currentPeers.length, 'peers')

      if (currentPeers.length > 0) {
        let hasConnectedPeer = false
        for (const peerId of currentPeers) {
          const existingPc = peerConnections.current.get(peerId)
          if (existingPc) {
            // 기존 연결이 있으면 트랙 추가하고 renegotiation
            attachLocalTracks(existingPc, stream)
            if (existingPc.connectionState === 'connected') {
              hasConnectedPeer = true
            }
            try {
              const offer = await existingPc.createOffer()
              await existingPc.setLocalDescription(offer)
              sendSignalMessage({ type: 'offer', to: peerId, offer })
              console.log('[RTC] Renegotiation offer sent to:', peerId.slice(0, 8))
            } catch (err) {
              console.error('[RTC] Renegotiation failed:', err)
            }
          } else {
            // 연결이 없으면 새로 만들어서 offer 전송
            console.log('[RTC] Creating new connection for peer:', peerId.slice(0, 8))
            void createOfferForPeer(peerId)
          }
        }
        setRtcStatus(hasConnectedPeer ? 'live' : 'connecting')
      } else {
        setRtcStatus('live')
      }
    } catch (err) {
      console.error('마이크 접근 실패:', err)
      setRtcError('마이크 권한이 필요합니다. 오디오 설정에서 장치를 확인해주세요.')
      setRtcStatus('error')
    }
  }

  const stopLocalMic = (notifyPeers = true) => {
    // Tauri 네이티브 오디오 사용 중인 경우
    if (nativeAudioActiveRef.current) {
      console.log('[Tauri] Stopping native audio...')

      // 비동기 정리 작업
      void (async () => {
        try {
          await stopAudio()
          console.log('[Tauri] Audio stopped')
        } catch (err) {
          console.error('[Tauri] Failed to stop audio:', err)
        }

        try {
          await shutdownNetwork()
          console.log('[Tauri] Network shutdown')
        } catch (err) {
          console.error('[Tauri] Failed to shutdown network:', err)
        }
      })()

      setUsingNativeAudio(false)
      setNativeNetworkInfo(null)
      nativeNetworkInfoRef.current = null
      nativeAudioActiveRef.current = false
      tauriPeersRef.current.clear()
      p2pConnectingRef.current.clear()
      setRtcStatus('idle')

      // 내 악기 정보 초기화
      setMyInstrumentState(null)
      myInstrumentRef.current = null

      // 내 악기 정보를 peerInstruments에서 제거
      const myId = clientIdRef.current
      if (myId) {
        setPeerInstruments(prev => {
          const next = { ...prev }
          delete next[myId]
          return next
        })
      }

      // 다른 피어들에게 연주 중단 알림
      if (notifyPeers) {
        sendSignalMessage({ type: 'stop-performing' })
      }
      return
    }

    // 웹 오디오 사용 중인 경우
    if (!localStream) return
    localStream.getTracks().forEach((track) => track.stop())
    peerConnections.current.forEach((pc) => {
      pc.getSenders().forEach((sender) => {
        if (sender.track && sender.track.kind === 'audio') {
          sender.replaceTrack(null).catch(() => {})
        }
      })
    })
    setLocalStream(null)
    setActualStreamSettings(null)
    if (peerConnections.current.size === 0) {
      setRtcStatus('idle')
    }

    // 내 악기 정보 초기화
    setMyInstrumentState(null)
    myInstrumentRef.current = null

    // 내 악기 정보를 peerInstruments에서 제거
    const myId = clientIdRef.current
    if (myId) {
      setPeerInstruments(prev => {
        const next = { ...prev }
        delete next[myId]
        return next
      })
    }

    // 다른 피어들에게 연주 중단 알림
    if (notifyPeers) {
      sendSignalMessage({ type: 'stop-performing' })
    }
  }

  // 로컬 마이크 뮤트/언뮤트 (연주자 상태 유지)
  const toggleLocalMute = () => {
    if (!localStream) return
    const audioTrack = localStream.getAudioTracks()[0]
    if (audioTrack) {
      audioTrack.enabled = !audioTrack.enabled
      setLocalMuted(!audioTrack.enabled)
      console.log('[AUDIO] Local mic muted:', !audioTrack.enabled)
    }
  }

  // 마스터 볼륨 뮤트/언뮤트
  const toggleMasterMute = () => {
    const newMuted = !masterMuted
    setMasterMuted(newMuted)
    // 모든 오디오 노드에 적용
    audioNodesRef.current.forEach((nodes, oderId) => {
      const settings = mixSettingsMap[oderId]
      if (newMuted) {
        nodes.gain.gain.value = 0
      } else {
        nodes.gain.gain.value = settings?.muted ? 0 : (settings?.volume ?? 1) * masterVolume
      }
    })
    console.log('[AUDIO] Master muted:', newMuted)
  }

  // 입력 테스트 모드 변경 시 합주실 오디오 일시 중지/재개
  useEffect(() => {
    isInputTestingRef.current = isInputTesting

    if (isInputTesting) {
      // 입력 테스트 시작: 현재 상태 저장 후 뮤트
      prevLocalMutedRef.current = localMuted

      // 로컬 마이크 뮤트 (웹)
      if (localStream) {
        const audioTrack = localStream.getAudioTracks()[0]
        if (audioTrack) {
          audioTrack.enabled = false
          setLocalMuted(true)
        }
      }

      // 마스터 볼륨 뮤트 (상대방 소리 안 들림)
      if (!masterMuted) {
        setMasterMuted(true)
        audioNodesRef.current.forEach((nodes) => {
          nodes.gain.gain.value = 0
        })
      }

      console.log('[ROOM] Input testing started - muted room audio')
    } else {
      // 입력 테스트 종료: 이전 상태로 복원
      if (localStream && !prevLocalMutedRef.current) {
        const audioTrack = localStream.getAudioTracks()[0]
        if (audioTrack) {
          audioTrack.enabled = true
          setLocalMuted(false)
        }
      }

      // 마스터 볼륨 복원 (이전에 뮤트가 아니었으면)
      setMasterMuted(false)
      audioNodesRef.current.forEach((nodes, oderId) => {
        const settings = mixSettingsMap[oderId]
        nodes.gain.gain.value = settings?.muted ? 0 : (settings?.volume ?? 1) * masterVolume
      })

      console.log('[ROOM] Input testing ended - restored room audio')
    }
  }, [isInputTesting]) // localMuted, masterMuted 등은 의존성에서 제외 (무한루프 방지)

  const joinRoom = (roomId: string, isHost?: boolean) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) {
      setJoinFeedback('시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.')
      return
    }
    // userId를 포함하여 서버에서 중복 연결 처리 가능하도록
    wsRef.current.send(JSON.stringify({
      type: 'join',
      roomId,
      nickname,
      isHost: isHost || false,
      userId: user?.id || null
    }))
    setCurrentRoomId(roomId)
    // 재연결용 ref 업데이트
    currentRoomIdRef.current = roomId
    isHostRef.current = isHost || false
    setJoinFeedback('룸 입장 시도 중...')
  }

  const leaveRoom = () => {
    // 서버에 나가기 알림
    sendSignalMessage({ type: 'leave' })
    teardownPeerConnections()
    stopLocalMic(false) // 이미 leave로 알렸으므로 중복 알림 방지
    setPeers([])
    setCurrentRoomId(null)
    // 재연결용 ref 초기화 (의도적으로 나간 경우 재연결 안 함)
    currentRoomIdRef.current = null
    isHostRef.current = false
    setJoinFeedback('')
    setChatMessages([])
    // 오디오 노드 정리
    audioNodesRef.current.forEach((nodes) => {
      nodes.source.disconnect()
      nodes.analyser.disconnect()
      nodes.gain.disconnect()
      nodes.panner.disconnect()
    })
    audioNodesRef.current.clear()
    setMixSettingsMap({})
    // 악기 정보 초기화
    setPeerInstruments({})
    setMyInstrumentState(null)
    myInstrumentRef.current = null
    // 네트워크 상태 초기화
    setPeerNetworkStats({})
    // 연주 요청 상태 초기화
    setPendingRequests([])
    setMyRequestStatus('none')
    setMyRequestInstrument(null)
    // 녹음 중지 (진행 중인 경우)
    if (mediaRecorderRef.current && isRecording) {
      mediaRecorderRef.current.stop()
      mediaRecorderRef.current = null
      setIsRecording(false)
    }
    if (recordingTimerRef.current) {
      clearInterval(recordingTimerRef.current)
      recordingTimerRef.current = null
    }
    setRecordingDuration(0)
    // 녹음 destination 해제
    recordingDestinationRef.current = null
    // 미니 플레이어 모드 해제
    isMiniPlayerModeRef.current = false
    setIsMiniPlayerMode(false)
  }

  // 기본 믹서 설정
  const defaultMixSettings: MixSettings = { volume: 1, pan: 0, muted: false }

  // 개인 믹서 함수들 (Web Audio API 사용)
  const setMixVolume = (peerId: string, volume: number) => {
    const clampedVolume = Math.max(0, Math.min(1, volume))
    const nodes = audioNodesRef.current.get(peerId)
    const context = audioContextRef.current
    console.log('[MIXER] setMixVolume called:', {
      peerId: peerId.slice(0, 8),
      volume: clampedVolume,
      hasNodes: !!nodes,
      contextState: context?.state,
      nodesCount: audioNodesRef.current.size
    })
    if (nodes) {
      const isMuted = masterMuted || (mixSettingsMap[peerId]?.muted ?? false)
      nodes.gain.gain.value = isMuted ? 0 : clampedVolume * masterVolume
      console.log('[MIXER] Volume applied:', nodes.gain.gain.value)
    } else {
      console.warn('[MIXER] No nodes found for peer:', peerId.slice(0, 8))
    }
    setMixSettingsMap(prev => ({
      ...prev,
      [peerId]: { ...defaultMixSettings, ...prev[peerId], volume: clampedVolume }
    }))
  }

  const setMixPan = (peerId: string, pan: number) => {
    const clampedPan = Math.max(-1, Math.min(1, pan))
    const nodes = audioNodesRef.current.get(peerId)
    if (nodes) {
      // 마스터 패닝과 개별 패닝을 합산 (클램핑)
      const combinedPan = Math.max(-1, Math.min(1, clampedPan + masterPan))
      nodes.panner.pan.value = combinedPan
      console.log('[MIXER] Pan set for', peerId.slice(0, 8), ':', clampedPan, '+ master', masterPan, '-> pan:', combinedPan)
    }
    setMixSettingsMap(prev => ({
      ...prev,
      [peerId]: { ...defaultMixSettings, ...prev[peerId], pan: clampedPan }
    }))
  }

  const setMixMuted = (peerId: string, muted: boolean) => {
    const nodes = audioNodesRef.current.get(peerId)
    const settings = mixSettingsMap[peerId]
    if (nodes) {
      const volume = settings?.volume ?? 1
      nodes.gain.gain.value = (muted || masterMuted) ? 0 : volume * masterVolume
      console.log('[MIXER] Mute set for', peerId.slice(0, 8), ':', muted, '-> gain:', nodes.gain.gain.value)
    }
    setMixSettingsMap(prev => ({
      ...prev,
      [peerId]: { ...defaultMixSettings, ...prev[peerId], muted }
    }))
  }

  // 마스터 볼륨/뮤트/패닝 변경 시 모든 Web Audio 노드 업데이트
  useEffect(() => {
    audioNodesRef.current.forEach((nodes, peerId) => {
      const settings = mixSettingsMap[peerId]
      const volume = settings?.volume ?? 1
      const pan = settings?.pan ?? 0
      const muted = settings?.muted ?? false

      // 볼륨 업데이트
      nodes.gain.gain.value = (masterMuted || muted) ? 0 : volume * masterVolume

      // 패닝 업데이트
      const combinedPan = Math.max(-1, Math.min(1, pan + masterPan))
      nodes.panner.pan.value = combinedPan
    })
  }, [masterVolume, masterMuted, masterPan, mixSettingsMap])

  // remoteAudioMap 변경 시 오디오 노드 정리
  useEffect(() => {
    audioNodesRef.current.forEach((_, peerId) => {
      if (!remoteAudioMap[peerId]) {
        unregisterAudioStream(peerId)
      }
    })
  }, [remoteAudioMap, unregisterAudioStream])

  // 미니플레이어 모드일 때만 자동으로 Web Audio 연결 및 숨겨진 audio 요소 생성
  // (일반 모드에서는 RemoteAudio 컴포넌트가 처리)
  useEffect(() => {
    // ref를 사용 (상태는 비동기 업데이트라 타이밍 이슈 발생 가능)
    const isInMiniMode = isMiniPlayerModeRef.current

    // 미니플레이어 모드가 아니면 숨겨진 audio 요소만 정리하고 리턴
    if (!isInMiniMode) {
      // 미니플레이어 모드에서 일반 모드로 전환 시 숨겨진 audio 요소 정리
      if (hiddenAudioElementsRef.current.size > 0) {
        console.log('[AUDIO] Cleaning up hidden audio elements (not in mini mode)')
        hiddenAudioElementsRef.current.forEach((audio, peerId) => {
          audio.srcObject = null
          audio.remove()
          console.log('[AUDIO] Cleaned up hidden audio:', peerId.slice(0, 8))
        })
        hiddenAudioElementsRef.current.clear()
      }
      return
    }

    // 미니플레이어 모드: 새 스트림에 대해 Web Audio 연결 + 숨겨진 audio 요소 생성
    console.log('[AUDIO] Mini player mode - processing', Object.keys(remoteAudioMap).length, 'streams')
    Object.entries(remoteAudioMap).forEach(([peerId, stream]) => {
      // Web Audio 연결 (아직 없는 경우에만)
      if (!audioNodesRef.current.has(peerId)) {
        console.log('[AUDIO] Mini player - auto-connecting stream:', peerId.slice(0, 8))
        connectToWebAudio(peerId, stream)
      }

      // 숨겨진 audio 요소 생성
      if (!hiddenAudioElementsRef.current.has(peerId)) {
        const audio = document.createElement('audio')
        audio.srcObject = stream
        audio.muted = true // Web Audio가 실제 출력 담당
        audio.autoplay = true
        audio.setAttribute('playsinline', 'true')

        audio.play().catch(err => {
          console.log('[AUDIO] Hidden audio play failed:', err.message)
        })

        hiddenAudioElementsRef.current.set(peerId, audio)
        console.log('[AUDIO] Created hidden audio element for:', peerId.slice(0, 8))
      }
    })

    // 제거된 스트림의 숨겨진 audio 요소 정리
    hiddenAudioElementsRef.current.forEach((audio, peerId) => {
      if (!remoteAudioMap[peerId]) {
        audio.srcObject = null
        audio.remove()
        hiddenAudioElementsRef.current.delete(peerId)
        console.log('[AUDIO] Removed hidden audio element for:', peerId.slice(0, 8))
      }
    })
  }, [remoteAudioMap, isMiniPlayerMode])


  // 채팅 메시지 전송
  const sendChatMessage = (message: string) => {
    if (!message.trim() || !wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return

    const chatPayload = {
      type: 'chat',
      message: message.trim(),
      nickname,
      timestamp: Date.now()
    }
    wsRef.current.send(JSON.stringify(chatPayload))

    // 내 메시지도 바로 추가
    setChatMessages(prev => [...prev, {
      id: `${clientId}-${Date.now()}`,
      peerId: clientId || 'me',
      nickname,
      message: message.trim(),
      timestamp: Date.now()
    }])
  }

  // 내 악기 설정 및 연주 시작 알림
  const setMyInstrument = (instrument: string, isHost?: boolean) => {
    setMyInstrumentState(instrument)
    myInstrumentRef.current = instrument

    // 서버에 연주 시작 알림 (서버가 방 전체에 브로드캐스트)
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify({
        type: 'start-performing',
        instrument,
        nickname,
        isHost: isHost || false
      }))
      console.log('[WS] Sent start-performing with isHost:', isHost)
    }

    // 내 악기 정보도 peerInstruments에 추가
    if (clientId) {
      setPeerInstruments(prev => ({
        ...prev,
        [clientId]: { peerId: clientId, instrument, nickname, isHost: isHost || false }
      }))
    }
  }

  // 연주 참여 요청 (관람자가 방장에게 요청)
  const requestPerform = (instrument: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'request-perform',
      instrument
    }))
    setMyRequestInstrument(instrument)
    console.log('[REQUEST] Sent perform request for:', instrument)
  }

  // 요청 취소
  const cancelRequest = () => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({ type: 'cancel-perform-request' }))
    setMyRequestStatus('none')
    setMyRequestInstrument(null)
    console.log('[REQUEST] Cancelled perform request')
  }

  // 악기 변경 요청 (연주자가 방장에게 요청)
  const requestInstrumentChange = (newInstrument: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'request-instrument-change',
      newInstrument
    }))
    setMyNewInstrument(newInstrument)
    console.log('[INSTRUMENT-CHANGE] Sent instrument change request for:', newInstrument)
  }

  // 악기 변경 요청 취소
  const cancelInstrumentChangeRequest = () => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({ type: 'cancel-instrument-change-request' }))
    setMyInstrumentChangeStatus('none')
    setMyNewInstrument(null)
    console.log('[INSTRUMENT-CHANGE] Cancelled instrument change request')
  }

  // 악기 변경 요청 승인 (방장)
  const approveInstrumentChange = (oderId: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'approve-instrument-change',
      targetId: oderId
    }))
    setPendingInstrumentChanges(prev => prev.filter(r => r.oderId !== oderId))
    console.log('[INSTRUMENT-CHANGE] Approved instrument change for:', oderId)
  }

  // 악기 변경 요청 거절 (방장)
  const rejectInstrumentChange = (oderId: string, reason?: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'reject-instrument-change',
      targetId: oderId,
      reason
    }))
    setPendingInstrumentChanges(prev => prev.filter(r => r.oderId !== oderId))
    console.log('[INSTRUMENT-CHANGE] Rejected instrument change for:', oderId)
  }

  // === 녹음 기능 ===

  // 지원되는 MIME 타입 찾기
  const getSupportedMimeType = (): string => {
    const types = [
      'audio/webm;codecs=opus',
      'audio/webm',
      'audio/ogg;codecs=opus',
      'audio/mp4',
      'audio/mpeg',
      ''  // 빈 문자열은 브라우저 기본값 사용
    ]
    for (const type of types) {
      if (type === '' || MediaRecorder.isTypeSupported(type)) {
        console.log('[RECORDING] Using MIME type:', type || 'browser default')
        return type
      }
    }
    return ''
  }

  // 녹음 시작
  const startRecording = async () => {
    if (isRecording) return

    // AudioContext가 suspended 상태면 resume
    const context = audioContextRef.current
    if (context && context.state === 'suspended') {
      console.log('[RECORDING] Resuming suspended AudioContext')
      await context.resume()
    }

    // 연결된 오디오 노드가 있는지 확인
    const connectedNodes = audioNodesRef.current.size
    console.log('[RECORDING] Connected audio nodes:', connectedNodes)

    if (connectedNodes === 0) {
      console.warn('[RECORDING] No audio sources connected')
      alert('녹음할 오디오가 없습니다. 연주자의 오디오가 연결되어 있어야 합니다.')
      return
    }

    // 녹음 destination 가져오기 (없으면 새로 생성)
    const recordingDest = getRecordingDestination()

    // 모든 기존 오디오 노드를 녹음 destination에 연결 (혹시 연결 안 되어 있으면)
    audioNodesRef.current.forEach((nodes, peerId) => {
      try {
        // 기존 연결 해제 후 다시 연결 (중복 연결 방지)
        try {
          nodes.panner.disconnect(recordingDest)
        } catch {
          // 연결되어 있지 않았으면 무시
        }
        nodes.panner.connect(recordingDest)
        console.log('[RECORDING] Connected panner to recording dest for:', peerId.slice(0, 8))
      } catch (err) {
        console.error('[RECORDING] Failed to connect panner:', peerId.slice(0, 8), err)
      }
    })

    const stream = recordingDest.stream

    // 오디오 트랙 확인
    const audioTracks = stream.getAudioTracks()
    console.log('[RECORDING] Audio tracks:', audioTracks.length, audioTracks.map(t => ({
      enabled: t.enabled,
      readyState: t.readyState,
      muted: t.muted
    })))

    if (audioTracks.length === 0) {
      console.warn('[RECORDING] No audio tracks available for recording')
      alert('녹음할 오디오 트랙이 없습니다.')
      return
    }

    try {
      const mimeType = getSupportedMimeType()
      recordingMimeTypeRef.current = mimeType

      // MediaRecorder 옵션 설정
      const options: MediaRecorderOptions = {}
      if (mimeType) {
        options.mimeType = mimeType
      }

      const recorder = new MediaRecorder(stream, options)
      mediaRecorderRef.current = recorder
      recordingChunksRef.current = []
      recordingStartTimeRef.current = Date.now()

      // 실제 사용되는 MIME 타입 확인
      const actualMimeType = recorder.mimeType
      console.log('[RECORDING] Actual recorder mimeType:', actualMimeType)
      recordingMimeTypeRef.current = actualMimeType

      recorder.ondataavailable = (event) => {
        console.log('[RECORDING] Data available:', event.data.size, 'bytes, chunks:', recordingChunksRef.current.length)
        if (event.data.size > 0) {
          recordingChunksRef.current.push(event.data)
        }
      }

      recorder.onstop = () => {
        const actualType = recordingMimeTypeRef.current || 'audio/webm'
        const chunks = recordingChunksRef.current

        console.log('[RECORDING] Stopping. Total chunks:', chunks.length, 'Total size:', chunks.reduce((acc, c) => acc + c.size, 0))

        if (chunks.length === 0) {
          console.warn('[RECORDING] No data recorded')
          alert('녹음된 데이터가 없습니다. 연주자의 소리가 들리는지 확인해주세요.')
          return
        }

        const blob = new Blob(chunks, { type: actualType })
        const url = URL.createObjectURL(blob)
        const duration = Math.floor((Date.now() - recordingStartTimeRef.current) / 1000)

        console.log('[RECORDING] Created blob:', blob.size, 'bytes, type:', blob.type)

        const newRecording: Recording = {
          id: `rec-${Date.now()}`,
          blob,
          url,
          timestamp: recordingStartTimeRef.current,
          duration,
          mimeType: actualType
        }

        setRecordings(prev => [...prev, newRecording])
        console.log('[RECORDING] Recording saved:', newRecording.id, 'duration:', duration, 's', 'size:', blob.size)

        // 타이머 정리
        if (recordingTimerRef.current) {
          clearInterval(recordingTimerRef.current)
          recordingTimerRef.current = null
        }
        setRecordingDuration(0)
      }

      recorder.onerror = (event) => {
        console.error('[RECORDING] Recorder error:', event)
      }

      // 녹음 시작 (500ms마다 데이터 수집)
      recorder.start(500)
      setIsRecording(true)

      // 녹음 시간 타이머
      recordingTimerRef.current = window.setInterval(() => {
        setRecordingDuration(Math.floor((Date.now() - recordingStartTimeRef.current) / 1000))
      }, 1000)

      console.log('[RECORDING] Started recording, recorder state:', recorder.state)
    } catch (err) {
      console.error('[RECORDING] Failed to start recording:', err)
      alert('녹음을 시작할 수 없습니다: ' + (err instanceof Error ? err.message : '알 수 없는 오류'))
    }
  }

  // 녹음 중지
  const stopRecording = () => {
    if (!isRecording || !mediaRecorderRef.current) return

    mediaRecorderRef.current.stop()
    mediaRecorderRef.current = null
    setIsRecording(false)
    console.log('[RECORDING] Stopped recording')
  }

  // 녹음 삭제
  const deleteRecording = (id: string) => {
    setRecordings(prev => {
      const recording = prev.find(r => r.id === id)
      if (recording) {
        URL.revokeObjectURL(recording.url) // 메모리 해제
      }
      return prev.filter(r => r.id !== id)
    })
    console.log('[RECORDING] Deleted recording:', id)
  }

  // === 미니 플레이어 모드 함수 ===

  // 미니 플레이어 모드로 전환 (RoomDetail 페이지를 떠날 때 호출)
  const enterMiniPlayerMode = () => {
    // 방으로 돌아가는 중이면 미니 플레이어 모드로 전환하지 않음
    if (isReturningToRoomRef.current) {
      console.log('[MINI] Skipping enter mini player mode - returning to room')
      isReturningToRoomRef.current = false
      return
    }
    if (!currentRoomId) {
      console.log('[MINI] Cannot enter mini player mode - not in a room')
      return
    }
    console.log('[MINI] Entering mini player mode, room:', currentRoomId.slice(0, 8))
    isMiniPlayerModeRef.current = true
    setIsMiniPlayerMode(true)
  }

  // 미니 플레이어 모드 종료 및 방 나가기
  const exitMiniPlayerMode = () => {
    console.log('[MINI] Exiting mini player mode and leaving room')
    isMiniPlayerModeRef.current = false
    setIsMiniPlayerMode(false)
    leaveRoom()
  }

  // 미니 플레이어에서 방으로 돌아가기 (현재 방 ID 반환)
  const returnToRoom = (): string | null => {
    if (!currentRoomId) {
      console.log('[MINI] Cannot return to room - no current room')
      return null
    }
    console.log('[MINI] Returning to room:', currentRoomId.slice(0, 8))
    // cleanup에서 다시 미니 모드로 전환되지 않도록 플래그 설정
    isReturningToRoomRef.current = true
    isMiniPlayerModeRef.current = false
    setIsMiniPlayerMode(false)
    return currentRoomId
  }

  // 미니 플레이어 모드만 해제 (방은 유지)
  const clearMiniPlayerMode = () => {
    if (isMiniPlayerMode) {
      console.log('[MINI] Clearing mini player mode')
      isReturningToRoomRef.current = true
      isMiniPlayerModeRef.current = false
      setIsMiniPlayerMode(false)
    }
  }

  // Tauri 모니터링 토글 (본인 소리 듣기)
  const toggleMonitoring = async () => {
    if (!isTauri() || !nativeAudioActiveRef.current) return

    try {
      const newState = !isMonitoringEnabled
      await setMonitoring(newState)
      setIsMonitoringEnabled(newState)
      console.log('[Tauri] Monitoring:', newState ? 'enabled' : 'disabled')
    } catch (err) {
      console.error('[Tauri] Failed to toggle monitoring:', err)
    }
  }

  // 요청 승인 (방장)
  const approveRequest = (targetId: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'approve-perform',
      targetId
    }))
    // 로컬에서 즉시 제거
    setPendingRequests(prev => prev.filter(r => r.oderId !== targetId))
    console.log('[REQUEST] Approved perform request for:', targetId.slice(0, 8))
  }

  // 요청 거절 (방장)
  const rejectRequest = (targetId: string, reason?: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({
      type: 'reject-perform',
      targetId,
      reason
    }))
    // 로컬에서 즉시 제거
    setPendingRequests(prev => prev.filter(r => r.oderId !== targetId))
    console.log('[REQUEST] Rejected perform request for:', targetId.slice(0, 8))
  }

  // nickname 변경 시 ref 업데이트
  useEffect(() => {
    nicknameRef.current = nickname
  }, [nickname])

  // localStream 변경 시 ref 업데이트
  useEffect(() => {
    localStreamRef.current = localStream
  }, [localStream])

  // peers 변경 시 ref 업데이트
  useEffect(() => {
    peersRef.current = peers
  }, [peers])

  // WebSocket setup
  useEffect(() => {
    if (typeof window === 'undefined' || typeof WebSocket === 'undefined') {
      return
    }

    // StrictMode double-mount 방지용 플래그
    let isMounted = true
    let reconnectTimeout: ReturnType<typeof setTimeout> | null = null
    let reconnectAttempts = 0

    const connect = () => {
      if (import.meta.env.DEV) console.log('[WS] Connecting to', SIGNALING_URL)
      setSignalStatus('connecting')
      const ws = new WebSocket(SIGNALING_URL)
      wsRef.current = ws

      ws.onopen = () => {
        console.log('[WS] Connected!')
        if (isMounted) setSignalStatus('connected')
        reconnectAttempts = 0 // 연결 성공 시 재연결 시도 횟수 리셋
      }
      ws.onerror = () => {
        // 서버 미실행 시 에러 로그 최소화
        if (!isMounted) return
        setSignalStatus('error')
      }
      ws.onclose = (event) => {
        if (import.meta.env.DEV && event.code !== 1006) console.log('[WS] Closed:', event.code, event.reason)
        if (!isMounted) return
        setSignalStatus('idle')
        setClientId(null)
        setPeers([])
        teardownPeerConnections()

        // 방에 입장 중이었고, 정상적인 종료가 아닌 경우에만 재연결 시도
        // code 1000 = 정상 종료, 1005 = 정상 종료 (no status), 그 외 = 비정상
        const shouldReconnect = currentRoomIdRef.current && event.code !== 1000 && event.code !== 1005
        if (shouldReconnect && reconnectAttempts < 3) {
          reconnectAttempts++
          console.log(`[WS] Was in room, attempting reconnect ${reconnectAttempts}/3 in 2s...`)
          reconnectTimeout = setTimeout(() => {
            if (isMounted && currentRoomIdRef.current) {
              connect()
            }
          }, 2000)
        } else if (reconnectAttempts >= 3) {
          console.log('[WS] Max reconnect attempts reached, giving up')
          currentRoomIdRef.current = null
          isHostRef.current = false
        }
      }
      ws.onmessage = (event) => {
        if (!isMounted) return
        try {
          const payload = JSON.parse(event.data)
          if (payload.type === 'welcome') {
            setClientId(payload.clientId)
            clientIdRef.current = payload.clientId

            // 재연결 후 이전 방에 자동으로 다시 join
            if (currentRoomIdRef.current) {
              console.log('[WS] Reconnected, rejoining room:', currentRoomIdRef.current.slice(0, 8))
              ws.send(JSON.stringify({
                type: 'join',
                roomId: currentRoomIdRef.current,
                nickname: nicknameRef.current,
                isHost: isHostRef.current,
                userId: user?.id || null,
                isRejoin: true
              }))
            }
            return
          }
        // 새로운 서버 메시지: 방 상태 (입장 시 수신)
        if (payload.type === 'room-state') {
          const peerList: string[] = Array.isArray(payload.peerIds) ? payload.peerIds : []
          const participants = Array.isArray(payload.participants) ? payload.participants : []
          console.log('[WS] Received room-state, peers:', peerList.length, 'participants:', participants.length)

          setPeers(peerList)
          setJoinFeedback(`룸 입장 완료 · 동시 연결 ${peerList.length + 1}명`)

          // 참여자 정보로 peerInstruments 초기화 (연주 중인 사람만)
          const instrumentsMap: Record<string, PeerInstrument> = {}
          for (const p of participants) {
            if (p.isPerforming && p.instrument) {
              instrumentsMap[p.oderId] = {
                peerId: p.oderId,
                instrument: p.instrument,
                nickname: p.nickname,
                isHost: p.isHost || false
              }
            }
          }
          setPeerInstruments(instrumentsMap)
          console.log('[WS] Initialized peerInstruments:', Object.keys(instrumentsMap).length, 'performers')

          // Tauri 앱이고 이미 연주 중이면 P2P discovery 전송
          if (nativeAudioActiveRef.current) {
            console.log('[Tauri] Sending P2P discovery to', peerList.length, 'peers')
            peerList.forEach((peerId) => {
              sendSignalMessage({ type: 'p2p-discovery', to: peerId })
            })
          } else if (localStreamRef.current) {
            // 웹 앱이고 연주 중이면 WebRTC offer 전송
            peerList.forEach((peerId) => {
              console.log('[RTC] Creating offer for peer:', peerId.slice(0, 8))
              void createOfferForPeer(peerId)
            })
          } else {
            console.log('[RTC] Not a performer, waiting for offers from performers')
          }
          return
        }

        // 새로운 참여자 입장
        if (payload.type === 'participant-joined' && payload.participant) {
          const participant = payload.participant
          console.log('[WS] Participant joined:', participant.oderId?.slice(0, 8), participant.nickname)
          setPeers((prev) => (prev.includes(participant.oderId) ? prev : [...prev, participant.oderId]))

          // 연주 중인 참여자라면 악기 정보 추가
          if (participant.isPerforming && participant.instrument) {
            setPeerInstruments(prev => ({
              ...prev,
              [participant.oderId]: {
                peerId: participant.oderId,
                instrument: participant.instrument,
                nickname: participant.nickname,
                isHost: participant.isHost || false
              }
            }))
          }

          // Tauri 앱이고 연주 중이면 새 참여자에게 P2P discovery 전송
          if (nativeAudioActiveRef.current) {
            console.log('[Tauri] Sending P2P discovery to new participant:', participant.oderId?.slice(0, 8))
            sendSignalMessage({ type: 'p2p-discovery', to: participant.oderId })
          } else if (localStreamRef.current) {
            // 웹 앱이고 연주 중이면 WebRTC offer 전송
            console.log('[RTC] I have localStream, creating offer for new participant:', participant.oderId?.slice(0, 8))
            void createOfferForPeer(participant.oderId)
          }
          return
        }

        // 참여자 퇴장
        if (payload.type === 'participant-left' && typeof payload.oderId === 'string') {
          const peerId = payload.oderId
          console.log('[WS] Participant left:', peerId.slice(0, 8))
          setPeers((prev) => prev.filter((id) => id !== peerId))
          closePeerConnection(peerId)
          // 악기 정보 제거
          setPeerInstruments(prev => {
            const next = { ...prev }
            delete next[peerId]
            return next
          })
          // 네트워크 상태 제거
          setPeerNetworkStats(prev => {
            const next = { ...prev }
            delete next[peerId]
            return next
          })
          return
        }

        // 연주자 상태 업데이트 (연주 시작/중단)
        if (payload.type === 'performer-updated' && typeof payload.oderId === 'string') {
          const { oderId, nickname: peerNickname, instrument, isPerforming, isHost: peerIsHost } = payload
          console.log('[WS] Performer updated:', oderId.slice(0, 8), isPerforming ? 'started' : 'stopped', instrument, 'isHost:', peerIsHost)

          if (isPerforming && instrument) {
            // 연주 시작
            setPeerInstruments(prev => ({
              ...prev,
              [oderId]: {
                peerId: oderId,
                instrument,
                nickname: peerNickname || `User ${oderId.slice(0, 4)}`,
                isHost: peerIsHost || false
              }
            }))

            // 새 연주자가 나타나면 연결 시작
            if (nativeAudioActiveRef.current) {
              // Tauri: P2P discovery 전송
              if (!tauriPeersRef.current.has(oderId)) {
                console.log('[Tauri] New performer detected, sending P2P discovery:', oderId.slice(0, 8))
                sendSignalMessage({ type: 'p2p-discovery', to: oderId })
              }
            } else if (!peerConnections.current.has(oderId)) {
              // 웹: WebRTC offer 전송
              console.log('[RTC] New performer detected, creating offer for:', oderId.slice(0, 8))
              void createOfferForPeer(oderId)
            }
          } else {
            // 연주 중단 - 악기 정보 제거
            setPeerInstruments(prev => {
              const next = { ...prev }
              delete next[oderId]
              return next
            })
          }
          return
        }

        // 하위 호환용: 기존 peers 메시지
        if (payload.type === 'peers') {
          const peerList: string[] = Array.isArray(payload.peerIds) ? payload.peerIds : []
          console.log('[WS] Received peers (legacy):', peerList)
          setPeers(peerList)
          setJoinFeedback(`룸 입장 완료 · 동시 연결 ${peerList.length + 1}명`)
          if (nativeAudioActiveRef.current) {
            peerList.forEach((peerId) => {
              sendSignalMessage({ type: 'p2p-discovery', to: peerId })
            })
          } else {
            peerList.forEach((peerId) => {
              void createOfferForPeer(peerId)
            })
          }
          return
        }

        // 하위 호환용: 기존 peer-joined 메시지
        if (payload.type === 'peer-joined' && typeof payload.peerId === 'string') {
          setPeers((prev) => (prev.includes(payload.peerId) ? prev : [...prev, payload.peerId]))
          // Tauri 앱이면 P2P discovery 전송
          if (nativeAudioActiveRef.current) {
            sendSignalMessage({ type: 'p2p-discovery', to: payload.peerId })
          }
          return
        }

        // 하위 호환용: 기존 peer-left 메시지
        if (payload.type === 'peer-left' && typeof payload.peerId === 'string') {
          setPeers((prev) => prev.filter((id) => id !== payload.peerId))
          closePeerConnection(payload.peerId)
          setPeerInstruments(prev => {
            const next = { ...prev }
            delete next[payload.peerId]
            return next
          })
          setPeerNetworkStats(prev => {
            const next = { ...prev }
            delete next[payload.peerId]
            return next
          })
          return
        }
        if (payload.type === 'offer' && typeof payload.from === 'string' && payload.offer) {
          void handleRemoteOffer(payload.from, payload.offer)
          return
        }
        if (payload.type === 'answer' && typeof payload.from === 'string' && payload.answer) {
          void handleRemoteAnswer(payload.from, payload.answer)
          return
        }
        if (payload.type === 'ice-candidate' && typeof payload.from === 'string' && payload.candidate) {
          void handleRemoteCandidate(payload.from, payload.candidate)
          return
        }
        // 채팅 메시지 수신 (서버는 oderId로 보냄)
        if (payload.type === 'chat') {
          const senderId = payload.oderId || payload.from
          if (typeof senderId === 'string') {
            setChatMessages(prev => [...prev, {
              id: `${senderId}-${payload.timestamp || Date.now()}`,
              peerId: senderId,
              nickname: payload.nickname || `User ${senderId.slice(0, 4)}`,
              message: payload.message,
              timestamp: payload.timestamp || Date.now()
            }])
          }
          return
        }
        // 하위 호환용: 악기 정보 수신 (레거시)
        if (payload.type === 'instrument' && typeof payload.from === 'string') {
          setPeerInstruments(prev => ({
            ...prev,
            [payload.from]: {
              peerId: payload.from,
              instrument: payload.instrument,
              nickname: payload.nickname || `User ${payload.from.slice(0, 4)}`
            }
          }))
          return
        }

        // === Tauri P2P 피어 발견 ===

        // P2P 발견 요청 수신 (내 address 전송)
        if (payload.type === 'p2p-discovery' && typeof payload.from === 'string') {
          // p2p-discovery를 보내는 피어도 Tauri 앱임을 기록
          tauriPeersRef.current.add(payload.from)
          console.log('[Tauri P2P] Peer sending discovery is Tauri:', payload.from.slice(0, 8))

          // Tauri 앱이고 네이티브 오디오 사용 중일 때만 응답
          const networkInfo = nativeNetworkInfoRef.current
          if (isTauri() && nativeAudioActiveRef.current && networkInfo) {
            // 우선순위: public_addr > lan_addr > local_addr
            const addr = networkInfo.public_addr || networkInfo.lan_addr || networkInfo.local_addr
            if (addr && !addr.startsWith('0.0.0.0')) {
              sendSignalMessage({
                type: 'p2p-discovery-response',
                to: payload.from,
                publicAddr: addr
              })
              console.log('[Tauri P2P] Sent discovery response to:', payload.from.slice(0, 8), 'addr:', addr)
            } else {
              console.warn('[Tauri P2P] No valid address to send:', networkInfo)
            }
          }
          return
        }

        // P2P 발견 응답 수신 (피어의 public address로 연결 시도)
        if (payload.type === 'p2p-discovery-response' && typeof payload.from === 'string' && payload.publicAddr) {
          // 이 피어는 Tauri 앱임을 기록 (WebRTC 대신 P2P 사용)
          tauriPeersRef.current.add(payload.from)

          // 이미 연결 시도 중이면 무시 (중복 방지)
          if (p2pConnectingRef.current.has(payload.from)) {
            return
          }

          // Tauri 앱이고 네이티브 오디오 사용 중일 때만 연결
          if (isTauri() && nativeAudioActiveRef.current) {
            // 연결 시도 중으로 마킹
            p2pConnectingRef.current.add(payload.from)
            console.log('[Tauri P2P] Connecting to peer:', payload.from.slice(0, 8), 'at', payload.publicAddr)
            void (async () => {
              try {
                await connectToPeer(payload.from, payload.publicAddr)
                console.log('[Tauri P2P] Connected to:', payload.from.slice(0, 8))
              } catch (err) {
                console.error('[Tauri P2P] Failed to connect:', err)
                // 실패 시 다시 시도 가능하도록 제거
                p2pConnectingRef.current.delete(payload.from)
              }
            })()
          }
          return
        }

        // === 연주 참여 요청 관련 메시지 ===

        // 요청 전송 확인 (요청자)
        if (payload.type === 'perform-request-sent') {
          setMyRequestStatus('pending')
          console.log('[REQUEST] Request sent, waiting for approval')
          return
        }

        // 요청 에러 (요청자)
        if (payload.type === 'request-perform-error') {
          setMyRequestStatus('none')
          setMyRequestInstrument(null)
          console.log('[REQUEST] Error:', payload.message)
          return
        }

        // 새 요청 수신 (방장)
        if (payload.type === 'perform-request-received' && payload.request) {
          console.log('[REQUEST] Received perform request:', payload.request)
          setPendingRequests(prev => [...prev, payload.request])
          return
        }

        // 요청 취소됨 (방장)
        if (payload.type === 'perform-request-cancelled' && payload.oderId) {
          console.log('[REQUEST] Request cancelled by:', payload.oderId.slice(0, 8))
          setPendingRequests(prev => prev.filter(r => r.oderId !== payload.oderId))
          return
        }

        // 요청 승인됨 (요청자)
        if (payload.type === 'perform-request-approved') {
          console.log('[REQUEST] Request approved! Instrument:', payload.instrument)
          setMyRequestStatus('approved')
          return
        }

        // 요청 거절됨 (요청자)
        if (payload.type === 'perform-request-rejected') {
          console.log('[REQUEST] Request rejected:', payload.reason)
          setMyRequestStatus('rejected')
          return
        }

        // === 악기 변경 요청 관련 메시지 ===

        // 악기 변경 요청 전송 확인 (연주자)
        if (payload.type === 'instrument-change-request-sent') {
          setMyInstrumentChangeStatus('pending')
          console.log('[INSTRUMENT-CHANGE] Request sent, waiting for approval')
          return
        }

        // 악기 변경 요청 수신 (방장)
        if (payload.type === 'instrument-change-request-received' && payload.request) {
          console.log('[INSTRUMENT-CHANGE] Received instrument change request:', payload.request)
          setPendingInstrumentChanges(prev => {
            // 중복 방지
            if (prev.some(r => r.oderId === payload.request.oderId)) return prev
            return [...prev, payload.request]
          })
          return
        }

        // 악기 변경 요청 취소됨 (방장)
        if (payload.type === 'instrument-change-request-cancelled' && payload.oderId) {
          console.log('[INSTRUMENT-CHANGE] Request cancelled by:', payload.oderId.slice(0, 8))
          setPendingInstrumentChanges(prev => prev.filter(r => r.oderId !== payload.oderId))
          return
        }

        // 악기 변경 승인됨 (연주자)
        if (payload.type === 'instrument-change-approved') {
          console.log('[INSTRUMENT-CHANGE] Approved! New instrument:', payload.newInstrument)
          setMyInstrumentChangeStatus('approved')
          // 악기만 업데이트 (start-performing을 다시 보내지 않음)
          setMyInstrumentState(payload.newInstrument)
          myInstrumentRef.current = payload.newInstrument
          // 내 peerInstruments도 업데이트 (닉네임과 isHost 유지)
          const myId = clientIdRef.current
          if (myId) {
            setPeerInstruments(prev => {
              if (!prev[myId]) return prev
              return {
                ...prev,
                [myId]: {
                  ...prev[myId],
                  instrument: payload.newInstrument
                }
              }
            })
          }
          // 상태 초기화
          setTimeout(() => {
            setMyInstrumentChangeStatus('none')
            setMyNewInstrument(null)
          }, 2000)
          return
        }

        // 악기 변경 거절됨 (연주자)
        if (payload.type === 'instrument-change-rejected') {
          console.log('[INSTRUMENT-CHANGE] Rejected:', payload.reason)
          setMyInstrumentChangeStatus('rejected')
          setTimeout(() => {
            setMyInstrumentChangeStatus('none')
            setMyNewInstrument(null)
          }, 3000)
          return
        }

        // 다른 참여자의 악기 변경됨
        if (payload.type === 'participant-instrument-changed' && payload.oderId && payload.instrument) {
          console.log('[INSTRUMENT-CHANGE] Participant changed instrument:', payload.oderId.slice(0, 8), '->', payload.instrument)
          setPeerInstruments(prev => {
            if (!prev[payload.oderId]) return prev
            return {
              ...prev,
              [payload.oderId]: {
                ...prev[payload.oderId],
                instrument: payload.instrument
              }
            }
          })
          return
        }
        } catch {
          // ignore malformed payloads
        }
      }
    }

    // 초기 연결
    connect()

    return () => {
      isMounted = false
      if (reconnectTimeout) {
        clearTimeout(reconnectTimeout)
      }
      // 연결이 완전히 열린 경우에만 close
      const ws = wsRef.current
      if (ws) {
        if (ws.readyState === WebSocket.OPEN) {
          ws.close()
        } else if (ws.readyState === WebSocket.CONNECTING) {
          // 연결 중이면 열리자마자 닫기
          ws.onopen = () => ws.close()
        }
      }
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Update peer connections when local stream changes
  useEffect(() => {
    if (localStream) {
      peerConnections.current.forEach((pc) => attachLocalTracks(pc, localStream))
    }
    return () => {
      localStream?.getTracks().forEach((track) => track.stop())
    }
  }, [localStream])

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      teardownPeerConnections()
    }
  }, [])

  // 피어 연결 네트워크 상태 측정 (2초마다)
  useEffect(() => {
    const measureNetworkStats = async () => {
      const statsUpdate: Record<string, PeerNetworkStats> = {}

      for (const [peerId, pc] of peerConnections.current.entries()) {
        // connectionState 또는 iceConnectionState로 연결 상태 확인
        const isConnected = pc.connectionState === 'connected' ||
                           pc.iceConnectionState === 'connected' ||
                           pc.iceConnectionState === 'completed'

        if (!isConnected) {
          // 연결 대기 중인 피어도 표시
          statsUpdate[peerId] = {
            audioLatency: null,
            jitter: null,
            packetsLost: 0,
            packetLossRate: 0,
            quality: 'unknown'
          }
          continue
        }

        try {
          const stats = await pc.getStats()
          let rtt: number | null = null
          let jitter: number | null = null
          let jitterBufferDelay: number | null = null
          let playoutDelay: number | null = null
          let packetsLost = 0
          let packetsReceived = 0

          stats.forEach((report) => {
            // candidate-pair에서 RTT 가져오기 (여러 상태 허용)
            if (report.type === 'candidate-pair' &&
                (report.state === 'succeeded' || report.nominated === true)) {
              if (typeof report.currentRoundTripTime === 'number') {
                rtt = report.currentRoundTripTime * 1000 // ms로 변환
              }
            }

            // remote-inbound-rtp에서도 RTT 가져오기 시도 (fallback)
            if (report.type === 'remote-inbound-rtp' && rtt === null) {
              if (typeof report.roundTripTime === 'number') {
                rtt = report.roundTripTime * 1000
              }
            }

            // inbound-rtp에서 jitter, 패킷 손실, playout delay 가져오기
            if (report.type === 'inbound-rtp' && report.kind === 'audio') {
              if (typeof report.jitter === 'number') {
                jitter = Math.round(report.jitter * 1000) // ms로 변환
              }
              if (typeof report.packetsLost === 'number') {
                packetsLost = report.packetsLost
              }
              if (typeof report.packetsReceived === 'number') {
                packetsReceived = report.packetsReceived
              }
              // jitterBufferDelay: 지터 버퍼에서 대기한 총 시간
              if (typeof report.jitterBufferDelay === 'number' &&
                  typeof report.jitterBufferEmittedCount === 'number' &&
                  report.jitterBufferEmittedCount > 0) {
                jitterBufferDelay = (report.jitterBufferDelay / report.jitterBufferEmittedCount) * 1000 // ms
              }
              // totalPlayoutDelay: 수신부터 스피커 출력까지의 총 지연 (디코딩+버퍼+재생)
              if (typeof report.totalPlayoutDelay === 'number' &&
                  typeof report.totalSamplesReceived === 'number' &&
                  report.totalSamplesReceived > 0) {
                // 샘플당 평균 playout 지연 (초) → ms로 변환
                // totalPlayoutDelay는 초 단위, totalSamplesReceived는 샘플 수
                // 48000 샘플 = 1초이므로, (totalPlayoutDelay / totalSamplesReceived) * sampleRate = 실제 지연(초)
                const sampleRate = 48000 // 기본 샘플레이트
                playoutDelay = (report.totalPlayoutDelay / report.totalSamplesReceived) * sampleRate * 1000
              }
            }
          })

          const totalPackets = packetsReceived + packetsLost
          const packetLossRate = totalPackets > 0 ? (packetsLost / totalPackets) * 100 : 0

          // 실제 오디오 지연 계산
          // playoutDelay가 있으면 사용 (수신→재생 지연)
          // 여기에 네트워크 단방향 지연(RTT/2)을 더하면 송신→재생 전체 지연
          let audioLatency: number | null = null
          if (rtt !== null) {
            const networkDelay = rtt / 2 // 네트워크 단방향 지연
            // playoutDelay가 있으면 실제 수신→재생 지연 사용, 없으면 jitterBuffer + 5ms 추정
            const localDelay = playoutDelay ?? (jitterBufferDelay ?? 0) + 5
            audioLatency = Math.round(networkDelay + localDelay)
          }

          statsUpdate[peerId] = {
            audioLatency,
            jitter,
            packetsLost,
            packetLossRate: Math.round(packetLossRate * 10) / 10,
            quality: getNetworkQuality(audioLatency, packetLossRate)
          }
        } catch (err) {
          console.error(`Failed to get stats for peer ${peerId}:`, err)
          statsUpdate[peerId] = {
            audioLatency: null,
            jitter: null,
            packetsLost: 0,
            packetLossRate: 0,
            quality: 'unknown'
          }
        }
      }

      if (Object.keys(statsUpdate).length > 0) {
        setPeerNetworkStats(prev => ({ ...prev, ...statsUpdate }))
      }
    }

    // 연결된 피어가 있을 때만 측정
    const interval = setInterval(() => {
      if (peerConnections.current.size > 0) {
        measureNetworkStats()
      }
    }, 2000)

    return () => clearInterval(interval)
  }, [])

  // Tauri P2P 네트워크 정보 주기적 업데이트 (2초마다)
  useEffect(() => {
    if (!usingNativeAudio) return

    const updateNetworkInfo = async () => {
      try {
        const info = await getNetworkInfo()
        setNativeNetworkInfo(info)
        nativeNetworkInfoRef.current = info
      } catch (err) {
        console.error('[Tauri] Failed to get network info:', err)
      }
    }

    const interval = setInterval(updateNetworkInfo, 2000)
    return () => clearInterval(interval)
  }, [usingNativeAudio])

  return (
    <RoomContext.Provider
      value={{
        signalStatus,
        clientId,
        peers,
        joinRoom,
        joinFeedback,
        rtcStatus,
        rtcError,
        localStream,
        remoteAudioMap,
        startLocalMic,
        stopLocalMic,
        localMuted,
        toggleLocalMute,
        currentRoomId,
        leaveRoom,
        actualStreamSettings,
        // 개인 믹서
        mixSettingsMap,
        setMixVolume,
        setMixPan,
        setMixMuted,
        masterVolume,
        setMasterVolume,
        masterMuted,
        toggleMasterMute,
        masterPan,
        setMasterPan,
        // 오디오 레벨
        audioLevels,
        masterLevel,
        resumeAllAudioContexts,
        registerAudioStream,
        unregisterAudioStream,
        // 채팅
        chatMessages,
        sendChatMessage,
        nickname,
        // 연주자 악기 정보
        peerInstruments,
        myInstrument,
        setMyInstrument,
        // 네트워크 상태
        peerNetworkStats,
        // 연주 참여 요청 (방장용)
        pendingRequests,
        approveRequest,
        rejectRequest,
        // 연주 참여 요청 (요청자용)
        myRequestStatus,
        myRequestInstrument,
        requestPerform,
        cancelRequest,
        // 악기 변경 요청 (방장용)
        pendingInstrumentChanges,
        approveInstrumentChange,
        rejectInstrumentChange,
        // 악기 변경 요청 (연주자용)
        myInstrumentChangeStatus,
        myNewInstrument,
        requestInstrumentChange,
        cancelInstrumentChangeRequest,
        // 녹음
        isRecording,
        recordings,
        recordingDuration,
        startRecording,
        stopRecording,
        deleteRecording,
        // 미니 플레이어 모드
        isMiniPlayerMode,
        enterMiniPlayerMode,
        exitMiniPlayerMode,
        returnToRoom,
        clearMiniPlayerMode,
        // Tauri 네이티브 오디오
        usingNativeAudio,
        nativeNetworkInfo,
        isMonitoringEnabled,
        toggleMonitoring,
      }}
    >
      {children}
    </RoomContext.Provider>
  )
}

export function useRoom() {
  const context = useContext(RoomContext)
  if (!context) {
    throw new Error('useRoom must be used within RoomProvider')
  }
  return context
}
