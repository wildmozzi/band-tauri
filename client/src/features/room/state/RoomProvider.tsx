/**
 * 합주실 디스패처·조립자 (docs/frontend-architecture.md 4절, ADR D3).
 *
 * 상태는 RoomSessionStore(React 밖)에 있고 화면은 roomHooks의 선택자 훅으로 필요한 조각만 구독한다.
 * 이 컴포넌트가 하는 일:
 * - 시그널링 WS(useSignaling), 엔진 세션(native/PerformSession ← useNativeSession, 스토어에 미러),
 *   브라우저 미디어(useViewerAudio 관람 수신·useWebAudioMixer 웹 믹서·useRecording 녹음), 관람자 방송(native/mixBridge)을 소유
 * - 메시지: 내 메아리 무시 → 엔진 세션 → reduceRoomSignal(순수) → store.update + effects 실행
 * - 명령(입장·퇴장·연주·요청·믹서…)을 한 번 만들어 컨텍스트로 내보낸다(참조가 바뀌지 않아 리렌더를 퍼뜨리지 않는다)
 * - 연주 상태에 따른 엔진 적용(웹 믹서 → 엔진 게인, 모니터, 메트로놈 볼륨)과 방송 시작·정지
 *
 * 결정사항(docs/overhaul/native-engine-design.md): 연주자는 앱 전용, 웹은 관람만. 앱 연주자끼리는 WebRTC를 쓰지 않는다.
 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useAuth } from '../../auth'
import { useAudioSettings } from '../../settings'
import { forgetRoomPassword, rememberRoomPassword } from '../../rooms'
import { useNativeSession } from '../../../native/useNativeSession'
import {
  browserMetronomePrefsStorage,
  clampMetronomeVolume,
  loadMetronomeGain,
  metronomeErrorFromCode,
  saveMetronomeGain,
} from '../../../native/metronomeLogic'
import { MixBroadcaster, MixSource } from '../../../native/mixBridge'
import { isEngineDescriptorErrorCode } from '../../../native/performSession'
import {
  MAX_PERFORMERS,
  engineErrorMessage,
  engineSetMasterGain,
  engineSetMetronomeGain,
  engineSetMonitor,
  isNativeApp,
} from '../../../native/engineApi'
import { appUpdater } from '../../../native/updater'
import { appendChat, makeChatId } from './chatModel'
import { initialPerformRequestState, performRequestReducer, type PerformRequestAction } from './performRequestState'
import { RoomRuntimeContext, type RoomActions, type RoomRuntime } from './roomHooks'
import { initialRoomState, RoomSessionStore, type RoomSnapshot } from './roomSessionStore'
import { reduceRoomSignal, type RoomEffect } from './signalReducer'
import { useRecording, type RecordingSource } from './useRecording'
import { useRoomSelector } from './useRoomSelector'
import { useSignaling, type SignalPayload } from './useSignaling'
import { useViewerAudio } from './useViewerAudio'
import { useWebAudioMixer } from './useWebAudioMixer'

const BROADCAST_RESYNC_MS = 5000

/** 엔진에 붙은 피어 id 목록(문자열) — 통계 틱마다 바뀌는 레벨 값이 아니라 피어 구성이 바뀔 때만 달라진다 */
const enginePeerIdsOf = (s: RoomSnapshot) => (s.perform.stats?.peers ?? []).map((p) => p.peerId).join(',')

const selectProviderView = (s: RoomSnapshot) => ({
  /** 엔진 세션 단계 (스토어 미러가 유일한 원천) */
  phase: s.perform.phase,
  enginePeerIds: enginePeerIdsOf(s),
  currentRoomId: s.connection.currentRoomId,
  isBroadcaster: s.derived.isBroadcaster,
  viewers: s.derived.viewers,
  mixSettingsMap: s.mixer.mixSettingsMap,
  masterVolume: s.mixer.masterVolume,
  masterMuted: s.mixer.masterMuted,
})

export function RoomProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth()
  const audioSettings = useAudioSettings()
  const audioSettingsRef = useRef(audioSettings)
  audioSettingsRef.current = audioSettings
  const userIdRef = useRef(user?.id)
  userIdRef.current = user?.id

  // 게스트 닉네임은 세션 동안 고정
  const [guestSuffix] = useState(() => Math.random().toString(36).slice(2, 6))
  const nickname: string = user?.user_metadata?.nickname || `Guest${guestSuffix}`

  // 클릭 볼륨·뮤트는 사용자별로 로컬에 저장해 방을 다시 들어와도 유지한다(PR #36 L5)
  const [store] = useState(() => {
    const base = initialRoomState()
    const gain = loadMetronomeGain(browserMetronomePrefsStorage(), user?.id)
    return new RoomSessionStore({
      ...base,
      connection: { ...base.connection, nickname },
      local: { ...base.local, metronomeVolume: gain.volume, metronomeMuted: gain.muted },
    })
  })
  useLayoutEffect(() => {
    store.patch('connection', { nickname })
  }, [store, nickname])

  /** 비밀번호 방 입장용 (재연결 시 다시 보낸다). 화면 상태가 아니라 스토어에 두지 않는다 */
  const roomPasswordRef = useRef<string | null>(null)
  /** 재연결 후 보낸 rejoin의 응답(room-state 또는 입장 거부)을 기다리는 중 (PR #23 M3) */
  const rejoiningRef = useRef(false)
  /** password-busy 자동 재시도 */
  const busyRetryRef = useRef<{ count: number; timer: ReturnType<typeof setTimeout> | null }>({ count: 0, timer: null })
  const clearBusyRetry = () => {
    if (busyRetryRef.current.timer) clearTimeout(busyRetryRef.current.timer)
    busyRetryRef.current = { count: 0, timer: null }
  }
  const isReturningToRoomRef = useRef(false)
  /** 녹음이 엔진 믹스를 쓰는 중인지 (엔진 정지 시 녹음도 멈춘다: M4) */
  const recordingFromEngineRef = useRef(false)

  const buildJoinMessage = (isRejoin: boolean): SignalPayload | null => {
    const c = store.getSnapshot().connection
    if (!c.currentRoomId) return null
    // isHost/userId는 구 서버 호환용. 새 서버는 무시하고 auth 토큰과 DB로 판정한다.
    return {
      type: 'join',
      roomId: c.currentRoomId,
      nickname: c.nickname,
      isHost: c.isRoomHost,
      userId: userIdRef.current ?? null,
      ...(roomPasswordRef.current ? { password: roomPasswordRef.current } : {}),
      ...(isRejoin ? { isRejoin: true } : {}),
    }
  }

  // 핸들러는 ref로 넘겨 WS 재연결 없이 최신 클로저를 쓴다
  const onMessageRef = useRef<(p: SignalPayload) => void>(() => {})
  const onReadyRef = useRef<(id: string, resumed: boolean) => void>(() => {})
  const onClosedRef = useRef<() => void>(() => {})
  const onGaveUpRef = useRef<() => void>(() => {})
  const buildJoinMessageRef = useRef(buildJoinMessage)
  buildJoinMessageRef.current = buildJoinMessage

  const { send, clientIdRef, isOpen } = useSignaling({
    onReady: (id, resumed) => onReadyRef.current(id, resumed),
    onMessage: (p) => onMessageRef.current(p),
    onClosed: () => onClosedRef.current(),
    onGaveUp: () => onGaveUpRef.current(),
    isInRoom: () => store.getSnapshot().connection.currentRoomId !== null,
    getJoinMessage: () => buildJoinMessageRef.current(false),
  }, store)

  // --- 연주 세션 (엔진) ---
  const { session } = useNativeSession({
    send,
    getSelfId: () => clientIdRef.current,
    getRoomId: () => store.getSnapshot().connection.currentRoomId,
    getOtherPerformers: () => Object.keys(store.getSnapshot().roster).filter((id) => id !== clientIdRef.current),
  })
  // 엔진 세션 상태를 스토어에 미러 (화면과 Provider 모두 스토어에서 읽는다)
  useEffect(() => {
    store.update({ perform: session.getState() })
    return session.subscribe((p) => store.update({ perform: p }))
  }, [store, session])

  // Provider가 쓰는 조각. 엔진 통계 틱(레벨만 바뀜)에는 값이 그대로라 Provider도 다시 그리지 않는다
  const view = useRoomSelector(store, selectProviderView)
  const { phase, enginePeerIds } = view
  const running = phase === 'running'

  // 연주 세션이 돌면 설정 화면의 테스트를 막는다
  const { setEngineInUse } = audioSettings
  useEffect(() => {
    setEngineInUse(phase !== 'idle')
  }, [phase, setEngineInUse])

  // 악기 변경 결과 표시 타이머
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([])
  useEffect(() => {
    const timers = timersRef.current
    return () => timers.forEach(clearTimeout)
  }, [])

  // --- 브라우저 미디어 ---
  const viewer = useViewerAudio({
    send,
    isAllowedSender: (peerId) => peerId in store.getSnapshot().roster,
    store,
  })
  const mixer = useWebAudioMixer(store)

  const [{ mixSource, broadcaster }] = useState(() => {
    const source = new MixSource()
    return {
      mixSource: source,
      broadcaster: new MixBroadcaster(source, send, (n) => store.patch('local', { broadcastViewerCount: n })),
    }
  })

  const resolveRecordingSource = async (): Promise<RecordingSource | null> => {
    if (session.getState().phase === 'running') {
      const lease = await mixSource.acquire()
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
  }
  const recording = useRecording(resolveRecordingSource, store)

  // 엔진이 멈추면(정지/거부/재시작) 믹스 구독을 무효화하고 그 믹스로 녹음 중이면 녹음을 끝낸다 (M4)
  const { stopRecording } = recording
  useEffect(() => {
    if (running) return
    if (recordingFromEngineRef.current) stopRecording()
    mixSource.invalidate()
  }, [running, stopRecording, mixSource])

  // 방에 있는 동안(엔진 시작 전 대기 포함)은 앱 업데이트 설치를 보류한다 (native/updater)
  useEffect(() => {
    appUpdater.setInRoom(view.currentRoomId !== null)
  }, [view.currentRoomId])

  // 명령과 부수효과가 쓰는 최신 의존성 (명령 객체는 한 번만 만든다)
  const deps = { session, viewer, mixer, recording, send, isOpen, clientIdRef }
  const depsRef = useRef(deps)
  depsRef.current = deps

  const [actions] = useState<RoomActions>(() => {
    const d = () => depsRef.current
    const conn = () => store.getSnapshot().connection
    const setLocalError = (localError: string | null) => store.patch('local', { localError })
    const reduceRequests = (action: PerformRequestAction) =>
      store.patch('requests', (r) => ({ perform: performRequestReducer(r.perform, action) }))
    const sendJoin = () => {
      const join = buildJoinMessageRef.current(false)
      if (join) d().send(join)
    }

    const leaveRoom = () => {
      const { send: sendMsg, session: s, viewer: v, mixer: m, recording: rec } = d()
      sendMsg({ type: 'leave' })
      roomPasswordRef.current = null
      clearBusyRetry()
      // 방 id를 먼저 비워야 시작 중인 엔진이 완료 시 "퇴장함"을 보고 스스로 내려간다 (H2)
      store.patch('connection', { currentRoomId: null, joinError: null, roomJoined: false })
      broadcaster.stop()
      // leave가 participant-left로 전파되므로 native-leave/stop-performing은 따로 보내지 않는다
      void s.stop({ notify: false })
      v.closeAll()
      m.reset()
      rec.stopRecording()
      store.update((st) => ({
        chat: [],
        requests: { perform: initialPerformRequestState, pendingInstrumentChanges: [], myInstrumentChangeStatus: 'none', myNewInstrument: null },
        connection: { ...st.connection, isRoomHost: false, peers: [], joinFeedback: '' },
        roster: {},
        local: { ...st.local, localError: null },
        miniPlayer: { active: false },
      }))
    }

    const startPerforming = async (instrument: string): Promise<boolean> => {
      const { session: s, clientIdRef: idRef } = d()
      setLocalError(null)
      s.clearError()
      if (!isNativeApp()) {
        setLocalError('연주는 데스크톱 앱에서만 할 수 있습니다.')
        return false
      }
      const selfId = idRef.current
      if (!selfId || !conn().currentRoomId) {
        setLocalError('합주실에 연결되지 않았습니다.')
        return false
      }
      const others = Object.keys(store.getSnapshot().roster).filter((id) => id !== selfId)
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
      const ok = await s.start({
        instrument,
        config: settings.config,
        nickname: conn().nickname,
        isHost: conn().isRoomHost,
      })
      if (ok) {
        // 연주자는 관람 수신을 하지 않는다
        d().viewer.closeAll()
        reduceRequests({ type: 'started' })
      }
      return ok
    }

    /** 클릭 볼륨을 화면 상태와 엔진에 적용한다. 엔진 제어값은 엔진 재시작 간에도 유지되므로 연주 중이 아니어도 보낸다(앱에서만) */
    const applyMetronomeGain = (volume: number, muted: boolean) => {
      store.patch('local', { metronomeVolume: volume, metronomeMuted: muted })
      if (isNativeApp()) {
        engineSetMetronomeGain(volume, muted).catch((err) => console.warn('[Native] 메트로놈 볼륨 실패:', engineErrorMessage(err)))
      }
    }

    return {
      joinRoom: (roomId, isHost, password) => {
        if (!d().isOpen()) {
          store.patch('connection', { joinFeedback: '시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.' })
          return
        }
        roomPasswordRef.current = password || null
        rejoiningRef.current = false
        clearBusyRetry()
        store.patch('connection', { currentRoomId: roomId, joinError: null, roomJoined: false, isRoomHost: isHost === true })
        sendJoin()
        store.patch('connection', { joinFeedback: '룸 입장 시도 중...' })
      },
      submitRoomPassword: (password) => {
        if (!conn().currentRoomId || !password) return
        roomPasswordRef.current = password
        store.patch('connection', { joinError: null })
        const join = buildJoinMessageRef.current(false)
        if (join && d().isOpen()) d().send(join)
        store.patch('connection', { joinFeedback: '룸 입장 시도 중...' })
      },
      /** 정원 초과 등으로 거부된 입장을 같은 조건으로 다시 시도 (PR #23 L3) */
      retryJoin: () => {
        if (!conn().currentRoomId) return
        if (!d().isOpen()) {
          store.patch('connection', { joinFeedback: '시그널링 서버에 연결 중입니다. 잠시 후 다시 시도해주세요.' })
          return
        }
        store.patch('connection', { joinError: null })
        sendJoin()
        store.patch('connection', { joinFeedback: '룸 입장 시도 중...' })
      },
      leaveRoom,

      startPerforming,
      stopPerforming: async () => {
        broadcaster.stop()
        await d().session.stop({ notify: true })
      },
      setMonitor: (enabled, gain) => {
        const g = Math.max(0, Math.min(1, gain))
        store.patch('local', { isMonitoringEnabled: enabled, monitorGain: g })
        if (d().session.getState().phase === 'running') {
          engineSetMonitor(enabled, g).catch((err) => console.warn('[Native] 모니터 설정 실패:', engineErrorMessage(err)))
        }
      },
      toggleInputMuted: () => {
        const s = d().session
        s.setInputMuted(!s.getState().inputMuted)
      },

      setHostMetronome: (on, bpm, beatsPerBar) => d().session.setHostMetronome(on, bpm, beatsPerBar),
      setMetronomeVolume: (volume, muted) => {
        const v = clampMetronomeVolume(volume)
        applyMetronomeGain(v, muted)
        saveMetronomeGain(browserMetronomePrefsStorage(), userIdRef.current, { volume: v, muted })
      },

      setMixVolume: (peerId, volume) => d().mixer.setMixVolume(peerId, volume),
      setMixPan: (peerId, pan) => d().mixer.setMixPan(peerId, pan),
      setMixMuted: (peerId, muted) => d().mixer.setMixMuted(peerId, muted),
      setMasterVolume: (volume) => d().mixer.setMasterVolume(volume),
      toggleMasterMute: () => d().mixer.toggleMasterMute(),
      setMasterPan: (pan) => d().mixer.setMasterPan(pan),
      resumeAllAudioContexts: () => {
        d().mixer.resume()
        mixSource.resume()
      },

      sendChatMessage: (message) => {
        const text = message.trim()
        if (!text || !d().isOpen()) return
        const timestamp = Date.now()
        const name = conn().nickname
        d().send({ type: 'chat', message: text, nickname: name, timestamp })
        const me = d().clientIdRef.current ?? 'me'
        store.update((st) => ({ chat: appendChat(st.chat, { id: makeChatId(me, timestamp), peerId: me, nickname: name, message: text, timestamp }) }))
      },

      dismissRequestNotice: () => reduceRequests({ type: 'dismiss-notice' }),
      approveRequest: (targetId) => {
        if (!d().isOpen()) return
        d().send({ type: 'approve-perform', targetId })
        reduceRequests({ type: 'resolved', targetId })
      },
      rejectRequest: (targetId, reason) => {
        if (!d().isOpen()) return
        const trimmed = reason?.trim()
        d().send({ type: 'reject-perform', targetId, ...(trimmed ? { reason: trimmed.slice(0, 200) } : {}) })
        reduceRequests({ type: 'resolved', targetId })
      },
      requestPerform: (instrument) => {
        if (!d().isOpen()) return
        d().send({ type: 'request-perform', instrument })
        reduceRequests({ type: 'requested', instrument })
      },
      cancelRequest: () => {
        if (d().isOpen()) d().send({ type: 'cancel-perform-request' })
        reduceRequests({ type: 'cancelled' })
      },
      dismissRejection: () => reduceRequests({ type: 'cancelled' }),
      approveInstrumentChange: (targetId) => {
        if (!d().isOpen()) return
        d().send({ type: 'approve-instrument-change', targetId })
        store.patch('requests', (r) => ({ pendingInstrumentChanges: r.pendingInstrumentChanges.filter((x) => x.oderId !== targetId) }))
      },
      rejectInstrumentChange: (targetId, reason) => {
        if (!d().isOpen()) return
        d().send({ type: 'reject-instrument-change', targetId, reason })
        store.patch('requests', (r) => ({ pendingInstrumentChanges: r.pendingInstrumentChanges.filter((x) => x.oderId !== targetId) }))
      },
      requestInstrumentChange: (newInstrument) => {
        if (!d().isOpen()) return
        d().send({ type: 'request-instrument-change', newInstrument })
        store.patch('requests', { myNewInstrument: newInstrument })
      },
      cancelInstrumentChangeRequest: () => {
        if (d().isOpen()) d().send({ type: 'cancel-instrument-change-request' })
        store.patch('requests', { myInstrumentChangeStatus: 'none', myNewInstrument: null })
      },

      startRecording: () => void d().recording.startRecording(),
      stopRecording: () => d().recording.stopRecording(),
      deleteRecording: (id) => d().recording.deleteRecording(id),

      enterMiniPlayerMode: () => {
        if (isReturningToRoomRef.current) {
          isReturningToRoomRef.current = false
          return
        }
        if (!conn().currentRoomId) return
        store.patch('miniPlayer', { active: true })
      },
      exitMiniPlayerMode: () => {
        store.patch('miniPlayer', { active: false })
        leaveRoom()
      },
      returnToRoom: () => {
        const roomId = conn().currentRoomId
        if (!roomId) return null
        isReturningToRoomRef.current = true
        store.patch('miniPlayer', { active: false })
        return roomId
      },
      clearMiniPlayerMode: () => {
        if (store.getSnapshot().miniPlayer.active) isReturningToRoomRef.current = true
        store.patch('miniPlayer', { active: false })
      },
    }
  })

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
    store.patch('connection', { peers: [] })
    viewer.closeAll()
    // 엔진(UDP 메시)은 유지한다. 재연결/세션 재개 여부는 onReady에서 결정 (M6)
    session.connectionLost()
  }

  onGaveUpRef.current = () => {
    // 재연결 예산(35초, 서버 세션 유예 30초 + 여유)을 넘겼다: 서버 쪽 세션은 이미 정리됐으므로 엔진도 멈춘다.
    // 화면은 signalStatus='gave-up'을 보고 "연결이 끊겼습니다 — 다시 입장" 안내를 띄운다.
    broadcaster.stop()
    void session.stop({ notify: false })
  }

  const runEffect = (e: RoomEffect) => {
    switch (e.type) {
      case 'stop-broadcast': broadcaster.stop(); return
      case 'stop-session': void session.stop({ notify: false }); return
      case 'forget-password': forgetRoomPassword(e.roomId); return
      case 'clear-password': roomPasswordRef.current = null; return
      case 'remember-password':
        if (roomPasswordRef.current) rememberRoomPassword(e.roomId, roomPasswordRef.current)
        return
      case 'set-rejoining': rejoiningRef.current = e.value; return
      case 'clear-busy-retry': clearBusyRetry(); return
      case 'schedule-busy-retry': {
        const retry = busyRetryRef.current
        retry.count++
        if (retry.timer) clearTimeout(retry.timer)
        retry.timer = setTimeout(() => {
          retry.timer = null
          if (store.getSnapshot().connection.currentRoomId !== e.roomId) return
          const join = buildJoinMessageRef.current(false)
          if (join && isOpen()) send(join)
        }, e.delayMs)
        return
      }
      case 'performer-appeared': session.performerAppeared(e.peerId); return
      case 'peer-gone': session.peerGone(e.peerId); return
      case 'close-viewer': viewer.closeFrom(e.peerId); return
      case 'viewer-offer': void viewer.handleOffer(e.from, e.offer); return
      case 'broadcast-answer': void broadcaster.handleAnswer(e.from, e.answer); return
      case 'ice-candidate':
        if (broadcaster.hasPeer(e.from)) void broadcaster.handleCandidate(e.from, e.candidate)
        else void viewer.handleCandidate(e.from, e.candidate)
        return
      case 'my-request-approved':
        // 승인되면 앱은 바로 엔진을 시작한다
        if (!e.instrument || !isNativeApp() || session.getState().phase !== 'idle') return
        void actions.startPerforming(e.instrument)
        return
      case 'my-instrument-changed': session.updateInstrument(e.instrument); return
      case 'reset-instrument-change-after':
        timersRef.current.push(setTimeout(() => store.patch('requests', { myInstrumentChangeStatus: 'none', myNewInstrument: null }), e.ms))
        return
      case 'server-error': console.warn('[WS] 서버 에러:', e.code, e.message); return
    }
  }

  onMessageRef.current = (p: SignalPayload) => {
    const self = clientIdRef.current
    const from = typeof p.from === 'string' ? p.from : null
    if (from && from === self) return

    // native-descriptor / native-leave / relay-token / 연주 거부 에러(perform-not-approved, room-full-performers,
    // login-required) / ssrc-conflict·key-conflict(엔진 재시작만)·descriptor-invalid(엔진 정지·안내) — 연주 거부가 아니므로 방송은 유지
    if (session.handleSignal(p)) {
      // 연주 거부 에러만 방송을 멈춘다(descriptor 거부는 엔진만, 메트로놈 에러는 메트로놈만)
      if (p.type === 'error' && !isEngineDescriptorErrorCode(p.code) && !metronomeErrorFromCode(p.code)) broadcaster.stop()
      return
    }

    const result = reduceRoomSignal(store.getSnapshot(), p, {
      selfId: self,
      rejoining: rejoiningRef.current,
      busyRetryCount: busyRetryRef.current.count,
      now: Date.now,
      makeChatId,
    })
    store.update(result.patch)
    result.effects.forEach(runEffect)
  }

  // --- 방송 담당 (관람자 브릿지) ---
  const { isBroadcaster, viewers } = view
  const viewersRef = useRef(viewers)
  viewersRef.current = viewers

  useEffect(() => {
    if (isBroadcaster) {
      broadcaster.start(viewersRef.current).catch((err) => {
        console.error('[MixBridge] 방송 시작 실패:', err)
        store.patch('local', { localError: `관람자 방송 시작 실패: ${engineErrorMessage(err)}` })
      })
    } else {
      broadcaster.stop()
    }
  }, [isBroadcaster, broadcaster, store])

  useEffect(() => {
    if (isBroadcaster) broadcaster.syncViewers(viewers)
  }, [isBroadcaster, viewers, broadcaster])

  // 실패/시간 초과한 관람자 연결 재시도
  useEffect(() => {
    if (!isBroadcaster) return
    const timer = setInterval(() => broadcaster.syncViewers(viewersRef.current), BROADCAST_RESYNC_MS)
    return () => clearInterval(timer)
  }, [isBroadcaster, broadcaster])

  useEffect(() => () => {
    broadcaster.stop()
  }, [broadcaster])

  // --- 엔진 믹서 적용 (연주 중) ---
  const { mixSettingsMap, masterVolume, masterMuted } = view
  const appliedGainRef = useRef<Record<string, string>>({})
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
  useEffect(() => {
    if (!running) return
    const { isMonitoringEnabled, monitorGain } = store.getSnapshot().local
    engineSetMonitor(isMonitoringEnabled, monitorGain).catch(() => {})
  }, [running, store])

  // 시작할 때(그리고 로그인 사용자가 바뀌면) 저장값을 화면·엔진에 한 번 맞춘다. 앱을 다시 띄워도 엔진(MixControls)과
  // 화면이 어긋나지 않게 한다(PR #36 L5)
  useEffect(() => {
    const prefs = loadMetronomeGain(browserMetronomePrefsStorage(), user?.id)
    store.patch('local', { metronomeVolume: prefs.volume, metronomeMuted: prefs.muted })
    if (isNativeApp()) {
      engineSetMetronomeGain(prefs.volume, prefs.muted).catch((err) => console.warn('[Native] 메트로놈 볼륨 실패:', engineErrorMessage(err)))
    }
  }, [user?.id, store])

  const runtime = useMemo<RoomRuntime>(() => ({ store, actions }), [store, actions])
  return <RoomRuntimeContext.Provider value={runtime}>{children}</RoomRuntimeContext.Provider>
}
