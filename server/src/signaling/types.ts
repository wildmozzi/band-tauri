import type { WebSocket } from 'ws'
import { ROOM_CAPACITY_MAX } from '../lib/roomCapacity'
import { DEFAULT_PASSWORD_ATTEMPT_LIMITS, type ScryptGate } from '../lib/roomPassword'
import type { RoomMeta } from '../repositories/roomRepository'
import type { TokenBucket } from './rateLimit'

export type Env = Record<string, string | undefined>
export type AuthState = 'pending' | 'verifying' | 'authenticated' | 'anonymous'

/** 외부 의존성 (테스트에서 교체 가능) */
export interface WsDeps {
  /** Supabase 등 인증/DB 백엔드가 설정되어 있는지 */
  hasBackend: boolean
  verifyToken: (token: string) => Promise<string | null>
  /** 방 정보. 방이 없으면 null, 조회 자체가 실패하면 throw(호출부가 "방 없음"과 구분해 판단을 보류할 수 있게, PR #53 H1) */
  fetchRoomMeta: (roomId: string) => Promise<RoomMeta | null>
  updateParticipants: (roomId: string, count: number) => Promise<void>
}

export interface WsLimits {
  /** 일반 메시지 토큰 버킷 */
  msgBurst: number
  msgPerSec: number
  /** chat 토큰 버킷 */
  chatBurst: number
  chatPerSec: number
  /** join/leave 토큰 버킷 (DB 쓰기 증폭 방지) */
  joinBurst: number
  joinPerSec: number
  /** IP당 동시 연결 수 */
  maxConnectionsPerIp: number
  /** 이만큼 메시지를 버리면 연결 종료 */
  maxDropped: number
  heartbeatMs: number
  /** 방 하나의 최대 연주자 수 (설계: 최대 6명) */
  maxPerformers: number
  /** 방 하나의 최대 인원(관람 포함) 서버 상한. rooms.max_participants가 없거나 더 크면 이 값 (방 생성 스키마 최대 20) */
  maxRoomParticipants: number
  /** 방 하나에 들어올 수 있는 익명(로그인 안 한) 관람자 수 상한 (정원과 별도, PR #23 H1) */
  maxAnonymousPerRoom: number
  /** 같은 IP(/64)에서 한 방에 들어올 수 있는 익명 연결 수 (PR #23 H1) */
  maxAnonymousPerIpPerRoom: number
  /** 인증된 사용자가 끊긴 뒤 세션을 재개할 수 있는 유예시간 (이 동안 방 참가자에게 leave를 보내지 않음) */
  resumeGraceMs: number
  /** 방 비밀번호: (방, 사용자) 허용 실패 횟수 */
  passwordMaxFailures: number
  /** 방 비밀번호: (방, IP) 허용 실패 횟수 */
  passwordMaxFailuresPerIp: number
  /** 방 비밀번호: (방 전체) 허용 실패 횟수 */
  passwordMaxFailuresPerRoom: number
  /** 방 비밀번호 실패 기록 유지 시간 */
  passwordWindowMs: number
  /** 메트로놈 상태를 다시 보내기 전 방장(DB host_id) 확인에 쓰는 방 정보 캐시 최대 나이 (PR #36 L1) */
  metronomeHostCheckMs: number
}

export const DEFAULT_LIMITS: WsLimits = {
  msgBurst: 40,
  msgPerSec: 20,
  chatBurst: 5,
  chatPerSec: 2,
  joinBurst: 5,
  joinPerSec: 0.2,
  maxConnectionsPerIp: 20,
  maxDropped: 200,
  maxPerformers: 6,
  maxRoomParticipants: ROOM_CAPACITY_MAX,
  maxAnonymousPerRoom: 10,
  maxAnonymousPerIpPerRoom: 2,
  resumeGraceMs: 30_000,
  heartbeatMs: 30_000,
  passwordMaxFailures: DEFAULT_PASSWORD_ATTEMPT_LIMITS.maxFailures,
  passwordMaxFailuresPerIp: DEFAULT_PASSWORD_ATTEMPT_LIMITS.maxFailuresPerIp,
  passwordMaxFailuresPerRoom: DEFAULT_PASSWORD_ATTEMPT_LIMITS.maxFailuresPerRoom,
  passwordWindowMs: DEFAULT_PASSWORD_ATTEMPT_LIMITS.windowMs,
  metronomeHostCheckMs: 2_000,
}

export interface WsOptions {
  /** scrypt 동시 실행 게이트 (테스트 주입용, 기본: 프로세스 공용) */
  scryptGate?: ScryptGate
  deps?: Partial<WsDeps>
  limits?: Partial<WsLimits>
  env?: Env
}

export interface Client {
  id: string
  ws: WebSocket
  ip: string
  roomId?: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
  /** 서버가 검증한 userId (익명이면 undefined) */
  userId?: string
  authState: AuthState
  /** 토큰 만료 시각(ms). 알 수 없으면 null */
  tokenExp: number | null
  expiryTimer?: ReturnType<typeof setTimeout>
  /** native-descriptor를 보낸 적 있음 (native-leave 허용 판단용) */
  nativeActive: boolean
  /** 마지막 native-descriptor의 ssrc (릴레이 토큰 발급용) */
  nativeSsrc?: number
  /** 마지막 native-descriptor의 엔진 공개키 (같은 방 안에서 이 연결·userId에 묶임, P1-1c) */
  nativeKey?: string
  /** 메시지 순차 처리 체인 (비동기 auth/DB 조회 중 순서 보장) */
  chain: Promise<void>
  pending: number
  isAlive: boolean
  msgBucket: TokenBucket
  chatBucket: TokenBucket
  joinBucket: TokenBucket
  dropped: number
  lastRateLimitNotice: number
  /** 끊긴 뒤 세션 재개 대기 중이면 유예 만료 타이머 */
  suspendTimer?: ReturnType<typeof setTimeout>
  /** 재개 유예 중에 이양으로 내 메트로놈이 꺼졌다: 재개 때 metronome-host-only를 보낸다 (PR #53 L2) */
  pendingMetronomeStop?: boolean
}

export type LeaveReason = 'leave' | 'move' | 'close' | 'duplicate'
