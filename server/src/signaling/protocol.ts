/**
 * 시그널링 계약의 TS 표현 (설계: docs/overhaul/native-engine-design.md 5절, docs/server-architecture.md 2.1절).
 * 상수·수신 메시지 타입·메시지 페이로드 타입과, 아래쪽에 송신 메시지(서버 → 클라이언트) 타입.
 * 이 파일은 다른 모듈을 import하지 않는다(실행 코드 없이 타입만 가져다 쓸 수 있게: e2e mock이 import type 한다).
 */

export const MAX_MESSAGE_BYTES = 64 * 1024
export const AUTH_TIMEOUT_MS = 5000
export const MAX_PENDING_MESSAGES = 100
export const MAX_ROOM_ID_LENGTH = 128
export const MAX_NICKNAME_LENGTH = 50
export const MAX_CHAT_LENGTH = 1000
export const ROOM_META_TTL_MS = 30_000
export const REJOIN_WINDOW_MS = 60_000
export const APPROVAL_GRACE_MS = 60_000
export const TOKEN_EXPIRY_GRACE_MS = 60_000
export const PARTICIPANT_UPDATE_DEBOUNCE_MS = 1000

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
// (S3 디스패치 정리 때). 지금은 e2e mock(client/e2e/mocks/signaling.ts)이 이 타입으로 맞춰 mock이 서버와 어긋나면 e2e tsc가 실패한다.

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
  | ErrorOf<'password-rate-limited' | 'password-busy', { roomId: string; retryAfterMs: number }>
  | ErrorOf<'room-full', { reason: 'room-full' | 'anonymous-limit'; roomId: string; maxParticipants: number }>
  | ErrorOf<'login-required' | 'perform-not-approved' | 'room-full-performers' | 'descriptor-invalid' | 'key-conflict' | 'ssrc-conflict' | 'metronome-invalid' | 'metronome-host-only'>

/** 방 안 브로드캐스트(broadcastToRoom)가 붙이는 보낸 사람 표시. 둘 다 보낸 사람의 clientId */
type FromSender = { oderId: string; from: string }

export type ServerMessage =
  | { type: 'welcome'; clientId: string; resumed?: true }
  | { type: 'auth-result'; authenticated: boolean; userId: string | null; resumed?: true }
  /** 앱 수준 ping에 대한 응답 (P1-15) */
  | { type: 'pong' }
  | ServerError
  | { type: 'room-state'; participants: Participant[]; peerIds: string[]; isHost: boolean }
  | ({ type: 'participant-joined'; participant: Participant; rejoin: boolean } & FromSender)
  | { type: 'participant-left'; oderId: string }
  | ({ type: 'performer-updated'; nickname: string; instrument?: string; isPerforming: boolean; isHost?: boolean } & FromSender)
  | ({ type: 'chat'; message: string; nickname: string; timestamp: number } & FromSender)
  /** 관람 WebRTC 중계: 클라이언트 메시지를 그대로 넘기고 from만 서버가 덮어쓴다 */
  | { type: 'offer' | 'answer' | 'ice-candidate'; from: string; to: string; [field: string]: unknown }
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
