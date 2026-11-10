import { useEffect, useState, useRef, useCallback } from 'react'
import { useParams, useNavigate, Link } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { useRoom } from '../contexts/RoomContext'
import { supabase } from '../lib/supabaseClient'
import { MAX_PERFORMERS, type PeerState, type PeerStats } from '../native/engineApi'
import { RELAY_PATH_HINT, pathLabel, peerPathHint } from '../native/relayLogic'
import { fetchRoomById, setRoomPassword } from '../services/roomApi'
import { describeJoinError, inviteLink, recallRoomPassword, validateRoomPasswordInput } from '../contexts/room/roomAccess'
import { REJECT_REASON_PRESETS } from '../contexts/room/performRequestState'
import './RoomDetail.css'

// 악기 슬롯 타입
interface InstrumentSlot {
  instrument: string
  count: number
}

interface Room {
  id: string
  title: string
  description: string | null
  host_id: string
  max_participants: number
  current_participants: number
  status: string
  genre: string | null
  tags: string[]
  free_join: boolean
  instrument_slots: InstrumentSlot[]
  /** 마이그레이션 008 전이면 undefined */
  is_private?: boolean
  has_password?: boolean
}

// 악기 정보 (아이콘, 이름)
const INSTRUMENT_INFO: Record<string, { icon: string; name: string }> = {
  vocal: { icon: '🎤', name: '보컬' },
  guitar: { icon: '🎸', name: '기타' },
  bass: { icon: '🎸', name: '베이스' },
  keyboard: { icon: '🎹', name: '건반' },
  drums: { icon: '🥁', name: '드럼' },
  other: { icon: '🎵', name: '기타 악기' },
}

const RTC_STATUS_TEXT: Record<string, string> = {
  idle: '대기',
  connecting: '연결 중',
  live: 'LIVE',
  error: '에러',
}

// 엔진 피어 연결 상태 배지
const PEER_STATE_INFO: Record<PeerState, { label: string; className: string }> = {
  connecting: { label: '연결 중', className: 'connecting' },
  connected: { label: '연결됨', className: 'connected' },
  reconnecting: { label: '재연결 중', className: 'reconnecting' },
  failed: { label: '연결 실패', className: 'failed' },
}

const DESKTOP_DOWNLOAD_URL: string | undefined = import.meta.env.VITE_DESKTOP_DOWNLOAD_URL || undefined
const PUBLIC_WEB_URL: string | undefined = import.meta.env.VITE_PUBLIC_WEB_URL || undefined

function formatMs(v: number | null | undefined, digits = 0): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '--'
  return `${v.toFixed(digits)}ms`
}

function peerStatsTitle(ps: PeerStats): string {
  return [
    `상태: ${PEER_STATE_INFO[ps.state]?.label ?? ps.state}`,
    `경로: ${pathLabel(ps.path) ?? '-'}${ps.path === 'relay' ? ` (${RELAY_PATH_HINT})` : ''}`,
    `RTT: ${formatMs(ps.rttMs)}`,
    `지터: ${formatMs(ps.jitterMs, 1)}`,
    `버퍼: ${formatMs(ps.bufferMs)}`,
    `손실: ${ps.lossPct.toFixed(1)}%`,
    `늦은 패킷: ${ps.late} · 은닉: ${ps.concealed}`,
  ].join('\n')
}

const GENRES = [
  '록', '재즈', '블루스', '클래식', '팝', '힙합',
  '일렉트로닉', '포크', '메탈', '펑크', '레게', '기타'
]

const AVAILABLE_INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

const COMMON_TAGS = [
  '초보환영', '경력자', '세션구함', '정기모임',
  '즉흥연주', '커버곡', '자작곡', '녹음가능'
]


export function RoomDetail() {
  const { roomId } = useParams<{ roomId: string }>()
  const { user, loading: authLoading } = useAuth()
  const navigate = useNavigate()
  const {
    signalStatus,
    clientId,
    peers,
    joinRoom,
    joinError,
    roomJoined,
    submitRoomPassword,
    rtcStatus,
    rtcError,
    remoteAudioMap,
    leaveRoom,
    isRoomHost,
    // 연주 (데스크톱 앱)
    canPerform,
    isPerforming,
    performStarting,
    startPerforming,
    stopPerforming,
    engineStats,
    peerEngineStats,
    isMonitoringEnabled,
    monitorGain,
    setMonitor,
    inputMuted,
    toggleInputMuted,
    broadcasterId,
    isBroadcaster,
    broadcastViewerCount,
    // 믹서
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
    // 채팅
    chatMessages,
    sendChatMessage,
    nickname,
    // 악기
    peerInstruments,
    myInstrument,
    // 네트워크 상태
    peerNetworkStats,
    // 연주 참여 요청
    pendingRequests,
    requestNotice,
    dismissRequestNotice,
    approveRequest,
    rejectRequest,
    myRequestStatus,
    myRequestInstrument,
    myRejectReason,
    hostOnline,
    requestPerform,
    cancelRequest,
    dismissRejection,
    // 악기 변경 요청
    pendingInstrumentChanges,
    approveInstrumentChange,
    rejectInstrumentChange,
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
    enterMiniPlayerMode,
    clearMiniPlayerMode,
    currentRoomId: contextRoomId,
  } = useRoom()

  // 네트워크 품질 아이콘
  const QUALITY_ICONS: Record<string, { icon: string; color: string; label: string }> = {
    excellent: { icon: '🟢', color: '#4ade80', label: '최상' },
    good: { icon: '🟢', color: '#4ade80', label: '양호' },
    fair: { icon: '🟡', color: '#facc15', label: '보통' },
    poor: { icon: '🔴', color: '#f87171', label: '불량' },
    unknown: { icon: '⚪', color: '#9ca3af', label: '측정 중' },
  }

  const [room, setRoom] = useState<Room | null>(null)
  const [loading, setLoading] = useState(true)
  // 연주자 여부는 엔진 상태에서 파생 (엔진 시작 실패 시 자동으로 관람자로 남는다)
  const isPerformer = isPerforming
  const [showPendingRequests, setShowPendingRequests] = useState(false)
  const [showRoomInfo, setShowRoomInfo] = useState(false)
  const [showInstrumentSelect, setShowInstrumentSelect] = useState(false)
  const [showRoomSettings, setShowRoomSettings] = useState(false)
  const [showRecordings, setShowRecordings] = useState(false)
  const [showInstrumentChangeModal, setShowInstrumentChangeModal] = useState(false)
  const [hostNickname, setHostNickname] = useState<string | null>(null)
  // 비밀번호 입장
  const [passwordInput, setPasswordInput] = useState('')
  // 초대 링크/방 코드 복사 결과
  const [copyNotice, setCopyNotice] = useState<string | null>(null)
  // 방장: 거절 사유 입력 중인 요청
  const [rejectingId, setRejectingId] = useState<string | null>(null)
  const [rejectReason, setRejectReason] = useState('')
  const [chatInput, setChatInput] = useState('')
  const chatContainerRef = useRef<HTMLDivElement | null>(null)
  const hasJoinedRef = useRef(false)
  // 악기 변경 말풍선 위치 계산을 위한 refs
  const performerRefs = useRef<Record<string, HTMLDivElement | null>>({})
  const performersListRef = useRef<HTMLDivElement | null>(null)
  const [bubblePositions, setBubblePositions] = useState<Record<string, { top: number; left: number }>>({})

  // 말풍선 위치 업데이트 함수
  const updateBubblePositions = useCallback(() => {
    const newPositions: Record<string, { top: number; left: number }> = {}
    pendingInstrumentChanges.forEach(request => {
      const el = performerRefs.current[request.oderId]
      if (el) {
        const rect = el.getBoundingClientRect()
        newPositions[request.oderId] = {
          top: rect.top + rect.height / 2,
          left: rect.right + 8
        }
      }
    })
    setBubblePositions(newPositions)
  }, [pendingInstrumentChanges])

  // 스크롤/리사이즈 시 말풍선 위치 업데이트
  useEffect(() => {
    if (pendingInstrumentChanges.length === 0) return

    updateBubblePositions()

    const performersList = performersListRef.current

    // 스크롤 이벤트 핸들러
    const handleScroll = () => {
      requestAnimationFrame(updateBubblePositions)
    }
    const handleResize = () => {
      requestAnimationFrame(updateBubblePositions)
    }

    // performers-list 스크롤 이벤트 리스너
    performersList?.addEventListener('scroll', handleScroll, { passive: true })
    window.addEventListener('resize', handleResize)
    window.addEventListener('scroll', handleScroll, { passive: true })

    return () => {
      performersList?.removeEventListener('scroll', handleScroll)
      window.removeEventListener('resize', handleResize)
      window.removeEventListener('scroll', handleScroll)
    }
  }, [pendingInstrumentChanges, updateBubblePositions])

  // 방 설정 폼 상태
  const [editTitle, setEditTitle] = useState('')
  const [editDescription, setEditDescription] = useState('')
  const [editGenre, setEditGenre] = useState('')
  const [editMaxParticipants, setEditMaxParticipants] = useState(8)
  const [editFreeJoin, setEditFreeJoin] = useState(true)
  const [editInstrumentSlots, setEditInstrumentSlots] = useState<InstrumentSlot[]>([])
  const [editTags, setEditTags] = useState<string[]>([])
  const [editCustomTag, setEditCustomTag] = useState('')
  const [editIsPrivate, setEditIsPrivate] = useState(false)
  /** 비밀번호: 그대로 / 새로 설정 / 해제 */
  const [editPasswordMode, setEditPasswordMode] = useState<'keep' | 'set' | 'clear'>('keep')
  const [editPassword, setEditPassword] = useState('')
  const [settingsSaving, setSettingsSaving] = useState(false)

  // 방장 여부 (UI 표시용. 실제 권한은 서버가 auth 토큰과 DB로 판정한다)
  const isHost = isRoomHost || !!(user && room?.host_id === user.id)

  // 연주 시작 처리 중 / 결과
  const [performBusy, setPerformBusy] = useState(false)

  // 각 악기별 사용 중인 슬롯 수 계산
  const getInstrumentUsage = (instrumentId: string) => {
    return Object.values(peerInstruments).filter(p => p.instrument === instrumentId).length
  }

  // DB에서 방 정보 가져오기
  useEffect(() => {
    const fetchRoom = async () => {
      if (!roomId || !supabase) {
        setLoading(false)
        return
      }

      try {
        // 비공개 방도 id(초대 링크/방 코드)로는 조회된다
        const data = await fetchRoomById(roomId) as unknown as Room | null
        if (!data) throw new Error('room not found')
        // 기본 악기 슬롯 설정
        const defaultSlots: InstrumentSlot[] = [
          { instrument: 'vocal', count: 1 },
          { instrument: 'guitar', count: 1 },
          { instrument: 'bass', count: 1 },
          { instrument: 'keyboard', count: 1 },
          { instrument: 'drums', count: 1 },
        ]
        setRoom({
          ...data,
          free_join: data.free_join ?? true,
          instrument_slots: data.instrument_slots ?? defaultSlots
        })
      } catch (err) {
        console.error('Failed to fetch room:', err)
        navigate('/rooms')
      } finally {
        setLoading(false)
      }
    }

    fetchRoom()
  }, [roomId, navigate])

  // 방장 닉네임 가져오기
  useEffect(() => {
    const fetchHostNickname = async () => {
      if (!room?.host_id || !supabase) return

      try {
        const { data, error } = await supabase
          .from('profiles')
          .select('nickname')
          .eq('id', room.host_id)
          .single()

        if (error) throw error
        setHostNickname(data?.nickname || '알 수 없음')
      } catch (err) {
        console.error('Failed to fetch host nickname:', err)
        setHostNickname('알 수 없음')
      }
    }

    fetchHostNickname()
  }, [room?.host_id])

  // clearMiniPlayerMode를 ref로 저장 (의존성 문제 방지)
  const clearMiniPlayerModeRef = useRef(clearMiniPlayerMode)
  useEffect(() => {
    clearMiniPlayerModeRef.current = clearMiniPlayerMode
  }, [clearMiniPlayerMode])

  // 방 입장 시 자동으로 joinRoom 호출
  useEffect(() => {
    // 인증 로딩이 완료될 때까지 대기 (isHost 정확히 계산하기 위해)
    if (authLoading) return

    // 이미 같은 방에 연결되어 있으면 (미니 플레이어에서 돌아온 경우 등) joinRoom 불필요
    if (contextRoomId === roomId) {
      if (!hasJoinedRef.current) {
        console.log('[ROOM] Already connected to this room, skipping joinRoom')
        hasJoinedRef.current = true
        // 미니 플레이어 모드 해제
        clearMiniPlayerModeRef.current()
      }
      return
    }

    // 다른 방에 연결되어 있으면 먼저 나가기 (미니 플레이어 모드도 해제됨)
    if (contextRoomId && contextRoomId !== roomId) {
      console.log('[ROOM] Connected to different room, leaving first:', contextRoomId.slice(0, 8))
      leaveRoom()
      hasJoinedRef.current = false
    }

    if (room && roomId && signalStatus === 'connected' && !hasJoinedRef.current) {
      hasJoinedRef.current = true
      // isHost를 여기서 다시 계산 (최신 user 상태 반영)
      const hostFlag = user && room.host_id === user.id
      // 비밀번호 방: 이번 세션에서 입력했던 비밀번호로 먼저 시도 (틀리면 서버가 다시 묻는다)
      joinRoom(roomId, hostFlag || false, hostFlag ? null : recallRoomPassword(roomId))
      console.log('[ROOM] Joining room with isHost:', hostFlag, 'user:', user?.id, 'host_id:', room.host_id)
      // 참여자 수 동기화는 room-state 수신 후 peers 변경 시 자동으로 처리됨
    }
  }, [room, roomId, signalStatus, authLoading, user, joinRoom, leaveRoom, contextRoomId])

  // 함수들을 ref로 저장 (의존성 문제 방지)
  const leaveRoomRef = useRef(leaveRoom)
  const enterMiniPlayerModeRef = useRef(enterMiniPlayerMode)
  const roomJoinedRef = useRef(roomJoined)
  roomJoinedRef.current = roomJoined
  useEffect(() => {
    leaveRoomRef.current = leaveRoom
    enterMiniPlayerModeRef.current = enterMiniPlayerMode
  }, [leaveRoom, enterMiniPlayerMode])

  // 페이지 이탈 시 cleanup - 미니 플레이어 모드로 전환 (방에서 완전히 나가지 않음)
  useEffect(() => {
    if (!roomId) return

    // cleanup: 컴포넌트 언마운트 시
    return () => {
      if (hasJoinedRef.current && !roomJoinedRef.current) {
        // 서버가 아직 입장시키지 않음(비밀번호 화면 등): 미니 플레이어 대신 퇴장
        console.log('[CLEANUP] Page leaving before join completed, leaving:', roomId.slice(0, 8))
        hasJoinedRef.current = false
        leaveRoomRef.current()
      } else if (hasJoinedRef.current) {
        // hasJoinedRef는 유지 (미니 플레이어에서 돌아올 때 재사용)
        console.log('[CLEANUP] Page leaving, entering mini player mode:', roomId.slice(0, 8))
        // leaveRoom 대신 미니 플레이어 모드로 전환
        enterMiniPlayerModeRef.current()
      }
    }
  }, [roomId])


  // 채팅 스크롤 자동 이동
  useEffect(() => {
    if (chatContainerRef.current) {
      chatContainerRef.current.scrollTop = chatContainerRef.current.scrollHeight
    }
  }, [chatMessages])

  // 연주자로 전환 요청 (악기 선택 모달 열기) — 데스크톱 앱 전용
  const handleBecomePerformer = () => {
    if (!user) {
      navigate('/auth', { state: { from: `/rooms/${roomId}` } })
      return
    }
    if (!canPerform) return

    // 악기 선택 모달 열기 (자유참여든 승인필요든 일단 악기 선택)
    setShowInstrumentSelect(true)
  }

  // 엔진 시작 → 성공해야 악기 설정/연주자 전환 (실패하면 관람자로 남음, 에러는 rtcError로 표시)
  const beginPerforming = async (instrumentId: string) => {
    if (performBusy) return
    setPerformBusy(true)
    try {
      await startPerforming(instrumentId)
    } finally {
      setPerformBusy(false)
    }
  }

  // 악기 선택 후 연주 시작 또는 요청
  const handleSelectInstrument = async (instrumentId: string) => {
    setShowInstrumentSelect(false)

    if (room?.free_join || isHost) {
      // 자유참여 또는 방장: 바로 연주 시작
      await beginPerforming(instrumentId)
    } else {
      // 승인 필요: 방장에게 요청 전송
      requestPerform(instrumentId)
    }
  }

  // 승인되면 연주 시작
  const handleStartAfterApproval = async () => {
    if (myRequestInstrument) {
      await beginPerforming(myRequestInstrument)
    }
  }

  // 관람자로 전환
  const handleBecomeViewer = () => {
    void stopPerforming()
    // 요청 중이었다면 취소
    if (myRequestStatus === 'pending') {
      cancelRequest()
    }
  }

  // 비밀번호 입력 후 입장
  const handleSubmitPassword = (e: React.FormEvent) => {
    e.preventDefault()
    const pw = passwordInput
    if (!pw) return
    submitRoomPassword(pw)
    setPasswordInput('')
  }

  // 초대 링크 / 방 코드 복사
  const copyText = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text)
      setCopyNotice(`${label}를 복사했습니다`)
    } catch {
      window.prompt(`${label}를 복사하세요`, text)
      setCopyNotice(null)
    }
    window.setTimeout(() => setCopyNotice(null), 2000)
  }

  // 방장: 요청 거절 (사유 선택)
  const confirmReject = (targetId: string, reason: string) => {
    rejectRequest(targetId, reason)
    setRejectingId(null)
    setRejectReason('')
  }

  // 퇴장
  const handleLeave = () => {
    leaveRoom()
    navigate('/rooms')
  }

  // 참여자 수 동기화 (방장만 가능)
  const handleSyncParticipants = async () => {
    if (!isHost || !roomId || !supabase) return

    // 현재 WebSocket으로 연결된 실제 참여자 수 (peers + 나)
    const actualCount = peers.length + 1

    try {
      const { error } = await supabase
        .from('rooms')
        .update({ current_participants: actualCount })
        .eq('id', roomId)

      if (error) throw error

      // 로컬 상태도 업데이트
      setRoom(prev => prev ? { ...prev, current_participants: actualCount } : null)
      console.log(`[SYNC] Participant count synced to ${actualCount}`)
      alert(`참여자 수가 ${actualCount}명으로 동기화되었습니다.`)
    } catch (err) {
      console.error('[SYNC] Failed to sync participants:', err)
      alert('참여자 수 동기화에 실패했습니다.')
    }
  }

  // 방 삭제 (방장만 가능)
  const handleDeleteRoom = async () => {
    if (!isHost || !roomId || !supabase) return

    const confirmed = window.confirm('정말로 이 합주실을 삭제하시겠습니까?\n삭제하면 되돌릴 수 없습니다.')
    if (!confirmed) return

    try {
      const { error } = await supabase
        .from('rooms')
        .delete()
        .eq('id', roomId)
        .eq('host_id', user!.id) // 방장 본인만 삭제 가능

      if (error) throw error

      leaveRoom()
      navigate('/rooms')
    } catch (err) {
      console.error('Failed to delete room:', err)
      alert('합주실 삭제에 실패했습니다.')
    }
  }

  // 방 설정 모달 열기
  const openRoomSettings = () => {
    if (!room) return
    setEditTitle(room.title)
    setEditDescription(room.description || '')
    setEditGenre(room.genre || '기타')
    setEditMaxParticipants(room.max_participants)
    setEditFreeJoin(room.free_join)
    setEditInstrumentSlots([...room.instrument_slots])
    setEditTags([...(room.tags || [])])
    setEditCustomTag('')
    setEditIsPrivate(room.is_private === true)
    setEditPasswordMode('keep')
    setEditPassword('')
    setShowRoomSettings(true)
  }

  // 악기 슬롯 관리
  const handleSlotCountChange = (instrument: string, count: number) => {
    setEditInstrumentSlots(prev =>
      prev.map(slot =>
        slot.instrument === instrument ? { ...slot, count: Math.max(0, Math.min(10, count)) } : slot
      )
    )
  }

  const handleAddInstrument = (instrumentId: string) => {
    if (!editInstrumentSlots.find(s => s.instrument === instrumentId)) {
      setEditInstrumentSlots(prev => [...prev, { instrument: instrumentId, count: 1 }])
    }
  }

  const handleRemoveInstrument = (instrument: string) => {
    setEditInstrumentSlots(prev => prev.filter(s => s.instrument !== instrument))
  }

  // 태그 토글
  const handleEditTagToggle = (tag: string) => {
    setEditTags(prev =>
      prev.includes(tag) ? prev.filter(t => t !== tag) : [...prev, tag]
    )
  }

  // 커스텀 태그 추가
  const handleAddEditCustomTag = () => {
    const tag = editCustomTag.trim()
    if (tag && !editTags.includes(tag)) {
      setEditTags(prev => [...prev, tag])
      setEditCustomTag('')
    }
  }

  // 방 설정 저장
  const handleSaveRoomSettings = async () => {
    if (!isHost || !roomId || !supabase) return

    if (!editTitle.trim()) {
      alert('방 제목을 입력하세요.')
      return
    }
    if (editPasswordMode === 'set') {
      const problem = validateRoomPasswordInput(editPassword)
      if (problem) {
        alert(problem)
        return
      }
    }

    setSettingsSaving(true)

    try {
      const validSlots = editInstrumentSlots.filter(s => s.count > 0)

      const { error } = await supabase
        .from('rooms')
        .update({
          title: editTitle.trim(),
          description: editDescription.trim() || null,
          genre: editGenre,
          max_participants: editMaxParticipants,
          free_join: editFreeJoin,
          instrument_slots: validSlots,
          tags: editTags,
          // 마이그레이션 008 전(컬럼 없음)에는 보내지 않는다
          ...(room?.is_private !== undefined ? { is_private: editIsPrivate } : {}),
        })
        .eq('id', roomId)
        .eq('host_id', user!.id)

      if (error) throw error

      // 비밀번호는 서버가 해시해 저장한다
      let hasPassword = room?.has_password
      if (editPasswordMode !== 'keep') {
        hasPassword = await setRoomPassword(roomId, editPasswordMode === 'set' ? editPassword : null)
      }

      // 로컬 상태 업데이트
      setRoom(prev => prev ? {
        ...prev,
        title: editTitle.trim(),
        description: editDescription.trim() || null,
        genre: editGenre,
        max_participants: editMaxParticipants,
        free_join: editFreeJoin,
        instrument_slots: validSlots,
        tags: editTags,
        ...(prev.is_private !== undefined ? { is_private: editIsPrivate } : {}),
        has_password: hasPassword,
      } : null)

      setShowRoomSettings(false)
    } catch (err) {
      console.error('Failed to update room settings:', err)
      alert(err instanceof Error && err.message ? `설정 저장에 실패했습니다: ${err.message}` : '설정 저장에 실패했습니다.')
    } finally {
      setSettingsSaving(false)
    }
  }

  // 아직 추가되지 않은 악기 목록
  const availableToAdd = AVAILABLE_INSTRUMENTS.filter(
    inst => !editInstrumentSlots.find(s => s.instrument === inst.id)
  )

  // 총 악기 슬롯 수 계산
  const editTotalSlots = editInstrumentSlots.reduce((sum, s) => sum + s.count, 0)

  // 녹음 시간 포맷
  const formatDuration = (seconds: number) => {
    const mins = Math.floor(seconds / 60)
    const secs = seconds % 60
    return `${mins}:${secs.toString().padStart(2, '0')}`
  }

  // 녹음 파일 다운로드
  const downloadRecording = (recording: { url: string; timestamp: number; duration: number; mimeType: string }) => {
    const date = new Date(recording.timestamp)

    // MIME 타입에 따른 확장자 결정
    let extension = 'webm'
    if (recording.mimeType.includes('mp4')) {
      extension = 'm4a'
    } else if (recording.mimeType.includes('ogg')) {
      extension = 'ogg'
    } else if (recording.mimeType.includes('mpeg') || recording.mimeType.includes('mp3')) {
      extension = 'mp3'
    } else if (recording.mimeType.includes('wav')) {
      extension = 'wav'
    }

    const filename = `recording_${date.getFullYear()}${(date.getMonth() + 1).toString().padStart(2, '0')}${date.getDate().toString().padStart(2, '0')}_${date.getHours().toString().padStart(2, '0')}${date.getMinutes().toString().padStart(2, '0')}.${extension}`

    const a = document.createElement('a')
    a.href = recording.url
    a.download = filename
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
  }

  // 채팅 전송
  const handleSendChat = (e: React.FormEvent) => {
    e.preventDefault()
    if (chatInput.trim()) {
      sendChatMessage(chatInput)
      setChatInput('')
    }
  }

  // 현재 연주 중인 사람들 (peerInstruments 기반, 자신 제외)
  const remotePerformers = Object.entries(peerInstruments).filter(([peerId]) => peerId !== clientId)
  const performerCount = (isPerformer && myInstrument ? 1 : 0) + remotePerformers.length

  // 연주자가 있는지 여부에 따른 표시 상태
  // 연주자가 없으면 항상 '대기', 있으면 실제 RTC 상태 표시
  const hasAnyPerformer = performerCount > 0
  const displayStatus = hasAnyPerformer ? rtcStatus : 'idle'
  const roomFull = performerCount >= MAX_PERFORMERS
  const broadcasterName = broadcasterId
    ? (broadcasterId === clientId ? '나' : peerInstruments[broadcasterId]?.nickname ?? broadcasterId.slice(0, 4))
    : null
  // 관람자가 듣는 방송 믹스 스트림 (방송 담당자 하나)
  const broadcastStreams = Object.keys(remoteAudioMap)

  if (loading) {
    return <div className="loading-state">합주실 정보를 불러오는 중...</div>
  }

  if (!room) {
    return null
  }

  if (signalStatus === 'connecting') {
    return (
      <div className="loading-state">
        <div className="connecting-spinner" />
        서버에 연결 중...
      </div>
    )
  }

  if (signalStatus === 'error') {
    return (
      <div className="error-state">
        <p>시그널링 서버 연결에 실패했습니다.</p>
        <button onClick={() => window.location.reload()}>다시 시도</button>
      </div>
    )
  }

  // 비밀번호 방 입장 (방장은 서버가 묻지 않는다)
  if (joinError) {
    const locked = joinError.code === 'password-rate-limited'
    return (
      <div className="room-password-gate">
        <form className="room-password-card" onSubmit={handleSubmitPassword}>
          <h2>🔒 {room.title}</h2>
          <p className={`room-password-message ${joinError.code === 'password-required' ? '' : 'error'}`}>
            {describeJoinError(joinError)}
          </p>
          <input
            type="password"
            autoFocus
            autoComplete="off"
            placeholder="합주실 비밀번호"
            value={passwordInput}
            onChange={(e) => setPasswordInput(e.target.value)}
            maxLength={64}
            disabled={locked}
          />
          <div className="room-password-actions">
            <button type="button" className="btn-secondary" onClick={handleLeave}>목록으로</button>
            <button type="submit" className="btn-primary" disabled={locked || !passwordInput}>입장</button>
          </div>
        </form>
      </div>
    )
  }

  return (
    <div className="live-room" onClick={resumeAllAudioContexts}>
      {/* 방장: 새 연주 참여 요청 알림 */}
      {isHost && requestNotice && !showPendingRequests && (
        <div className="request-notice-toast" role="status">
          <span>🎫 <strong>{requestNotice.nickname}</strong>님이 {INSTRUMENT_INFO[requestNotice.instrument]?.name || requestNotice.instrument} 연주를 요청했습니다</span>
          <button onClick={(e) => { e.stopPropagation(); dismissRequestNotice(); setShowPendingRequests(true) }}>확인</button>
          <button className="close-btn" aria-label="닫기" onClick={(e) => { e.stopPropagation(); dismissRequestNotice() }}>×</button>
        </div>
      )}
      {copyNotice && <div className="copy-notice-toast" role="status">{copyNotice}</div>}
      {/* 방 정보 모달 */}
      {showRoomInfo && (
        <div className="room-info-modal">
          <div className="modal-backdrop" onClick={() => setShowRoomInfo(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>방 정보</h2>
              <button onClick={() => setShowRoomInfo(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              {/* 방 제목 */}
              <div className="info-title-section">
                <h3>{room.title}</h3>
                {room.description && <p className="room-description">{room.description}</p>}
              </div>

              {/* 기본 정보 */}
              <div className="info-section">
                <div className="info-row"><span>👑 방장</span><strong>{hostNickname || '...'}</strong></div>
                <div className="info-row"><span>🎵 장르</span><strong>{room.genre || '기타'}</strong></div>
                <div className="info-row"><span>🚪 참여 방식</span><strong>{room.free_join ? '자유 참여' : '승인 필요'}</strong></div>
                <div className="info-row"><span>👥 현재 인원</span><strong>{peers.length + 1} / {room.max_participants}명</strong></div>
                <div className="info-row"><span>👁️ 공개 여부</span><strong>{room.is_private ? '비공개 (초대 링크로만 입장)' : '공개'}</strong></div>
                {room.has_password && <div className="info-row"><span>🔒 비밀번호</span><strong>설정됨</strong></div>}
              </div>

              {/* 초대 */}
              <div className="info-section">
                <h4>초대</h4>
                <div className="invite-actions">
                  <button className="sync-btn" onClick={() => void copyText(inviteLink(room.id, PUBLIC_WEB_URL, window.location.origin), '초대 링크')}>
                    🔗 초대 링크 복사
                  </button>
                  <button className="sync-btn" onClick={() => void copyText(room.id, '방 코드')}>
                    #️⃣ 방 코드 복사
                  </button>
                </div>
                {room.has_password && <small className="form-hint">비밀번호는 링크에 포함되지 않습니다. 따로 알려주세요.</small>}
              </div>

              {/* 태그 */}
              {room.tags && room.tags.length > 0 && (
                <div className="info-section">
                  <h4>태그</h4>
                  <div className="info-tags-list">
                    {room.tags.map((tag) => <span key={tag} className="info-tag">#{tag}</span>)}
                  </div>
                </div>
              )}

              {/* 악기 구성 */}
              {room.instrument_slots && room.instrument_slots.length > 0 && (
                <div className="info-section">
                  <h4>악기 구성</h4>
                  <div className="info-instrument-list">
                    {room.instrument_slots.map(slot => {
                      const info = INSTRUMENT_INFO[slot.instrument] || { icon: '🎵', name: slot.instrument }
                      const used = getInstrumentUsage(slot.instrument)
                      const available = slot.count - used
                      return (
                        <div key={slot.instrument} className="info-instrument-item">
                          <span className="info-inst-icon">{info.icon}</span>
                          <span className="info-inst-name">{info.name}</span>
                          <span className={`info-inst-slots ${available === 0 ? 'full' : ''}`}>
                            {used}/{slot.count}
                          </span>
                        </div>
                      )
                    })}
                  </div>
                </div>
              )}

              {/* 방장 전용 버튼들 */}
              {isHost && (
                <div className="host-actions">
                  <button onClick={handleSyncParticipants} className="sync-btn">
                    🔄 참여자 수 동기화
                  </button>
                  <button onClick={handleDeleteRoom} className="delete-room-btn">
                    🗑️ 합주실 삭제
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* 승인 요청 관리 모달 (방장용) */}
      {showPendingRequests && (
        <div className="pending-requests-modal">
          <div className="modal-backdrop" onClick={() => setShowPendingRequests(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>🎫 연주 참여 요청</h2>
              <button onClick={() => setShowPendingRequests(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              {pendingRequests.length === 0 ? (
                <p className="no-requests">대기 중인 요청이 없습니다</p>
              ) : (
                <div className="requests-list">
                  {pendingRequests.map(request => {
                    const instInfo = INSTRUMENT_INFO[request.instrument] || { icon: '🎵', name: request.instrument }
                    return (
                      <div key={request.oderId} className="request-item">
                        <div className="request-info">
                          <span className="request-icon">{instInfo.icon}</span>
                          <div className="request-details">
                            <span className="request-nickname">{request.nickname}</span>
                            <span className="request-instrument">{instInfo.name} 연주 희망</span>
                          </div>
                        </div>
                        {rejectingId === request.oderId ? (
                          <div className="reject-reason-picker">
                            {REJECT_REASON_PRESETS.map(preset => (
                              <button key={preset} className="reject-reason-preset" onClick={() => confirmReject(request.oderId, preset)}>
                                {preset}
                              </button>
                            ))}
                            <form
                              className="reject-reason-custom"
                              onSubmit={(e) => { e.preventDefault(); confirmReject(request.oderId, rejectReason) }}
                            >
                              <input
                                type="text"
                                placeholder="직접 입력 (선택)"
                                value={rejectReason}
                                onChange={(e) => setRejectReason(e.target.value)}
                                maxLength={200}
                              />
                              <button type="submit" className="reject-btn">거절</button>
                              <button type="button" className="cancel-request-btn" onClick={() => { setRejectingId(null); setRejectReason('') }}>취소</button>
                            </form>
                          </div>
                        ) : (
                          <div className="request-actions">
                            <button
                              onClick={() => approveRequest(request.oderId)}
                              className="approve-btn"
                            >
                              ✓ 승인
                            </button>
                            <button
                              onClick={() => { setRejectingId(request.oderId); setRejectReason('') }}
                              className="reject-btn"
                            >
                              ✗ 거절
                            </button>
                          </div>
                        )}
                      </div>
                    )
                  })}
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* 방 설정 모달 (방장용) */}
      {showRoomSettings && (
        <div className="room-edit-modal">
          <div className="modal-backdrop" onClick={() => setShowRoomSettings(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>합주실 설정</h2>
              <button onClick={() => setShowRoomSettings(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              <div className="edit-form-group">
                <label htmlFor="edit-title">방 제목 *</label>
                <input
                  id="edit-title"
                  type="text"
                  value={editTitle}
                  onChange={(e) => setEditTitle(e.target.value)}
                  placeholder="예: 주말 재즈 세션"
                  maxLength={100}
                />
              </div>

              <div className="edit-form-group">
                <label htmlFor="edit-description">설명</label>
                <textarea
                  id="edit-description"
                  value={editDescription}
                  onChange={(e) => setEditDescription(e.target.value)}
                  rows={3}
                  maxLength={500}
                  placeholder="어떤 합주를 하고 싶으신가요? (선택사항)"
                />
              </div>

              <div className="edit-form-row">
                <div className="edit-form-group">
                  <label htmlFor="edit-genre">장르</label>
                  <select
                    id="edit-genre"
                    value={editGenre}
                    onChange={(e) => setEditGenre(e.target.value)}
                  >
                    {GENRES.map(g => (
                      <option key={g} value={g}>{g}</option>
                    ))}
                  </select>
                </div>

                <div className="edit-form-group">
                  <label htmlFor="edit-max">최대 인원</label>
                  <select
                    id="edit-max"
                    value={editMaxParticipants}
                    onChange={(e) => setEditMaxParticipants(Number(e.target.value))}
                  >
                    {[2, 4, 6, 8, 10, 12, 16, 20].map(n => (
                      <option key={n} value={n}>{n}명</option>
                    ))}
                  </select>
                </div>
              </div>

              {/* 태그 */}
              <div className="edit-form-group">
                <label>태그</label>
                <div className="edit-tags-container">
                  {COMMON_TAGS.map(tag => (
                    <button
                      key={tag}
                      type="button"
                      className={`edit-tag-btn ${editTags.includes(tag) ? 'active' : ''}`}
                      onClick={() => handleEditTagToggle(tag)}
                    >
                      {tag}
                    </button>
                  ))}
                </div>
                <div className="edit-custom-tag-input">
                  <input
                    type="text"
                    placeholder="커스텀 태그 추가"
                    value={editCustomTag}
                    onChange={(e) => setEditCustomTag(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleAddEditCustomTag())}
                    maxLength={20}
                  />
                  <button type="button" onClick={handleAddEditCustomTag}>추가</button>
                </div>
                {editTags.length > 0 && (
                  <div className="edit-selected-tags">
                    {editTags.map(tag => (
                      <span key={tag} className="edit-selected-tag">
                        {tag}
                        <button type="button" onClick={() => handleEditTagToggle(tag)}>×</button>
                      </span>
                    ))}
                  </div>
                )}
              </div>

              {/* 악기 구성 */}
              <div className="edit-form-group">
                <label>악기 구성</label>
                <p className="edit-form-hint">각 악기별 참여 가능 인원을 설정하세요 (총 {editTotalSlots}자리)</p>

                <div className="edit-instrument-slots">
                  {editInstrumentSlots.map(slot => {
                    const instInfo = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
                    return (
                      <div key={slot.instrument} className="edit-instrument-slot">
                        <span className="edit-slot-icon">{instInfo?.icon}</span>
                        <span className="edit-slot-name">{instInfo?.name || slot.instrument}</span>
                        <div className="edit-slot-count-control">
                          <button
                            type="button"
                            onClick={() => handleSlotCountChange(slot.instrument, slot.count - 1)}
                            disabled={slot.count <= 0}
                          >
                            -
                          </button>
                          <span className="edit-slot-count">{slot.count}</span>
                          <button
                            type="button"
                            onClick={() => handleSlotCountChange(slot.instrument, slot.count + 1)}
                            disabled={slot.count >= 10}
                          >
                            +
                          </button>
                        </div>
                        <button
                          type="button"
                          className="edit-slot-remove"
                          onClick={() => handleRemoveInstrument(slot.instrument)}
                          title="악기 제거"
                        >
                          ×
                        </button>
                      </div>
                    )
                  })}
                </div>

                {availableToAdd.length > 0 && (
                  <div className="edit-add-instrument">
                    <select
                      onChange={(e) => {
                        if (e.target.value) {
                          handleAddInstrument(e.target.value)
                          e.target.value = ''
                        }
                      }}
                      defaultValue=""
                    >
                      <option value="" disabled>+ 악기 추가</option>
                      {availableToAdd.map(inst => (
                        <option key={inst.id} value={inst.id}>
                          {inst.icon} {inst.name}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
              </div>

              {/* 참여 방식 */}
              <div className="edit-form-group">
                <label>참여 방식</label>
                <div className="edit-toggle-option">
                  <label className="toggle-switch">
                    <input
                      type="checkbox"
                      checked={editFreeJoin}
                      onChange={(e) => setEditFreeJoin(e.target.checked)}
                    />
                    <span className="toggle-slider"></span>
                  </label>
                  <div className="toggle-label">
                    <strong>{editFreeJoin ? '자유 참여' : '승인 필요'}</strong>
                    <span>{editFreeJoin ? '누구나 바로 연주자로 참여할 수 있습니다' : '방장이 승인해야 연주자로 참여할 수 있습니다'}</span>
                  </div>
                </div>
              </div>

              {/* 공개 여부 (마이그레이션 008 이후) */}
              {room.is_private !== undefined && (
                <div className="edit-form-group">
                  <label>공개 여부</label>
                  <div className="edit-toggle-option">
                    <label className="toggle-switch">
                      <input
                        type="checkbox"
                        checked={!editIsPrivate}
                        onChange={(e) => setEditIsPrivate(!e.target.checked)}
                      />
                      <span className="toggle-slider"></span>
                    </label>
                    <div className="toggle-label">
                      <strong>{editIsPrivate ? '비공개' : '공개'}</strong>
                      <span>{editIsPrivate ? '목록에 보이지 않습니다. 초대 링크/방 코드로만 들어올 수 있습니다' : '합주실 목록에 표시됩니다'}</span>
                    </div>
                  </div>
                </div>
              )}

              {/* 비밀번호 (마이그레이션 008 이후) */}
              {room.has_password !== undefined && (
              <div className="edit-form-group">
                <label>비밀번호 {room.has_password ? '(설정됨)' : '(없음)'}</label>
                <div className="password-mode-options">
                  <label><input type="radio" checked={editPasswordMode === 'keep'} onChange={() => setEditPasswordMode('keep')} /> 그대로</label>
                  <label><input type="radio" checked={editPasswordMode === 'set'} onChange={() => setEditPasswordMode('set')} /> {room.has_password ? '변경' : '설정'}</label>
                  {room.has_password && (
                    <label><input type="radio" checked={editPasswordMode === 'clear'} onChange={() => setEditPasswordMode('clear')} /> 해제</label>
                  )}
                </div>
                {editPasswordMode === 'set' && (
                  <input
                    type="password"
                    autoComplete="new-password"
                    placeholder="6~64자"
                    value={editPassword}
                    onChange={(e) => setEditPassword(e.target.value)}
                    maxLength={64}
                  />
                )}
                <small className="form-hint">이미 들어와 있는 참여자는 그대로 남습니다. 새로 들어오는 사람부터 적용됩니다.</small>
              </div>
              )}

              <div className="edit-form-actions">
                <button
                  onClick={() => setShowRoomSettings(false)}
                  className="edit-cancel-btn"
                  disabled={settingsSaving}
                >
                  취소
                </button>
                <button
                  onClick={handleSaveRoomSettings}
                  className="edit-save-btn"
                  disabled={settingsSaving}
                >
                  {settingsSaving ? '저장 중...' : '저장'}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 악기 선택 모달 */}
      {showInstrumentSelect && (
        <div className="instrument-select-modal">
          <div className="modal-backdrop" onClick={() => setShowInstrumentSelect(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>악기 선택</h2>
              <button onClick={() => setShowInstrumentSelect(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              <p className="modal-description">연주할 악기를 선택하세요</p>
              <div className="instrument-grid">
                {room?.instrument_slots?.map(slot => {
                  const info = INSTRUMENT_INFO[slot.instrument] || { icon: '🎵', name: slot.instrument }
                  const used = getInstrumentUsage(slot.instrument)
                  const available = slot.count - used
                  const isAvailable = available > 0

                  return (
                    <button
                      key={slot.instrument}
                      className={`instrument-option ${!isAvailable ? 'disabled' : ''}`}
                      onClick={() => isAvailable && handleSelectInstrument(slot.instrument)}
                      disabled={!isAvailable}
                    >
                      <span className="inst-icon">{info.icon}</span>
                      <span className="inst-name">{info.name}</span>
                      <span className={`inst-slots ${available === 0 ? 'full' : ''}`}>
                        {available}/{slot.count}
                      </span>
                    </button>
                  )
                })}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 악기 변경 요청 모달 */}
      {showInstrumentChangeModal && (
        <div className="instrument-select-modal">
          <div className="modal-backdrop" onClick={() => setShowInstrumentChangeModal(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>🔄 악기 변경 요청</h2>
              <button onClick={() => setShowInstrumentChangeModal(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              <p className="modal-description">
                현재 악기: <strong>{INSTRUMENT_INFO[myInstrument || '']?.name || myInstrument}</strong>
              </p>
              <p className="modal-description">변경할 악기를 선택하세요 (방장의 승인이 필요합니다)</p>
              <div className="instrument-grid">
                {room?.instrument_slots?.map(slot => {
                  const info = INSTRUMENT_INFO[slot.instrument] || { icon: '🎵', name: slot.instrument }
                  const used = getInstrumentUsage(slot.instrument)
                  // 현재 내 악기는 내가 사용 중이므로 1 빼줌
                  const myCurrentUsage = slot.instrument === myInstrument ? 1 : 0
                  const available = slot.count - used + myCurrentUsage
                  const isAvailable = available > 0 && slot.instrument !== myInstrument

                  return (
                    <button
                      key={slot.instrument}
                      className={`instrument-option ${!isAvailable ? 'disabled' : ''} ${slot.instrument === myInstrument ? 'current' : ''}`}
                      onClick={() => {
                        if (isAvailable) {
                          requestInstrumentChange(slot.instrument)
                          setShowInstrumentChangeModal(false)
                        }
                      }}
                      disabled={!isAvailable}
                    >
                      <span className="inst-icon">{info.icon}</span>
                      <span className="inst-name">{info.name}</span>
                      {slot.instrument === myInstrument ? (
                        <span className="inst-slots current">현재</span>
                      ) : (
                        <span className={`inst-slots ${available === 0 ? 'full' : ''}`}>
                          {available}/{slot.count}
                        </span>
                      )}
                    </button>
                  )
                })}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 녹음 목록 모달 */}
      {showRecordings && (
        <div className="recordings-modal">
          <div className="modal-backdrop" onClick={() => setShowRecordings(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>🎙️ 녹음 목록</h2>
              <button onClick={() => setShowRecordings(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              {recordings.length === 0 ? (
                <div className="no-recordings">
                  <p>녹음된 파일이 없습니다</p>
                  <small>녹음 버튼을 눌러 합주를 녹음해보세요</small>
                </div>
              ) : (
                <div className="recordings-list">
                  {recordings.map(recording => {
                    const date = new Date(recording.timestamp)
                    const timeStr = `${date.getHours().toString().padStart(2, '0')}:${date.getMinutes().toString().padStart(2, '0')}`
                    // 파일 형식 표시
                    const format = recording.mimeType.includes('webm') ? 'WebM'
                      : recording.mimeType.includes('mp4') ? 'M4A'
                      : recording.mimeType.includes('ogg') ? 'OGG'
                      : 'Audio'
                    return (
                      <div key={recording.id} className="recording-item">
                        <div className="recording-info">
                          <span className="recording-time">🕐 {timeStr}</span>
                          <span className="recording-duration">{formatDuration(recording.duration)}</span>
                          <span className="recording-format">{format}</span>
                        </div>
                        <div className="recording-controls">
                          <audio controls src={recording.url} className="recording-audio" />
                        </div>
                        <div className="recording-actions">
                          <button
                            onClick={() => downloadRecording(recording)}
                            className="download-btn"
                            title="다운로드"
                          >
                            ⬇️
                          </button>
                          <button
                            onClick={() => deleteRecording(recording.id)}
                            className="delete-btn"
                            title="삭제"
                          >
                            🗑️
                          </button>
                        </div>
                      </div>
                    )
                  })}
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* 상단 헤더 */}
      <header className="live-header">
        <div className="live-header-left">
          <Link to="/rooms" className="back-link">←</Link>
          <div className="live-title-area">
            <h1>{room.title}</h1>
            <div className="live-meta">
              <span className={`live-badge ${displayStatus}`}>
                {displayStatus === 'live' ? '● LIVE' : RTC_STATUS_TEXT[displayStatus]}
              </span>
              <span className="genre-badge">{room.genre || '기타'}</span>
              <span className="viewer-count">👁 {peers.length + 1}명</span>
              <span className="host-info">👑 {hostNickname || '...'}</span>
            </div>
          </div>
        </div>
        <div className="live-header-right">
          <button onClick={() => setShowRoomInfo(true)} className="header-btn">
            ℹ️ 정보
          </button>
          {isHost && (
            <button onClick={openRoomSettings} className="header-btn">
              ⚙️ 방 설정
            </button>
          )}
          <button onClick={handleLeave} className="header-btn leave">
            나가기
          </button>
        </div>
      </header>

      <div className="live-body">
        {/* 왼쪽: 연주자 패널 */}
        <aside className="performers-panel">
          <div className="panel-header">
            <h2>🎸 연주자</h2>
            <span className="performer-count">{performerCount}명</span>
          </div>

          <div className="performers-list" ref={performersListRef}>
            {/* 내 채널 (연주자일 경우) */}
            {isPerformer && myInstrument && (
              <div className="performer-item me active">
                <div className="performer-avatar">
                  {isHost && <span className="host-crown">👑</span>}
                  <div className="avatar-circle">
                    <span>{INSTRUMENT_INFO[myInstrument]?.icon || '🎵'}</span>
                  </div>
                  <span className="live-indicator" />
                </div>
                <div className="performer-info">
                  <span className="performer-name">
                    {nickname} {isHost && '(방장)'}
                    {isBroadcaster && <span className="broadcast-badge" title={`관람자 ${broadcastViewerCount}명에게 믹스 송출 중`}>📡 방송</span>}
                  </span>
                  <span className="performer-instrument">{INSTRUMENT_INFO[myInstrument]?.name || myInstrument}</span>
                  {/* 내 로컬 처리 지연 / xrun */}
                  <span className="engine-local-stats" title="로컬 처리 지연 = 입력 버퍼 + 프레임 + 출력 버퍼 (네트워크 제외)">
                    처리 지연 {formatMs(engineStats?.estLatencyMs, 1)}
                    {' · '}
                    <span className={(engineStats?.xruns ?? 0) > 0 ? 'xrun-warn' : ''}>xrun {engineStats?.xruns ?? 0}</span>
                  </span>
                  <div className="level-meter mini">
                    <div className="level-bar">
                      <div className="level-fill" style={{ width: `${Math.min(100, (engineStats?.inputLevel ?? 0) * 100)}%` }} />
                    </div>
                  </div>
                  {/* 방장이 아닌 연주자: 악기 변경 요청 UI */}
                  {!isHost && myInstrumentChangeStatus === 'none' && (
                    <button
                      onClick={() => setShowInstrumentChangeModal(true)}
                      className="change-instrument-btn"
                      title="악기 변경 요청"
                    >
                      🔄 악기 변경
                    </button>
                  )}
                  {!isHost && myInstrumentChangeStatus === 'pending' && (
                    <div className="my-instrument-change-status pending">
                      <span>⏳ 변경 요청 중: {INSTRUMENT_INFO[myNewInstrument || '']?.name || myNewInstrument}</span>
                      <button onClick={cancelInstrumentChangeRequest} className="cancel-btn small">취소</button>
                    </div>
                  )}
                  {!isHost && myInstrumentChangeStatus === 'approved' && (
                    <div className="my-instrument-change-status approved">
                      <span>✅ 변경 승인됨!</span>
                    </div>
                  )}
                  {!isHost && myInstrumentChangeStatus === 'rejected' && (
                    <div className="my-instrument-change-status rejected">
                      <span>❌ 변경 거절됨</span>
                    </div>
                  )}
                </div>
                {/* 내 입력 뮤트 (engine_set_input_muted) */}
                <button
                  onClick={toggleInputMuted}
                  className={`mic-toggle ${inputMuted ? 'off' : 'on'}`}
                  title={inputMuted ? '마이크 켜기' : '마이크 끄기 (상대·관람자에게 무음)'}
                >
                  {inputMuted ? '🔇' : '🎤'}
                </button>
                {/* 모니터링 (내 소리 듣기) */}
                <div className="monitoring-controls">
                  <button
                    onClick={() => setMonitor(!isMonitoringEnabled, monitorGain)}
                    className={`monitor-toggle ${isMonitoringEnabled ? 'on' : 'off'}`}
                    title={isMonitoringEnabled ? '모니터링 끄기 (내 소리 안 들림)' : '모니터링 켜기 (내 소리 들림)'}
                  >
                    {isMonitoringEnabled ? '🎧' : '🔕'}
                  </button>
                  {isMonitoringEnabled && (
                    <input
                      type="range"
                      min="0"
                      max="1"
                      step="0.01"
                      value={monitorGain}
                      onChange={(e) => setMonitor(true, Number(e.target.value))}
                      className="monitoring-volume-slider"
                      title={`모니터링 볼륨: ${Math.round(monitorGain * 100)}%`}
                    />
                  )}
                </div>
              </div>
            )}

            {/* 다른 연주자들 (peerInstruments 기반) */}
            {remotePerformers.map(([oderId, peerInfo]) => {
              const instInfo = INSTRUMENT_INFO[peerInfo.instrument] || { icon: '🎵', name: peerInfo.instrument }
              // 내가 연주 중이면 엔진 피어 상태, 아니면(관람) 방송 담당 여부만 표시
              const ps = isPerformer ? peerEngineStats[oderId] : undefined
              const state: PeerState | null = isPerformer ? (ps?.state ?? 'connecting') : null
              const stateInfo = state ? PEER_STATE_INFO[state] : null
              const isActive = isPerformer ? state === 'connected' : true
              const muted = mixSettingsMap[oderId]?.muted ?? false
              // 'failed'(릴레이까지 실패) 안내 또는 릴레이 경로 지연 안내
              const pathHint = state ? peerPathHint(state, ps?.path, engineStats?.relay) : null

              return (
                <div
                  key={oderId}
                  className="performer-item-wrapper"
                  ref={(el) => { performerRefs.current[oderId] = el }}
                >
                  <div className={`performer-item ${isActive ? 'active' : 'connecting'}`}>
                    <div className="performer-avatar">
                      {peerInfo.isHost && <span className="host-crown">👑</span>}
                      <div className="avatar-circle">
                        <span>{instInfo.icon}</span>
                      </div>
                      {isActive && <span className="live-indicator" />}
                    </div>
                    <div className="performer-info">
                      <span className="performer-name">
                        {peerInfo.nickname || `연주자 ${oderId.slice(0, 4)}`} {peerInfo.isHost && '(방장)'}
                        {broadcasterId === oderId && <span className="broadcast-badge" title="관람자에게 믹스를 송출하는 연주자">📡 방송</span>}
                      </span>
                      <span className="performer-instrument">{instInfo.name}</span>
                      {stateInfo && (
                        <span className={`peer-state-badge ${stateInfo.className}`} title={ps ? peerStatsTitle(ps) : undefined}>
                          {stateInfo.label}
                          {ps?.path && <> · {pathLabel(ps.path)}</>}
                        </span>
                      )}
                      {pathHint && <span className={state === 'failed' ? 'peer-failed-hint' : 'peer-relay-hint'}>{pathHint}</span>}
                    </div>
                    {/* 네트워크 상태 (연주 중일 때만: 엔진 통계) */}
                    {ps && state !== 'failed' && (
                      <div className="performer-latency" title={peerStatsTitle(ps)}>
                        <span className="latency-value">{formatMs(ps.rttMs)}</span>
                        <span className="quality-indicator">{ps.lossPct >= 5 ? '🔴' : ps.lossPct >= 1 ? '🟡' : '🟢'}</span>
                      </div>
                    )}
                    {/* 피어별 음소거 (엔진) */}
                    {isPerformer && (
                      <button
                        className={`performer-mute-btn ${muted ? 'muted' : ''}`}
                        onClick={() => setMixMuted(oderId, !muted)}
                        title={muted ? '음소거 해제' : '음소거'}
                      >
                        {muted ? '🔇' : '🔊'}
                      </button>
                    )}
                  </div>
                </div>
              )
            })}

            {performerCount === 0 && (
              <div className="no-performers">
                <p>아직 연주자가 없습니다</p>
                <small>{canPerform ? '첫 번째 연주자가 되어보세요!' : '연주가 시작되면 여기에 표시됩니다'}</small>
              </div>
            )}
          </div>

          {/* 방장: 승인 요청 알림 (참여 버튼 위에 표시) */}
          {isHost && pendingRequests.length > 0 && (
            <div className="pending-requests-alert" onClick={() => setShowPendingRequests(true)}>
              <span className="alert-badge">{pendingRequests.length}</span>
              <span>연주 참여 요청</span>
            </div>
          )}

          {/* 참여/관람 전환 버튼 */}
          <div className="participation-toggle">
            {isPerformer ? (
              <button onClick={handleBecomeViewer} className="toggle-btn viewer">
                👀 관람자로 전환
              </button>
            ) : !canPerform ? (
              /* 웹: 연주는 데스크톱 앱 전용 */
              <div className="web-notice">
                <p>🖥️ 연주는 데스크톱 앱에서</p>
                <small>웹에서는 관람만 할 수 있습니다. 연주에 참여하려면 Band 데스크톱 앱을 설치해주세요.</small>
                {DESKTOP_DOWNLOAD_URL ? (
                  <a href={DESKTOP_DOWNLOAD_URL} className="download-link" target="_blank" rel="noopener noreferrer">📥 데스크톱 앱 다운로드</a>
                ) : (
                  <span className="download-link disabled">📥 다운로드 준비 중</span>
                )}
              </div>
            ) : performBusy || performStarting ? (
              <div className="request-status pending">
                <p>⏳ 오디오 엔진 시작 중...</p>
              </div>
            ) : myRequestStatus === 'pending' ? (
              /* 승인 대기 중 */
              <div className="request-status pending">
                <p>⏳ 방장 승인 대기 중...</p>
                <small>{INSTRUMENT_INFO[myRequestInstrument || '']?.name || myRequestInstrument} 연주 요청</small>
                <small className="request-hint">
                  {hostOnline === false
                    ? '방장이 지금 접속해 있지 않습니다. 방장이 들어오면 요청이 전달됩니다.'
                    : '승인되면 오디오 엔진이 자동으로 시작됩니다.'}
                </small>
                <button onClick={cancelRequest} className="cancel-request-btn">
                  요청 취소
                </button>
              </div>
            ) : myRequestStatus === 'approved' ? (
              /* 승인됨 - 연주 시작 가능 */
              <div className="request-status approved">
                <p>✅ 승인되었습니다!</p>
                {rtcError && <small>엔진을 자동으로 시작하지 못했습니다. 설정을 확인한 뒤 다시 시도하세요.</small>}
                <button onClick={handleStartAfterApproval} className="toggle-btn performer">
                  🎤 연주 시작하기
                </button>
              </div>
            ) : myRequestStatus === 'rejected' ? (
              /* 거절됨 */
              <div className="request-status rejected">
                <p>❌ 요청이 거절되었습니다</p>
                {myRejectReason && <small className="reject-reason">사유: {myRejectReason}</small>}
                <button onClick={dismissRejection} className="toggle-btn performer">
                  🎤 다시 요청하기
                </button>
              </div>
            ) : (
              /* 기본 상태 (데스크톱 앱) */
              <button
                onClick={handleBecomePerformer}
                className="toggle-btn performer"
                disabled={roomFull}
                title={roomFull ? `연주자는 최대 ${MAX_PERFORMERS}명입니다` : undefined}
              >
                🎤 {roomFull ? `연주자 가득 참 (${MAX_PERFORMERS}명)` : '연주 참여하기'}
              </button>
            )}
            {canPerform && !isPerformer && rtcError && (
              <p className="perform-error">{rtcError}</p>
            )}
            {canPerform && !room.free_join && !isHost && !isPerformer && myRequestStatus === 'none' && (
              <p className="approval-notice">이 방은 방장 승인이 필요합니다</p>
            )}
          </div>
        </aside>

        {/* 가운데: 믹서 */}
        <main className="mixer-panel">
          <div className="panel-header">
            <h2>🎚️ 개인 믹서</h2>
            <span className="mixer-hint">내가 듣는 소리 조절</span>
          </div>

          <div className="mixer-content">
            {/* 마스터 컨트롤 */}
            <div className={`mixer-master ${masterMuted ? 'muted' : ''}`}>
              <div className="master-header">
                <label>마스터</label>
                <button
                  className={`master-mute-btn ${masterMuted ? 'active' : ''}`}
                  onClick={toggleMasterMute}
                  title={masterMuted ? '음소거 해제' : '음소거'}
                >
                  {masterMuted ? '🔇' : '🔊'}
                </button>
              </div>
              <div className="master-control-row">
                <label>볼륨</label>
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.01"
                  value={masterVolume}
                  onChange={(e) => setMasterVolume(parseFloat(e.target.value))}
                />
                <span>{masterMuted ? 'MUTE' : `${Math.round(masterVolume * 100)}%`}</span>
              </div>
              {/* 마스터 패닝은 웹 재생(관람)에서만. 엔진은 피어별 pan만 있다 */}
              {!isPerformer && (
                <div className="master-control-row">
                  <label>패닝</label>
                  <input
                    type="range"
                    min="-1"
                    max="1"
                    step="0.01"
                    value={masterPan}
                    onChange={(e) => setMasterPan(parseFloat(e.target.value))}
                  />
                  <span>{masterPan < 0 ? `L${Math.round(Math.abs(masterPan) * 100)}` : masterPan > 0 ? `R${Math.round(masterPan * 100)}` : 'C'}</span>
                </div>
              )}
              <div className="level-meter master-level">
                <div className="level-bar">
                  <div
                    className={`level-fill ${masterLevel > 80 ? 'high' : masterLevel > 50 ? 'mid' : ''}`}
                    style={{ width: `${masterLevel}%` }}
                  />
                </div>
              </div>
            </div>

            <div className="mixer-channels">
              {isPerformer ? (
                <>
                  {/* 내 채널 */}
                  {myInstrument && (
                    <div className="mixer-channel me">
                      <div className="channel-header">
                        <span className="channel-icon">{INSTRUMENT_INFO[myInstrument]?.icon || '🎵'}</span>
                        <span className="channel-name">나 (모니터)</span>
                      </div>
                      <div className="channel-info">
                        <span className="audio-info">
                          {engineStats ? `${engineStats.sampleRate / 1000}kHz · 버퍼 ${engineStats.deviceBuffer}` : '엔진 상태 확인 중'}
                        </span>
                        <span className="audio-info">처리 지연 {formatMs(engineStats?.estLatencyMs, 1)} · xrun {engineStats?.xruns ?? 0}</span>
                      </div>
                      <div className="channel-level">
                        <div className="level-meter">
                          <div className="level-bar">
                            <div className="level-fill" style={{ width: `${audioLevels[clientId ?? ''] ?? 0}%` }} />
                          </div>
                        </div>
                      </div>
                      {rtcError && <div className="channel-error">{rtcError}</div>}
                    </div>
                  )}

                  {/* 다른 연주자 채널 스트립 (엔진 피어별 gain/mute) */}
                  {remotePerformers.map(([oderId, peerInfo]) => {
                    const mix = mixSettingsMap[oderId] || { volume: 1, pan: 0, muted: false }
                    const instInfo = INSTRUMENT_INFO[peerInfo.instrument] || { icon: '🎵', name: peerInfo.instrument }
                    const ps = peerEngineStats[oderId]
                    const state: PeerState = ps?.state ?? 'connecting'
                    const level = audioLevels[oderId] || 0
                    return (
                      <div key={oderId} className={`mixer-channel ${mix.muted ? 'muted' : ''}`}>
                        <div className="channel-header">
                          <span className="channel-icon">{instInfo.icon}</span>
                          <span className="channel-name">{peerInfo.nickname || `연주자 ${oderId.slice(0, 4)}`}</span>
                          <button
                            className={`mute-btn ${mix.muted ? 'active' : ''}`}
                            onClick={() => setMixMuted(oderId, !mix.muted)}
                            title={mix.muted ? '음소거 해제' : '음소거'}
                          >
                            {mix.muted ? '🔇' : '🔊'}
                          </button>
                        </div>
                        <div className="channel-latency">
                          <span className={`peer-state-badge ${PEER_STATE_INFO[state].className}`}>{PEER_STATE_INFO[state].label}</span>
                          {state === 'failed' ? (
                            <span className="peer-failed-hint">{peerPathHint(state, ps?.path, engineStats?.relay)}</span>
                          ) : ps ? (
                            <span className="latency-text engine-peer-stats" title={peerStatsTitle(ps)}>
                              RTT {formatMs(ps.rttMs)} · 지터 {formatMs(ps.jitterMs, 1)} · 버퍼 {formatMs(ps.bufferMs)} · 손실 {ps.lossPct.toFixed(1)}%
                              {' · '}{pathLabel(ps.path) ?? '경로 탐색 중'}
                              {ps.path === 'relay' && <span className="peer-relay-hint"> ({RELAY_PATH_HINT})</span>}
                            </span>
                          ) : (
                            <span className="latency-text">연결 정보 교환 중</span>
                          )}
                        </div>
                        <div className="channel-level">
                          <div className="level-meter">
                            <div className="level-bar">
                              <div
                                className={`level-fill ${level > 80 ? 'high' : level > 50 ? 'mid' : ''}`}
                                style={{ width: `${level}%` }}
                              />
                            </div>
                          </div>
                        </div>
                        <div className="channel-controls">
                          <div className="control-row">
                            <label>볼륨</label>
                            <input
                              type="range"
                              min="0"
                              max="1"
                              step="0.01"
                              value={mix.volume}
                              onChange={(e) => setMixVolume(oderId, parseFloat(e.target.value))}
                              disabled={mix.muted}
                            />
                            <span>{Math.round(mix.volume * 100)}%</span>
                          </div>
                          <div className="control-row">
                            <label>패닝</label>
                            <input
                              type="range"
                              min="-1"
                              max="1"
                              step="0.01"
                              value={mix.pan}
                              onChange={(e) => setMixPan(oderId, parseFloat(e.target.value))}
                              disabled={mix.muted}
                            />
                            <span>{mix.pan < 0 ? `L${Math.round(Math.abs(mix.pan) * 100)}` : mix.pan > 0 ? `R${Math.round(mix.pan * 100)}` : 'C'}</span>
                          </div>
                        </div>
                      </div>
                    )
                  })}

                  {remotePerformers.length === 0 && (
                    <div className="mixer-empty">
                      <p>다른 연주자가 없습니다</p>
                      <small>연주자가 참여하면 여기서 볼륨을 조절할 수 있습니다</small>
                    </div>
                  )}
                </>
              ) : (
                <>
                  {/* 관람: 방송 담당 연주자의 합주 믹스 */}
                  {broadcastStreams.map((senderId) => {
                    const mix = mixSettingsMap[senderId] || { volume: 1, pan: 0, muted: false }
                    const netStats = peerNetworkStats[senderId]
                    const qualityInfo = QUALITY_ICONS[netStats?.quality || 'unknown']
                    const level = audioLevels[senderId] || 0
                    return (
                      <div key={senderId} className={`mixer-channel ${mix.muted ? 'muted' : ''}`}>
                        <div className="channel-header">
                          <span className="channel-icon">📡</span>
                          <span className="channel-name">합주 믹스 {broadcasterName && `(${broadcasterName})`}</span>
                          <button
                            className={`mute-btn ${mix.muted ? 'active' : ''}`}
                            onClick={() => setMixMuted(senderId, !mix.muted)}
                            title={mix.muted ? '음소거 해제' : '음소거'}
                          >
                            {mix.muted ? '🔇' : '🔊'}
                          </button>
                        </div>
                        <div className="channel-latency">
                          <span className="quality-dot" style={{ background: qualityInfo.color }}></span>
                          <span className="latency-text">
                            {netStats?.audioLatency != null ? `${netStats.audioLatency}ms` : '측정 중'}
                            {netStats?.jitter != null && <small> (지터: {netStats.jitter}ms)</small>}
                          </span>
                        </div>
                        <div className="channel-level">
                          <div className="level-meter">
                            <div className="level-bar">
                              <div
                                className={`level-fill ${level > 80 ? 'high' : level > 50 ? 'mid' : ''}`}
                                style={{ width: `${level}%` }}
                              />
                            </div>
                          </div>
                        </div>
                        <div className="channel-controls">
                          <div className="control-row">
                            <label>볼륨</label>
                            <input
                              type="range"
                              min="0"
                              max="1"
                              step="0.01"
                              value={mix.volume}
                              onChange={(e) => setMixVolume(senderId, parseFloat(e.target.value))}
                              disabled={mix.muted}
                            />
                            <span>{Math.round(mix.volume * 100)}%</span>
                          </div>
                          <div className="control-row">
                            <label>패닝</label>
                            <input
                              type="range"
                              min="-1"
                              max="1"
                              step="0.01"
                              value={mix.pan}
                              onChange={(e) => setMixPan(senderId, parseFloat(e.target.value))}
                              disabled={mix.muted}
                            />
                            <span>{mix.pan < 0 ? `L${Math.round(Math.abs(mix.pan) * 100)}` : mix.pan > 0 ? `R${Math.round(mix.pan * 100)}` : 'C'}</span>
                          </div>
                        </div>
                      </div>
                    )
                  })}

                  {broadcastStreams.length === 0 && (
                    <div className="mixer-empty">
                      {hasAnyPerformer ? (
                        <>
                          <p>합주 방송에 연결 중...</p>
                          <small>{rtcError ?? '방송 담당 연주자의 믹스를 기다리는 중입니다'}</small>
                        </>
                      ) : (
                        <>
                          <p>연주자가 없습니다</p>
                          <small>연주가 시작되면 여기서 볼륨을 조절할 수 있습니다</small>
                        </>
                      )}
                    </div>
                  )}
                </>
              )}
            </div>
          </div>

          {/* 녹음 컨트롤 */}
          <div className="recording-section">
            <div className="recording-control">
              <button
                onClick={isRecording ? stopRecording : startRecording}
                className={`recording-btn ${isRecording ? 'recording' : ''}`}
                title={isRecording ? '녹음 중지' : '녹음 시작'}
              >
                {isRecording ? '⏹️' : '⏺️'}
                <span>{isRecording ? '녹음 중지' : '녹음'}</span>
              </button>
              {isRecording && (
                <span className="recording-time">
                  🔴 {formatDuration(recordingDuration)}
                </span>
              )}
              {recordings.length > 0 && (
                <button
                  onClick={() => setShowRecordings(true)}
                  className="recordings-list-btn"
                >
                  📁 녹음 목록 ({recordings.length})
                </button>
              )}
            </div>
          </div>

          {/* 관람자 안내 */}
          {!isPerformer && (
            <div className="viewer-guide">
              <p>👀 관람 모드</p>
              <small>
                {canPerform
                  ? '연주에 참여하려면 왼쪽에서 "연주 참여하기"를 클릭하세요'
                  : '연주는 데스크톱 앱에서만 할 수 있습니다. 웹에서는 합주를 들을 수 있어요.'}
              </small>
            </div>
          )}
        </main>

        {/* 오른쪽: 채팅 */}
        <aside className="chat-panel">
          <div className="panel-header">
            <h2>💬 채팅</h2>
            <div className="nickname-area">
              <span className="nickname-display">{nickname}</span>
            </div>
          </div>

          <div className="chat-messages" ref={chatContainerRef}>
            {chatMessages.length === 0 ? (
              <div className="chat-empty">
                <p>채팅을 시작해보세요!</p>
              </div>
            ) : (
              chatMessages.map((msg) => (
                <div
                  key={msg.id}
                  className={`chat-message ${msg.peerId === clientId || msg.peerId === 'me' ? 'me' : ''}`}
                >
                  <span className="chat-nickname">{msg.nickname}</span>
                  <span className="chat-text">{msg.message}</span>
                </div>
              ))
            )}
          </div>

          <form className="chat-input-area" onSubmit={handleSendChat}>
            <input
              type="text"
              placeholder="메시지 입력..."
              value={chatInput}
              onChange={(e) => setChatInput(e.target.value)}
              className="chat-input"
            />
            <button type="submit" className="chat-send-btn">전송</button>
          </form>
        </aside>
      </div>

      {/* 악기 변경 요청 말풍선들 (fixed position으로 믹서 위에 표시) */}
      {isHost && pendingInstrumentChanges.map(request => {
        const pos = bubblePositions[request.oderId]
        if (!pos) return null

        return (
          <div
            key={request.oderId}
            className="instrument-change-bubble fixed"
            style={{
              top: pos.top,
              left: pos.left
            }}
          >
            <div className="bubble-arrow" />
            <div className="bubble-content">
              <span className="bubble-text">
                🔄 {INSTRUMENT_INFO[request.newInstrument]?.icon} {INSTRUMENT_INFO[request.newInstrument]?.name || request.newInstrument}
              </span>
              <div className="bubble-actions">
                <button
                  onClick={() => approveInstrumentChange(request.oderId)}
                  className="bubble-btn approve"
                  title="승인"
                >
                  ✓
                </button>
                <button
                  onClick={() => rejectInstrumentChange(request.oderId)}
                  className="bubble-btn reject"
                  title="거절"
                >
                  ✕
                </button>
              </div>
            </div>
          </div>
        )
      })}
    </div>
  )
}
