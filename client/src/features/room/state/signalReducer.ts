/**
 * 시그널링 메시지 → 방 상태 변경 + 부수효과 기술 (순수 함수, React·WS 없이 테스트).
 *
 * RoomProvider가 내 메시지 메아리를 거르고 엔진 세션(session.handleSignal)에 먼저 준 뒤, 거기서 처리하지 않은 메시지를
 * 여기 넘긴다. 처리 순서: 채팅 → 참여·악기 변경 요청 → 입장 오류 → 참가자/WebRTC (설계 문서 4절).
 * 엔진·WebRTC·저장소·타이머 호출은 하지 않고 `effects`로 돌려준다 — 실행은 RoomProvider.
 *
 * 신뢰 규칙: 발신자는 서버가 붙인 `from`/`oderId`만 쓴다.
 */
import { joinErrorFromSignal, joinErrorStopsSession } from '../../rooms'
import { appendChat, type ChatMessage } from './chatModel'
import {
  INSTRUMENT_CHANGE_RESULT_MS,
  isInstrumentChangeRequest,
  isPerformRequestSignal,
  performRequestReducer,
} from './performRequestState'
import type { RoomPatch, RoomState } from './roomSessionStore'
import { withPeer, type PeerInstrument } from './roomState'
import type { SignalPayload } from './useSignaling'

export type RoomEffect =
  /** 관람자 방송 중지 */
  | { type: 'stop-broadcast' }
  /** 엔진 세션 정지(알림 없이) */
  | { type: 'stop-session' }
  | { type: 'forget-password'; roomId: string }
  /** 기억해 둔 입장 비밀번호(재연결용) 비우기 */
  | { type: 'clear-password' }
  /** 입장 성공: 입력한 비밀번호가 있으면 세션 동안 기억 */
  | { type: 'remember-password'; roomId: string }
  | { type: 'set-rejoining'; value: boolean }
  | { type: 'clear-busy-retry' }
  /** password-busy: 같은 조건으로 다시 입장 시도 예약 */
  | { type: 'schedule-busy-retry'; roomId: string; delayMs: number }
  | { type: 'performer-appeared'; peerId: string }
  | { type: 'peer-gone'; peerId: string }
  | { type: 'close-viewer'; peerId: string }
  | { type: 'viewer-offer'; from: string; offer: RTCSessionDescriptionInit }
  | { type: 'broadcast-answer'; from: string; answer: RTCSessionDescriptionInit }
  /** 방송 담당이 그 피어에 연결 중이면 방송 쪽, 아니면 관람 수신 쪽 */
  | { type: 'ice-candidate'; from: string; candidate: RTCIceCandidateInit }
  /** 내 연주 참여 요청 승인 (앱: 엔진 자동 시작) */
  | { type: 'my-request-approved'; instrument: string | null }
  /** 내 악기 변경 승인 (엔진 세션 악기 갱신) */
  | { type: 'my-instrument-changed'; instrument: string }
  /** 악기 변경 결과 표시 후 기본 상태로 */
  | { type: 'reset-instrument-change-after'; ms: number }
  | { type: 'server-error'; code: unknown; message: unknown }

export interface SignalContext {
  selfId: string | null
  /** 재연결 후 보낸 rejoin의 응답을 기다리는 중 */
  rejoining: boolean
  /** password-busy 자동 재시도 횟수 */
  busyRetryCount: number
  now: () => number
  makeChatId: (senderId: string, timestamp: number) => string
}

export interface SignalResult {
  /** 이 메시지를 처리했는지 (모르는 메시지면 false) */
  handled: boolean
  patch: RoomPatch | null
  effects: RoomEffect[]
}

export const MAX_BUSY_RETRIES = 5

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

function performerOf(p: ParticipantPayload & { instrument: string }): PeerInstrument {
  return { peerId: p.oderId, instrument: p.instrument, nickname: p.nickname || `User ${p.oderId.slice(0, 4)}`, isHost: p.isHost === true }
}

function withoutKey<T>(obj: Record<string, T>, key: string): Record<string, T> {
  if (!(key in obj)) return obj
  const next = { ...obj }
  delete next[key]
  return next
}

const UNHANDLED: SignalResult = { handled: false, patch: null, effects: [] }

export function reduceRoomSignal(s: RoomState, p: SignalPayload, ctx: SignalContext): SignalResult {
  return reduceChat(s, p, ctx) ?? reduceRequests(s, p) ?? reduceJoinError(s, p, ctx) ?? reduceParticipants(s, p, ctx) ?? UNHANDLED
}

// --- 채팅 ---

function reduceChat(s: RoomState, p: SignalPayload, ctx: SignalContext): SignalResult | null {
  if (p.type !== 'chat') return null
  const senderId = p.oderId
  if (typeof senderId !== 'string' || typeof p.message !== 'string') return { handled: true, patch: null, effects: [] }
  const timestamp = typeof p.timestamp === 'number' ? p.timestamp : ctx.now()
  const msg: ChatMessage = {
    id: ctx.makeChatId(senderId, timestamp),
    peerId: senderId,
    nickname: typeof p.nickname === 'string' ? p.nickname : `User ${senderId.slice(0, 4)}`,
    message: p.message,
    timestamp,
  }
  return { handled: true, patch: { chat: appendChat(s.chat, msg) }, effects: [] }
}

// --- 연주 참여·악기 변경 요청 ---

function reduceRequests(s: RoomState, p: SignalPayload): SignalResult | null {
  const req = s.requests
  if (isPerformRequestSignal(p.type)) {
    const perform = performRequestReducer(req.perform, { type: 'signal', payload: p })
    const effects: RoomEffect[] = []
    if (p.type === 'perform-request-approved') {
      effects.push({ type: 'my-request-approved', instrument: typeof p.instrument === 'string' ? p.instrument : req.perform.myInstrument })
    }
    return { handled: true, patch: perform === req.perform ? null : { requests: { ...req, perform } }, effects }
  }
  const done = (requests: RoomState['requests'] | null, effects: RoomEffect[] = []): SignalResult =>
    ({ handled: true, patch: requests ? { requests } : null, effects })
  switch (p.type) {
    case 'instrument-change-request-sent':
      return done({ ...req, myInstrumentChangeStatus: 'pending' })
    case 'instrument-change-request-received': {
      if (!isInstrumentChangeRequest(p.request)) return done(null)
      const r = p.request
      return done(req.pendingInstrumentChanges.some((x) => x.oderId === r.oderId) ? null : { ...req, pendingInstrumentChanges: [...req.pendingInstrumentChanges, r] })
    }
    case 'instrument-change-request-cancelled': {
      if (typeof p.oderId !== 'string') return done(null)
      const id = p.oderId
      return done({ ...req, pendingInstrumentChanges: req.pendingInstrumentChanges.filter((r) => r.oderId !== id) })
    }
    case 'instrument-change-approved':
      if (typeof p.newInstrument !== 'string') return done(null)
      return done({ ...req, myInstrumentChangeStatus: 'approved' }, [
        { type: 'my-instrument-changed', instrument: p.newInstrument },
        { type: 'reset-instrument-change-after', ms: INSTRUMENT_CHANGE_RESULT_MS.approved },
      ])
    case 'instrument-change-rejected':
      return done({ ...req, myInstrumentChangeStatus: 'rejected' }, [
        { type: 'reset-instrument-change-after', ms: INSTRUMENT_CHANGE_RESULT_MS.rejected },
      ])
    case 'participant-instrument-changed': {
      if (typeof p.oderId !== 'string' || typeof p.instrument !== 'string') return done(null)
      const prev = s.roster[p.oderId]
      return { handled: true, patch: prev ? { roster: { ...s.roster, [p.oderId]: { ...prev, instrument: p.instrument } } } : null, effects: [] }
    }
    default:
      return null
  }
}

// --- 입장 오류 (비밀번호·정원) ---

function reduceJoinError(s: RoomState, p: SignalPayload, ctx: SignalContext): SignalResult | null {
  const je = joinErrorFromSignal(p)
  if (!je) return null
  const roomId = s.connection.currentRoomId
  if (je.roomId && je.roomId !== roomId) return { handled: true, patch: null, effects: [] }
  const effects: RoomEffect[] = []
  if (roomId && je.code === 'password-invalid') effects.push({ type: 'forget-password', roomId })
  if (je.code === 'password-required' || je.code === 'password-invalid') effects.push({ type: 'clear-password' })
  const rejoining = ctx.rejoining
  if (je.code !== 'password-busy') effects.push({ type: 'set-rejoining', value: false })
  // 방 밖으로 밀려났다: 엔진·방송을 멈춘다 (서버는 방 밖 연결의 연주 메시지를 거부한다)
  if (joinErrorStopsSession(je, rejoining)) effects.push({ type: 'stop-broadcast' }, { type: 'stop-session' })
  // 서버 혼잡/방 준비 중: 같은 비밀번호로 몇 번 자동 재시도
  if (je.code === 'password-busy' && roomId && ctx.busyRetryCount < MAX_BUSY_RETRIES) {
    effects.push({ type: 'schedule-busy-retry', roomId, delayMs: Math.min(Math.max(je.retryAfterMs ?? 1000, 500), 10_000) })
  }
  return {
    handled: true,
    patch: {
      connection: { ...s.connection, joinError: rejoining ? { ...je, afterReconnect: true } : je, joinFeedback: '', roomJoined: false },
    },
    effects,
  }
}

// --- 참가자·연주자 / WebRTC 중계 ---

function reduceParticipants(s: RoomState, p: SignalPayload, ctx: SignalContext): SignalResult | null {
  const self = ctx.selfId
  const from = typeof p.from === 'string' ? p.from : null
  const conn = s.connection
  const ensurePeer = (peerId: string | null): RoomPatch | null => {
    const peers = withPeer(conn.peers, peerId, self)
    return peers === conn.peers ? null : { connection: { ...conn, peers } }
  }

  switch (p.type) {
    case 'room-state': {
      const peerIds = Array.isArray(p.peerIds) ? (p.peerIds as unknown[]).filter((x): x is string => typeof x === 'string' && x !== self) : []
      const participants = Array.isArray(p.participants) ? (p.participants as unknown[]).filter(isParticipant) : []
      const roster: Record<string, PeerInstrument> = {}
      for (const part of participants) {
        if (part.isPerforming && part.instrument && part.oderId !== self) roster[part.oderId] = performerOf({ ...part, instrument: part.instrument })
      }
      const effects: RoomEffect[] = [{ type: 'set-rejoining', value: false }, { type: 'clear-busy-retry' }]
      if (conn.currentRoomId) effects.push({ type: 'remember-password', roomId: conn.currentRoomId })
      return {
        handled: true,
        patch: {
          connection: {
            ...conn,
            ...(typeof p.isHost === 'boolean' ? { isRoomHost: p.isHost } : {}),
            peers: peerIds,
            joinError: null,
            roomJoined: true,
            joinFeedback: `룸 입장 완료 · 동시 연결 ${peerIds.length + 1}명`,
          },
          roster,
        },
        effects,
      }
    }

    case 'participant-joined': {
      const part = p.participant
      if (!isParticipant(part)) return { handled: true, patch: null, effects: [] }
      const patch: RoomPatch = { ...ensurePeer(part.oderId) }
      const effects: RoomEffect[] = []
      if (part.isPerforming && part.instrument) {
        if (part.oderId !== self) patch.roster = { ...s.roster, [part.oderId]: performerOf({ ...part, instrument: part.instrument }) }
        effects.push({ type: 'performer-appeared', peerId: part.oderId })
      }
      return { handled: true, patch, effects }
    }

    case 'participant-left': {
      if (typeof p.oderId !== 'string') return { handled: true, patch: null, effects: [] }
      const id = p.oderId
      // 바뀐 것이 없는 조각은 패치에 넣지 않는다(기존 참조 유지) — 그 조각을 구독한 화면을 다시 그리지 않는다
      const patch: RoomPatch = {}
      const peers = conn.peers.filter((x) => x !== id)
      if (peers.length !== conn.peers.length) patch.connection = { ...conn, peers }
      const roster = withoutKey(s.roster, id)
      if (roster !== s.roster) patch.roster = roster
      const perform = performRequestReducer(s.requests.perform, { type: 'peer-left', peerId: id })
      const changes = s.requests.pendingInstrumentChanges
      const remaining = changes.filter((r) => r.oderId !== id)
      const changesTouched = remaining.length !== changes.length
      if (perform !== s.requests.perform || changesTouched) {
        patch.requests = { ...s.requests, perform, pendingInstrumentChanges: changesTouched ? remaining : changes }
      }
      return { handled: true, patch, effects: [{ type: 'peer-gone', peerId: id }, { type: 'close-viewer', peerId: id }] }
    }

    case 'performer-updated': {
      if (typeof p.oderId !== 'string') return { handled: true, patch: null, effects: [] }
      const id = p.oderId
      const patch: RoomPatch = { ...ensurePeer(id) }
      if (p.isPerforming === true && typeof p.instrument === 'string') {
        if (id !== self) {
          patch.roster = { ...s.roster, [id]: performerOf({ oderId: id, nickname: p.nickname as string | undefined, instrument: p.instrument, isHost: p.isHost === true }) }
        }
        return { handled: true, patch, effects: [{ type: 'performer-appeared', peerId: id }] }
      }
      patch.roster = withoutKey(s.roster, id)
      return { handled: true, patch, effects: [{ type: 'peer-gone', peerId: id }, { type: 'close-viewer', peerId: id }] }
    }

    case 'offer': {
      // 관람자만 받는다. 앱 연주자는 엔진으로 듣는다.
      const effects: RoomEffect[] = []
      if (from && p.offer && s.perform.phase === 'idle') effects.push({ type: 'viewer-offer', from, offer: p.offer as RTCSessionDescriptionInit })
      return { handled: true, patch: ensurePeer(from), effects }
    }

    case 'answer':
      return {
        handled: true,
        patch: ensurePeer(from),
        effects: from && p.answer ? [{ type: 'broadcast-answer', from, answer: p.answer as RTCSessionDescriptionInit }] : [],
      }

    case 'ice-candidate':
      if (!from || !p.candidate) return { handled: true, patch: null, effects: [] }
      return { handled: true, patch: ensurePeer(from), effects: [{ type: 'ice-candidate', from, candidate: p.candidate as RTCIceCandidateInit }] }

    case 'error':
      return { handled: true, patch: null, effects: [{ type: 'server-error', code: p.code, message: p.message }] }

    default:
      return null
  }
}
