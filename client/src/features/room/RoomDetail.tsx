import { useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useAuth } from '../auth'
import { useRoom } from './state/RoomContext'
import { MAX_PERFORMERS } from '../../native/engineApi'
import { instrumentUsage } from './roomDetailModel'
import { useRoomDetailData } from './hooks/useRoomDetailData'
import { useRoomPresence } from './hooks/useRoomPresence'
import { useBubblePositions } from './hooks/useBubblePositions'
import { useRoomSettingsForm } from './hooks/useRoomSettingsForm'
import { useRoomHostActions } from './hooks/useRoomHostActions'
import { usePerformControls } from './hooks/usePerformControls'
import { useRejectPicker } from './hooks/useRejectPicker'
import { RoomInfoModal } from './components/RoomInfoModal'
import { PendingRequestsModal, RequestNoticeToast } from './components/PendingRequestsModal'
import { RoomSettingsModal } from './components/RoomSettingsModal'
import { InstrumentChangeModal, InstrumentSelectModal } from './components/InstrumentModals'
import { RecordingsModal } from './components/RecordingsModal'
import { InstrumentChangeBubbles, LiveHeader, RoomPasswordGate } from './components/RoomChrome'
import { PerformersPanel } from './components/PerformersPanel'
import { MixerPanel } from './components/MixerPanel'
import { ChatPanel } from './components/ChatPanel'
import './RoomDetail.css'

/**
 * 합주실 화면. 하위 컴포넌트는 components/, 화면 훅은 hooks/, 세션 상태는 state/에 있다.
 * 서버 상태(방 상세·방장 닉네임·설정 저장·삭제)는 features/rooms·profile의 query 훅,
 * 실시간 상태(시그널링·엔진·믹서·채팅)는 RoomContext가 갖는다.
 */
export function RoomDetail() {
  const { roomId } = useParams<{ roomId: string }>()
  const { user } = useAuth()
  const navigate = useNavigate()
  const {
    signalStatus,
    clientId,
    peers,
    joinError,
    submitRoomPassword,
    retryJoin,
    rtcStatus,
    leaveRoom,
    isRoomHost,
    isPerforming,
    myInstrument,
    peerInstruments,
    broadcasterId,
    resumeAllAudioContexts,
    pendingRequests,
    requestNotice,
    dismissRequestNotice,
    approveRequest,
    rejectRequest,
    pendingInstrumentChanges,
    approveInstrumentChange,
    rejectInstrumentChange,
    requestInstrumentChange,
    recordings,
    deleteRecording,
  } = useRoom()

  const { room, loading, hostNickname } = useRoomDetailData(roomId)
  useRoomPresence(roomId, room)

  // 연주자 여부는 엔진 상태에서 파생 (엔진 시작 실패 시 자동으로 관람자로 남는다)
  const isPerformer = isPerforming
  const [showPendingRequests, setShowPendingRequests] = useState(false)
  const [showRoomInfo, setShowRoomInfo] = useState(false)
  const [showRecordings, setShowRecordings] = useState(false)
  const [showInstrumentChangeModal, setShowInstrumentChangeModal] = useState(false)
  // 비밀번호 입장
  const [passwordInput, setPasswordInput] = useState('')
  // 초대 링크/방 코드 복사 결과
  const [copyNotice, setCopyNotice] = useState<string | null>(null)
  const [chatInput, setChatInput] = useState('')
  const rejectPicker = useRejectPicker(rejectRequest)
  // 악기 변경 말풍선 위치 계산
  const { performerRefs, performersListRef, bubblePositions } = useBubblePositions(pendingInstrumentChanges)

  // 방장 여부 (UI 표시용. 실제 권한은 서버가 auth 토큰과 DB로 판정한다)
  const isHost = isRoomHost || !!(user && room?.host_id === user.id)

  const settings = useRoomSettingsForm(room, roomId, user?.id, isHost)
  const { handleSyncParticipants, handleDeleteRoom } = useRoomHostActions({
    roomId,
    userId: user?.id,
    isHost,
    peerCount: peers.length,
    leaveRoom,
  })
  const perform = usePerformControls(roomId, room?.free_join, isHost)

  // 각 악기별 사용 중인 슬롯 수 계산
  const getInstrumentUsage = (instrumentId: string) => instrumentUsage(peerInstruments, instrumentId)

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

  // 퇴장
  const handleLeave = () => {
    leaveRoom()
    navigate('/rooms')
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

  // 비밀번호 방 입장 (방장은 서버가 묻지 않는다) / 정원 초과 안내
  if (joinError) {
    return (
      <RoomPasswordGate
        title={room.title}
        joinError={joinError}
        passwordInput={passwordInput}
        onPasswordInput={setPasswordInput}
        onSubmit={handleSubmitPassword}
        onRetry={retryJoin}
        onLeave={handleLeave}
      />
    )
  }

  return (
    <div className="live-room" onClick={resumeAllAudioContexts}>
      {/* 방장: 새 연주 참여 요청 알림 */}
      {isHost && requestNotice && !showPendingRequests && (
        <RequestNoticeToast
          notice={requestNotice}
          onConfirm={() => { dismissRequestNotice(); setShowPendingRequests(true) }}
          onDismiss={dismissRequestNotice}
        />
      )}
      {copyNotice && <div className="copy-notice-toast" role="status">{copyNotice}</div>}
      {/* 방 정보 모달 */}
      {showRoomInfo && (
        <RoomInfoModal
          room={room}
          hostNickname={hostNickname}
          participantCount={peers.length + 1}
          isHost={isHost}
          getInstrumentUsage={getInstrumentUsage}
          onCopy={copyText}
          onSyncParticipants={handleSyncParticipants}
          onDeleteRoom={handleDeleteRoom}
          onClose={() => setShowRoomInfo(false)}
        />
      )}

      {/* 승인 요청 관리 모달 (방장용) */}
      {showPendingRequests && (
        <PendingRequestsModal
          requests={pendingRequests}
          picker={rejectPicker}
          onApprove={approveRequest}
          onClose={() => setShowPendingRequests(false)}
        />
      )}

      {/* 방 설정 모달 (방장용) */}
      {settings.showRoomSettings && <RoomSettingsModal room={room} form={settings} />}

      {/* 악기 선택 모달 */}
      {perform.showInstrumentSelect && (
        <InstrumentSelectModal
          slots={room?.instrument_slots}
          getInstrumentUsage={getInstrumentUsage}
          onSelect={(id) => void perform.handleSelectInstrument(id)}
          onClose={perform.closeInstrumentSelect}
        />
      )}

      {/* 악기 변경 요청 모달 */}
      {showInstrumentChangeModal && (
        <InstrumentChangeModal
          slots={room?.instrument_slots}
          myInstrument={myInstrument}
          getInstrumentUsage={getInstrumentUsage}
          onRequest={requestInstrumentChange}
          onClose={() => setShowInstrumentChangeModal(false)}
        />
      )}

      {/* 녹음 목록 모달 */}
      {showRecordings && (
        <RecordingsModal recordings={recordings} onDelete={deleteRecording} onClose={() => setShowRecordings(false)} />
      )}

      {/* 상단 헤더 */}
      <LiveHeader
        room={room}
        displayStatus={displayStatus}
        participantCount={peers.length + 1}
        hostNickname={hostNickname}
        isHost={isHost}
        onOpenInfo={() => setShowRoomInfo(true)}
        onOpenSettings={settings.openRoomSettings}
        onLeave={handleLeave}
      />

      <div className="live-body">
        {/* 왼쪽: 연주자 패널 */}
        <PerformersPanel
          isPerformer={isPerformer}
          isHost={isHost}
          freeJoin={room.free_join}
          performerCount={performerCount}
          remotePerformers={remotePerformers}
          roomFull={roomFull}
          performBusy={perform.performBusy}
          performerRefs={performerRefs}
          performersListRef={performersListRef}
          onOpenInstrumentChange={() => setShowInstrumentChangeModal(true)}
          onOpenPendingRequests={() => setShowPendingRequests(true)}
          onBecomePerformer={perform.handleBecomePerformer}
          onBecomeViewer={perform.handleBecomeViewer}
          onStartAfterApproval={() => void perform.handleStartAfterApproval()}
        />

        {/* 가운데: 믹서 */}
        <MixerPanel
          isPerformer={isPerformer}
          remotePerformers={remotePerformers}
          hasAnyPerformer={hasAnyPerformer}
          broadcasterName={broadcasterName}
          onOpenRecordings={() => setShowRecordings(true)}
          roomTitle={room.title}
        />

        {/* 오른쪽: 채팅 */}
        <ChatPanel chatInput={chatInput} onChatInputChange={setChatInput} />
      </div>

      {/* 악기 변경 요청 말풍선들 (fixed position으로 믹서 위에 표시) */}
      {isHost && (
        <InstrumentChangeBubbles
          requests={pendingInstrumentChanges}
          positions={bubblePositions}
          onApprove={approveInstrumentChange}
          onReject={(id) => rejectInstrumentChange(id)}
        />
      )}
    </div>
  )
}
