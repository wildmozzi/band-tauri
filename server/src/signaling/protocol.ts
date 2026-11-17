/**
 * 시그널링 계약의 TS 표현 (설계: docs/overhaul/native-engine-design.md 5절, docs/server-architecture.md 2.1절).
 * 상수·수신 메시지 타입·메시지 페이로드 타입과, 아래쪽에 송신 메시지(서버 → 클라이언트) 타입.
 * 이 파일은 다른 모듈을 import하지 않는다(실행 코드 없이 타입만 가져다 쓸 수 있게: e2e mock이 import type 한다).
 */

export const MAX_MESSAGE_BYTES = 64 * 1024
/**
 * 연결 하나의 보내기 대기열(ws.bufferedAmount) 상한. 읽지 않는(느린) 수신자에게 중계가 쌓이면 서버 메모리가 끝없이 늘어
 * 256MB VM이 OOM으로 죽는다(모든 방 합주 끊김). 정상 시그널링은 수십 KB를 넘지 않으므로 넘으면 그 연결을 끊는다(registry.send).
 */
export const MAX_BUFFERED_BYTES = 1024 * 1024
export const AUTH_TIMEOUT_MS = 5000
/**
 * 관람 WebRTC 중계 본문 상한 (#184 리뷰 M). 앱의 오디오 전용 SDP는 수 KB(후보는 trickle로 따로 보냄)라 16KB면 넉넉하다.
 * 후보 한 줄(candidate:…)은 수백 바이트. 넘으면 중계하지 않는다.
 */
export const MAX_SDP_LENGTH = 16 * 1024
export const MAX_ICE_CANDIDATE_LENGTH = 1024
export const MAX_ICE_FIELD_LENGTH = 256
export const MAX_PENDING_MESSAGES = 100
export const MAX_ROOM_ID_LENGTH = 128
export const MAX_NICKNAME_LENGTH = 50
export const MAX_CHAT_LENGTH = 1000
export const MAX_REJECT_REASON_LENGTH = 200
export const ROOM_META_TTL_MS = 30_000
export const REJOIN_WINDOW_MS = 60_000
export const APPROVAL_GRACE_MS = 60_000
export const TOKEN_EXPIRY_GRACE_MS = 60_000
export const PARTICIPANT_UPDATE_DEBOUNCE_MS = 1000
/** join 중 방 조회가 실패했을 때(room-unavailable) 클라이언트에게 권하는 재시도 간격 */
export const ROOM_UNAVAILABLE_RETRY_MS = 2000

/** 길이 제한이 있는 비어 있지 않은 문자열 (S3: wsService에서 이동, 순수) */
export function isNonEmptyString(v: unknown, max = 256): v is string {
  return typeof v === 'string' && v.length > 0 && v.length <= max
}

export function sanitizeNickname(v: unknown): string | undefined {
  if (typeof v !== 'string') return undefined
  const trimmed = v.trim().slice(0, MAX_NICKNAME_LENGTH)
  return trimmed || undefined
}

/**
 * 앞 `max` UTF-16 코드 유닛까지 자르되 코드 포인트 경계를 지킨다: 경계에 서로게이트 쌍(이모지 등)이
 * 걸리면 짝 잃은 상위 서로게이트를 버린다(결과는 `max` 유닛 이하, 깨진 글자 없음).
 */
export function truncateCodePoints(s: string, max: number): string {
  if (s.length <= max) return s
  const code = s.charCodeAt(max - 1)
  return s.slice(0, code >= 0xd800 && code <= 0xdbff ? max - 1 : max)
}

/**
 * 방장 거절 사유(`reject-perform`·`reject-instrument-change` 공통, P2-11): 문자열이 아니거나
 * 앞뒤 공백을 뺀 값이 비면 `fallback`, 아니면 앞뒤 공백을 뺀 앞 200 코드 유닛(코드 포인트 경계에서 자름).
 * 받는 쪽 UI가 빈 사유·공백 사유를 따로 걸러내지 않아도 서버가 늘 보여줄 문구를 채운다.
 */
export function rejectReason(v: unknown, fallback: string): string {
  return (typeof v === 'string' ? truncateCodePoints(v.trim(), MAX_REJECT_REASON_LENGTH) : '') || fallback
}

export interface Participant {
  oderId: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
}

export interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

export interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

export type Msg = Record<string, unknown> & { type: string }

/** 공유 메트로놈(P2-3) 계획. startUs는 방장 엔진 시계(µs) 기준 박 0 시각 */
export interface MetronomeState {
  running: boolean
  bpm: number
  beatsPerBar: number
  startUs: number
}

export interface EngineCandidate { kind: 'lan' | 'public'; addr: string }

// ---------- 송신 메시지(서버 → 클라이언트) ----------
// 모양의 기준은 서버 코드와 test/fixtures/ws-transcript.json이다. 서버 send는 아직 이 타입으로 검사하지 않는다
// (S3은 위치만 옮겨 아직 하지 않음 — S4 이후). 지금은 e2e mock(client/e2e/mocks/signaling.ts)이 이 타입으로 맞춰 mock이 서버와 어긋나면 e2e tsc가 실패한다.

/** 관람 중계 본문: RTCSessionDescriptionInit 중 type·sdp (type은 offer|answer|pranswer|rollback) */
export interface SessionDescription { type?: string; sdp: string }
/** 관람 중계 본문: RTCIceCandidateInit 중 candidate(빈 문자열 = 후보 끝)·sdpMid·sdpMLineIndex(0..65535)·usernameFragment */
export interface IceCandidate { candidate: string; sdpMid?: string | null; sdpMLineIndex?: number | null; usernameFragment?: string | null }

export interface EngineDescriptor {
  ssrc: number
  key: string
  candidates: EngineCandidate[]
}

type ErrorOf<C extends string, X = Record<never, never>> = { type: 'error'; code: C; message: string } & X

export type ServerError =
  | ErrorOf<'auth-required' | 'rate-limited', { rejectedType: string }>
  | ErrorOf<'room-not-found'>
  | ErrorOf<'password-required', { roomId: string }>
  | ErrorOf<'password-invalid', { roomId: string; remainingAttempts: number }>
  | ErrorOf<'password-rate-limited' | 'password-busy' | 'room-unavailable', { roomId: string; retryAfterMs: number }>
  | ErrorOf<'room-full', { reason: 'room-full' | 'anonymous-limit'; roomId: string; maxParticipants: number }>
  | ErrorOf<'login-required' | 'perform-not-approved' | 'room-full-performers' | 'descriptor-invalid' | 'key-conflict' | 'ssrc-conflict' | 'metronome-invalid' | 'metronome-host-only' | 'demote-not-performer'>

/** 방 안 브로드캐스트(broadcastToRoom)가 붙이는 보낸 사람 표시. 둘 다 보낸 사람의 clientId */
type FromSender = { oderId: string; from: string }

export type ServerMessage =
  | { type: 'welcome'; clientId: string; resumed?: true }
  /** userId: auth 타임아웃(AUTH_TIMEOUT_MS) 경로는 이 키 없이 보낸다 */
  | { type: 'auth-result'; authenticated: boolean; userId?: string | null; resumed?: true }
  /** 앱 수준 ping에 대한 응답 (P1-15) */
  | { type: 'pong' }
  | ServerError
  /** isPerforming: 받는 사람 자신의 연주 상태(서버 기준, P2-9) */
  | { type: 'room-state'; participants: Participant[]; peerIds: string[]; isHost: boolean; isPerforming: boolean }
  | ({ type: 'participant-joined'; participant: Participant; rejoin: boolean } & FromSender)
  | { type: 'participant-left'; oderId: string }
  | ({ type: 'performer-updated'; nickname: string; instrument?: string; isPerforming: boolean; isHost?: boolean } & FromSender)
  | ({ type: 'chat'; message: string; nickname: string; timestamp: number } & FromSender)
  /** 관람 WebRTC 중계: 서버가 아는 필드만으로 다시 구성한다(viewerRelay.buildViewerSignal). from은 서버의 clientId */
  | { type: 'offer'; from: string; to: string; offer: SessionDescription }
  | { type: 'answer'; from: string; to: string; answer: SessionDescription }
  | { type: 'ice-candidate'; from: string; to: string; candidate: IceCandidate }
  | { type: 'native-descriptor'; from: string; descriptor: EngineDescriptor }
  | { type: 'native-leave'; from: string }
  | { type: 'relay-token'; token: string; bindKey: string; addr: string; ssrc: number; expiresAt: number; expiresIn: number; serverNow: number }
  | { type: 'metronome'; from: string; state: MetronomeState }
  | { type: 'request-perform-error'; code: 'login-required'; message: string }
  | { type: 'perform-request-sent'; instrument: string; hostOnline: boolean }
  | { type: 'perform-request-received'; request: PerformRequest }
  | { type: 'perform-request-approved'; instrument: string }
  | { type: 'perform-request-rejected'; reason: string; instrument: string }
  | { type: 'perform-request-cancelled'; oderId: string }
  | { type: 'instrument-change-request-sent'; newInstrument: string }
  | { type: 'instrument-change-request-received'; request: InstrumentChangeRequest }
  | { type: 'instrument-change-approved'; newInstrument: string }
  | { type: 'instrument-change-rejected'; reason: string }
  | { type: 'instrument-change-request-cancelled'; oderId: string }
  | { type: 'participant-instrument-changed'; oderId: string; instrument: string }
  /** 방장이 나를 관람자로 내렸다 (P2-9): 엔진 연주를 멈추고 관람 모드로 */
  | { type: 'demoted-to-viewer' }
