import { WebSocketServer, WebSocket, type RawData } from 'ws'
import type { IncomingMessage } from 'http'
import { Server as HttpServer } from 'http'
import { Server as HttpsServer } from 'https'
import { v4 as uuidv4 } from 'uuid'
import { SupabaseClient } from '@supabase/supabase-js'
import { isInsecureDevMode } from '../lib/securityConfig'
import { ROOM_CAPACITY_MAX } from '../lib/roomCapacity'
import { deriveRelayBindKey, isValidSsrc, relayConfigFromEnv, signRelayToken } from './relayToken'
import { createRoomRepository, type RoomMeta, type RoomRepository } from '../repositories/roomRepository'
import { DEFAULT_PASSWORD_ATTEMPT_LIMITS, PasswordAttemptLimiter, ScryptBusyError, defaultScryptGate, ipBucket, verifyRoomPassword, type ScryptGate } from '../lib/roomPassword'

/**
 * 시그널링 WebSocket 서버 (설계: docs/overhaul/native-engine-design.md 5절)
 *
 * 인증
 * - 연결 직후 클라이언트는 `{ type: 'auth', accessToken }`을 보낸다. 서버가 Supabase로 검증해 userId를 결정한다.
 * - accessToken이 없거나 검증 실패 → 익명 관람자(연주 요청/연주 시작/네이티브 시그널링 불가).
 * - auth 전에 온 다른 메시지는 거부한다. AUTH_TIMEOUT_MS 안에 auth가 없으면 익명으로 진행한다(관람자 호환).
 * - 같은 사용자의 갱신 토큰으로 재인증할 수 있다. 토큰 만료 후 유예시간까지 재인증이 없으면 4001로 끊는다.
 *
 * 권한
 * - isHost는 DB `rooms.host_id === userId`로 서버가 판정한다(방 정보는 짧게 캐시, 승인 계열 처리 때 재확인).
 * - start-performing은 방장, free_join 방, 또는 방장이 승인한 사용자만 가능하다.
 * - 클라이언트가 보낸 isHost/userId/from/isRejoin은 신뢰하지 않는다. 중계 시 서버가 `from`을 붙인다.
 *
 * 방 비밀번호 (P2-6)
 * - room_secrets에 해시가 있는 방은 join에 `password`가 필요하다. 방장은 비밀번호 없이 입장한다.
 * - 실패는 (방, 사용자) 5회 · (방, IPv4 또는 IPv6 /64) 20회 · (방 전체) 100회 / 15분으로 제한(`password-rate-limited`).
 *   이미 그 방에 있는 연결(세션 재개)은 다시 묻지 않는다.
 * - scrypt는 프로세스 공용 게이트로 동시 2개까지만 돌린다. 대기열이 차면 검증하지 않고 `password-busy`(시도 횟수 미차감).
 * - rooms.has_password=true인데 해시가 없으면(생성/변경 중) 입장을 막는다(fail-closed).
 * - 비공개 방은 목록(RLS)에서만 숨기고, 방 id(초대 링크/방 코드)를 아는 사람은 입장할 수 있다.
 *
 * UDP 릴레이 (docs/overhaul/relay-design.md)
 * - RELAY_SECRET/RELAY_ADDR가 설정되면, 연주자가 native-descriptor(ssrc 포함)를 보낼 때 `relay-token`을 발급한다.
 * - `relay-token-request`로 갱신. 토큰 내용(roomId, memberId=userId, ssrc)은 서버가 정한다.
 * - 같은 방에서 다른 사용자가 이미 쓰는 ssrc의 descriptor는 거부한다(`ssrc-conflict`).
 *
 * 엔진 공개키 바인딩 (P1-1c)
 * - descriptor의 `key`(X25519 공개키, 표준 base64 32바이트)는 보낸 연결(clientId)과 JWT userId에 묶인다.
 *   형식이 틀리면(키, ssrc 정수 범위, candidates 개수·원소 형식) `descriptor-invalid`, 같은 방의 다른 userId가 이미 쓰는 키면 `key-conflict`로 거부하고 중계하지 않는다.
 * - 바인딩은 Client.nativeKey 하나뿐이다(연결당 키 1개 → 메모리 상한 = 연결 수 상한). 같은 사용자가 새 키를 보내면
 *   (엔진 재시작) 덮어쓰고, native-leave·퇴장·방 이동·연결 최종 종료(유예 만료, 중복 연결 정리) 때 사라진다.
 *   세션 재개 유예 중에는 바인딩을 유지한다(그 사이 다른 사람이 키를 가로채지 못하게).
 * - 중계하는 descriptor는 서버가 아는 필드(ssrc, key, candidates 원소의 {kind, addr}까지)만 검증해 다시 구성하고 `from`은 서버가 붙인다.
 * - 한계: 서버 인스턴스 1대 가정(P3-10g 정원과 같음). 여러 대로 늘리면 방 단위 상태(바인딩·정원·승인)를 공유 저장소로 옮겨야 한다.
 */

const MAX_MESSAGE_BYTES = 64 * 1024
const AUTH_TIMEOUT_MS = 5000
const MAX_PENDING_MESSAGES = 100
const MAX_ROOM_ID_LENGTH = 128
const MAX_NICKNAME_LENGTH = 50
const MAX_CHAT_LENGTH = 1000
const ROOM_META_TTL_MS = 30_000
const REJOIN_WINDOW_MS = 60_000
const APPROVAL_GRACE_MS = 60_000
const TOKEN_EXPIRY_GRACE_MS = 60_000
const PARTICIPANT_UPDATE_DEBOUNCE_MS = 1000
/** X25519 공개키 길이(바이트). descriptor.key는 이것의 표준 base64(패딩 포함 44자) */
const ENGINE_KEY_BYTES = 32
const ENGINE_KEY_B64_RE = /^[A-Za-z0-9+/]{43}=$/
/** descriptor 후보 개수 상한(엔진 net/peer.rs MAX_CANDIDATES와 같음) */
export const MAX_ENGINE_CANDIDATES = 8
/** 후보 주소 형식: IPv4 `a.b.c.d:port`만(엔진 소켓은 IPv4, 받는 쪽 엔진도 IPv6를 거부한다) */
const CANDIDATE_ADDR_RE = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3}):(\d{1,5})$/
const CANDIDATE_ADDR_MAX = 21 // '255.255.255.255:65535'

type Env = Record<string, string | undefined>
type AuthState = 'pending' | 'verifying' | 'authenticated' | 'anonymous'

export type { RoomMeta }

/** 외부 의존성 (테스트에서 교체 가능) */
export interface WsDeps {
  /** Supabase 등 인증/DB 백엔드가 설정되어 있는지 */
  hasBackend: boolean
  verifyToken: (token: string) => Promise<string | null>
  /** 방 정보. 방이 없으면 null */
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

const DEFAULT_LIMITS: WsLimits = {
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

class TokenBucket {
  private tokens: number
  private last = Date.now()
  constructor(private capacity: number, private perSec: number) {
    this.tokens = capacity
  }
  take(): boolean {
    const now = Date.now()
    this.tokens = Math.min(this.capacity, this.tokens + ((now - this.last) / 1000) * this.perSec)
    this.last = now
    if (this.tokens < 1) return false
    this.tokens -= 1
    return true
  }
}

interface Client {
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
}

interface Participant {
  oderId: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
}

interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

type Msg = Record<string, unknown> & { type: string }

/** 공유 메트로놈(P2-3) 계획. startUs는 방장 엔진 시계(µs) 기준 박 0 시각 */
export interface MetronomeState {
  running: boolean
  bpm: number
  beatsPerBar: number
  startUs: number
}

/** 클라이언트가 보낸 메트로놈 계획 검증(엔진 계약과 같은 범위: bpm 20..300, 박자 1..16) */
export function parseMetronomeState(v: unknown): MetronomeState | null {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null
  const { running, bpm, beatsPerBar, startUs } = v as Record<string, unknown>
  if (typeof running !== 'boolean') return null
  if (typeof bpm !== 'number' || !Number.isFinite(bpm) || bpm < 20 || bpm > 300) return null
  if (typeof beatsPerBar !== 'number' || !Number.isInteger(beatsPerBar) || beatsPerBar < 1 || beatsPerBar > 16) return null
  if (typeof startUs !== 'number' || !Number.isFinite(startUs) || startUs < 0 || startUs > Number.MAX_SAFE_INTEGER) return null
  return { running, bpm, beatsPerBar, startUs }
}
type LeaveReason = 'leave' | 'move' | 'close' | 'duplicate'

/**
 * descriptor.key 검증: 표준 base64(패딩 포함), 디코딩하면 정확히 32바이트(X25519 공개키).
 * 엔진(net/peer.rs)은 STANDARD로 인코딩한다. 비정규 표기(마지막 문자의 남는 비트)로 같은 키를 다른 문자열로
 * 보내 바인딩을 우회하지 못하게, 다시 인코딩한 값과 같아야 통과한다. 통과하면 키 문자열, 아니면 null.
 */
export function parseEngineKey(v: unknown): string | null {
  if (typeof v !== 'string' || !ENGINE_KEY_B64_RE.test(v)) return null
  const raw = Buffer.from(v, 'base64')
  if (raw.length !== ENGINE_KEY_BYTES || raw.toString('base64') !== v) return null
  return v
}

export interface EngineCandidate { kind: 'lan' | 'public'; addr: string }

/**
 * descriptor.candidates 검증과 재구성(PR #48 M1): 배열, 최대 MAX_ENGINE_CANDIDATES개, 각 원소는
 * `{ kind: 'lan'|'public', addr: 'IPv4:port' }`. 원소의 다른 필드는 버리고 아는 필드만 새로 만든다.
 * 옥텟 0..255, 포트 1..65535, 선행 0 표기는 거부(같은 주소를 다른 문자열로 보내지 못하게).
 * 0.0.0.0·멀티캐스트·루프백 같은 의미 검사는 받는 쪽 엔진(validate_candidate)이 한다(루프백은 테스트 옵션으로 허용될 수 있음).
 * 하나라도 틀리면 null.
 */
export function parseCandidates(v: unknown): EngineCandidate[] | null {
  if (!Array.isArray(v) || v.length > MAX_ENGINE_CANDIDATES) return null
  const out: EngineCandidate[] = []
  for (const item of v) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) return null
    const { kind, addr } = item as Record<string, unknown>
    if (kind !== 'lan' && kind !== 'public') return null
    if (typeof addr !== 'string' || addr.length > CANDIDATE_ADDR_MAX) return null
    const m = CANDIDATE_ADDR_RE.exec(addr)
    if (!m) return null
    const parts = m.slice(1)
    if (parts.some(p => p.length > 1 && p.startsWith('0'))) return null
    const nums = parts.map(Number)
    if (nums.slice(0, 4).some(n => n > 255)) return null
    if (nums[4] < 1 || nums[4] > 65535) return null
    out.push({ kind, addr })
  }
  return out
}

function send(ws: WebSocket, data: unknown) {
  if (ws.readyState === ws.OPEN) {
    ws.send(JSON.stringify(data))
  }
}

function isNonEmptyString(v: unknown, max = 256): v is string {
  return typeof v === 'string' && v.length > 0 && v.length <= max
}

function sanitizeNickname(v: unknown): string | undefined {
  if (typeof v !== 'string') return undefined
  const trimmed = v.trim().slice(0, MAX_NICKNAME_LENGTH)
  return trimmed || undefined
}

/** JWT의 exp(초)를 ms로. 검증은 하지 않는다(verifyToken 통과 후에만 사용) */
export function jwtExpiryMs(token: string): number | null {
  const parts = token.split('.')
  if (parts.length !== 3) return null
  try {
    const payload = JSON.parse(Buffer.from(parts[1].replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8'))
    return typeof payload.exp === 'number' ? payload.exp * 1000 : null
  } catch {
    return null
  }
}

function clientIp(req: IncomingMessage, env: Env): string {
  // fly.io 프록시 뒤에서는 Fly-Client-IP가 실제 클라이언트 IP (fly 밖에서는 위조 가능하므로 사용 안 함)
  if (env.FLY_APP_NAME) {
    const h = req.headers['fly-client-ip']
    const v = Array.isArray(h) ? h[0] : h
    if (v) return v
  }
  return req.socket.remoteAddress || 'unknown'
}

function supabaseDeps(supabase: SupabaseClient | null): WsDeps {
  return {
    hasBackend: !!supabase,
    verifyToken: async (token) => {
      if (!supabase) return null
      try {
        const { data, error } = await supabase.auth.getUser(token)
        return !error && data.user ? data.user.id : null
      } catch (err) {
        console.error('[AUTH] getUser failed:', err)
        return null
      }
    },
    ...roomDeps(supabase ? createRoomRepository(supabase) : null),
  }
}

/** rooms 테이블 접근 (repository). 오류는 로그 후 "방 없음"/무시로 처리 — 기존 동작 */
function roomDeps(rooms: RoomRepository | null): Pick<WsDeps, 'fetchRoomMeta' | 'updateParticipants'> {
  return {
    fetchRoomMeta: async (roomId) => {
      if (!rooms) return null
      try {
        return await rooms.findMeta(roomId)
      } catch (err) {
        console.error('[DB] Failed to look up room:', err instanceof Error ? err.message : err)
        return null
      }
    },
    updateParticipants: async (roomId, count) => {
      if (!rooms) return
      try {
        await rooms.updateParticipantCount(roomId, count)
      } catch (err) {
        console.error('[DB] Failed to update participants:', err instanceof Error ? err.message : err)
      }
    },
  }
}

export function initializeWebSocket(
  server: HttpServer | HttpsServer,
  supabase: SupabaseClient | null,
  options: WsOptions = {},
): WebSocketServer {
  const env = options.env ?? process.env
  const deps: WsDeps = { ...supabaseDeps(supabase), ...options.deps }
  const limits: WsLimits = { ...DEFAULT_LIMITS, ...options.limits }
  const scryptGate = options.scryptGate ?? defaultScryptGate
  const relayConfig = relayConfigFromEnv(env)

  // 서버 인스턴스별 상태
  const rooms = new Map<string, Set<string>>()
  const clients = new Map<string, Client>()
  const performRequests = new Map<string, PerformRequest[]>()
  const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()
  /**
   * roomId → 방장이 켠 공유 메트로놈(P2-3). hostId = 켠 연결(clientId = 엔진 peerId), hostUserId = 켤 때의 DB 방장.
   * 방장 엔진이 내려가면(native-leave·퇴장) 지우고, DB 방장이 바뀐 것을 알게 되면 정지를 알리고 지운다(PR #36 L1)
   */
  const metronomes = new Map<string, { hostId: string; hostUserId: string | undefined; state: MetronomeState }>()
  /** roomId → (userId → 만료 시각 | null=무기한). 방장 승인 기록 (H1) */
  const approvals = new Map<string, Map<string, number | null>>()
  /** roomId → (userId → 만료 시각). 연결이 끊긴 사용자 (서버 판정 rejoin, M1) */
  const recentlyLeft = new Map<string, Map<string, number>>()
  /** roomId → (userId → 만료 시각). 유예 밖으로 끊긴 연주자·승인 보유자의 자리 예약: 이 동안 재입장은 정원 검사를 건너뛴다 (PR #23 M2) */
  const seatHolds = new Map<string, Map<string, number>>()
  const roomMetaCache = new Map<string, { meta: RoomMeta | null; fetchedAt: number }>()
  const participantTimers = new Map<string, ReturnType<typeof setTimeout>>()
  const connectionsPerIp = new Map<string, number>()
  /** clientId → 연결이 끊겨 재개를 기다리는 클라이언트 (방/연주 상태 유지) */
  const suspended = new Map<string, Client>()
  const passwordLimiter = new PasswordAttemptLimiter({
    maxFailures: limits.passwordMaxFailures,
    maxFailuresPerIp: limits.passwordMaxFailuresPerIp,
    maxFailuresPerRoom: limits.passwordMaxFailuresPerRoom,
    windowMs: limits.passwordWindowMs,
  })

  /** 유예 중인 클라이언트를 최종 제거 (방 참가자에게 leave 전송) */
  function finalizeSuspended(c: Client) {
    if (c.suspendTimer) clearTimeout(c.suspendTimer)
    c.suspendTimer = undefined
    suspended.delete(c.id)
    if (clients.get(c.id) !== c) return
    console.log(`[RESUME] ${c.id.slice(0, 8)} grace period over - leaving`)
    if (c.roomId && rooms.has(c.roomId)) handleLeave(c, c.roomId, 'close')
    c.roomId = undefined
    clients.delete(c.id)
  }

  // maxPayload 초과 시 ws가 연결을 1009로 끊는다
  const wss = new WebSocketServer({ server, maxPayload: MAX_MESSAGE_BYTES })

  const insecureDevMode = isInsecureDevMode(deps.hasBackend, env)
  if (insecureDevMode) {
    console.warn('[WS][SECURITY] INSECURE dev mode (NODE_ENV=development, ALLOW_INSECURE_WS=1, no Supabase): client-sent userId/isHost are trusted.')
  } else if (!deps.hasBackend) {
    console.warn('[WS][SECURITY] Supabase not configured - all clients are anonymous viewers.')
  }

  // ---------- helpers ----------

  /** 엔진 상태(descriptor 바인딩) 정리: native-leave·퇴장·방 이동 */
  function clearNative(c: Client) {
    c.nativeActive = false
    c.nativeSsrc = undefined
    c.nativeKey = undefined
  }

  const isAuthenticated = (c: Client) => c.authState === 'authenticated' && !!c.userId
  /** 연주자 = 연주 중이거나 방장 */
  const isPerformer = (c: Client) => c.isPerforming || c.isHost

  /**
   * 릴레이 토큰 발급 (연주자 전용). roomId/memberId/ssrc는 서버가 아는 값만 쓴다.
   * - memberId는 검증된 userId: 세션 재개 없이 재접속해도(새 clientId) 릴레이가 같은 멤버로 보고 이전 슬롯을 교체한다.
   * - 회수 수단이 없으므로 수명을 짧게(기본 120초) 두고 클라이언트가 갱신한다. 릴레이 등록은 토큰 만료를 넘지 않는다.
   * - expiresIn(남은 수명, 초)·serverNow를 함께 보낸다: 클라이언트는 자기 시계가 아니라 받은 시각 + expiresIn으로
   *   만료·갱신을 판단한다(시계가 어긋난 PC에서 토큰이 주기적으로 끊기지 않게, PR #10 M1).
   * - bindKey: 토큰별 Bind MAC 키(relay-design 2.1.1). TLS로만 전달되어 경로상 공격자의 등록 가로채기를 막는다.
   */
  function sendRelayToken(c: Client) {
    if (!relayConfig || !c.roomId || !c.userId || c.nativeSsrc === undefined || !isPerformer(c)) return
    const serverNow = Math.floor(Date.now() / 1000)
    const expiresAt = serverNow + relayConfig.ttlSec
    const token = signRelayToken({ roomId: c.roomId, memberId: c.userId, ssrc: c.nativeSsrc, expiresAt }, relayConfig.secret)
    send(c.ws, {
      type: 'relay-token',
      token: token.toString('base64url'),
      bindKey: deriveRelayBindKey(token, relayConfig.secret).toString('base64url'),
      addr: relayConfig.addr,
      ssrc: c.nativeSsrc,
      expiresAt,
      expiresIn: relayConfig.ttlSec,
      serverNow,
    })
  }

  /** 같은 방의 다른 사용자가 이 ssrc를 쓰고 있는지 (ssrc 선점·가로채기 방지) */
  function ssrcTakenInRoom(c: Client, ssrc: number): boolean {
    if (!c.roomId) return false
    for (const peerId of rooms.get(c.roomId) || []) {
      if (peerId === c.id) continue
      const peer = clients.get(peerId)
      if (peer && peer.nativeSsrc === ssrc && peer.userId !== c.userId) return true
    }
    return false
  }

  /**
   * 같은 방의 다른 사용자(JWT userId 기준)가 이 엔진 공개키를 쓰고 있는지 (P1-1c: 공개키 위장·선점 방지).
   * 세션 재개 유예 중인 연결도 방에 남아 있으므로 그 키는 계속 보호된다. 같은 userId(재접속·엔진 재시작)는 허용.
   */
  function keyTakenInRoom(c: Client, key: string): boolean {
    if (!c.roomId) return false
    for (const peerId of rooms.get(c.roomId) || []) {
      if (peerId === c.id) continue
      const peer = clients.get(peerId)
      if (peer && peer.nativeKey === key && peer.userId !== c.userId) return true
    }
    return false
  }

  function getRoomParticipants(roomId: string): Participant[] {
    const room = rooms.get(roomId)
    if (!room) return []
    const participants: Participant[] = []
    for (const oderId of room) {
      const c = clients.get(oderId)
      if (c) participants.push({ oderId: c.id, nickname: c.nickname, instrument: c.instrument, isPerforming: c.isPerforming, isHost: c.isHost })
    }
    return participants
  }

  function broadcastToRoom(roomId: string, senderId: string, data: object) {
    const room = rooms.get(roomId)
    if (!room) return
    for (const oderId of room) {
      if (oderId === senderId) continue
      const peer = clients.get(oderId)
      if (peer) send(peer.ws, { ...data, oderId: senderId, from: senderId })
    }
  }

  function findRoomHost(roomId: string): Client | null {
    for (const oderId of rooms.get(roomId) || []) {
      const c = clients.get(oderId)
      if (c?.isHost) return c
    }
    return null
  }

  /** 같은 방에 있는 대상만 반환 (방 밖 중계 차단) */
  function getRoomPeer(sender: Client, targetId: unknown): Client | null {
    if (!sender.roomId || typeof targetId !== 'string' || targetId === sender.id) return null
    const target = clients.get(targetId)
    if (!target || target.roomId !== sender.roomId) return null
    return target
  }

  /**
   * 방 정원 계산용 자리 수: 방 멤버(유예 중 세션 포함)에서 자기 자신과 같은 userId의 기존 연결(입장 시 대체됨)은 뺀다.
   * members = 로그인 사용자(userId 있음), anonymous = 익명 관람자, anonymousFromIp = 그중 같은 IP(/64)에서 온 연결.
   */
  function countRoomSeats(roomId: string, self: Client): { total: number; members: number; anonymous: number; anonymousFromIp: number } {
    const bucket = ipBucket(self.ip)
    const seats = { total: 0, members: 0, anonymous: 0, anonymousFromIp: 0 }
    for (const pid of rooms.get(roomId) || []) {
      if (pid === self.id) continue
      const c = clients.get(pid)
      if (self.userId && c?.userId === self.userId) continue
      seats.total++
      if (c?.userId) seats.members++
      else {
        seats.anonymous++
        if (c && ipBucket(c.ip) === bucket) seats.anonymousFromIp++
      }
    }
    return seats
  }

  /**
   * 정원 검사 (P3-12b, PR #23 H1). 통과하면 null, 아니면 거부 사유.
   * - 로그인 사용자는 로그인 사용자 수로만 정원을 센다: 익명 관람자가 자리를 채워도 밴드 멤버·연주자는 막히지 않는다.
   * - 익명 관람자는 빈자리가 있을 때만(전체 < 정원), 방별 익명 상한·IP당 방별 익명 상한 안에서 들어온다.
   *   이미 있는 익명을 밀어내지 않으므로, 로그인 사용자가 뒤에 들어오면 전체 인원이 잠시 정원을 넘을 수 있다.
   */
  function checkCapacity(roomId: string, self: Client, capacity: number): 'room-full' | 'anonymous-limit' | null {
    const seats = countRoomSeats(roomId, self)
    if (self.userId) return seats.members >= capacity ? 'room-full' : null
    if (seats.total >= capacity) return 'room-full'
    if (seats.anonymous >= limits.maxAnonymousPerRoom || seats.anonymousFromIp >= limits.maxAnonymousPerIpPerRoom) return 'anonymous-limit'
    return null
  }

  /** 자리 예약(유예 밖 재입장 우선권)이 살아 있는지. 확인만 하고 지우지 않는다 */
  function hasSeatHold(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const m = seatHolds.get(roomId)
    const exp = m?.get(userId)
    if (exp === undefined) return false
    if (exp > Date.now()) return true
    clearSeatHold(roomId, userId)
    return false
  }

  function clearSeatHold(roomId: string, userId: string | undefined) {
    if (!userId) return
    const m = seatHolds.get(roomId)
    m?.delete(userId)
    if (m && m.size === 0) seatHolds.delete(roomId)
  }

  async function getRoomMeta(roomId: string, maxAgeMs = ROOM_META_TTL_MS): Promise<RoomMeta | null> {
    const cached = roomMetaCache.get(roomId)
    if (cached && Date.now() - cached.fetchedAt <= maxAgeMs) return cached.meta
    const meta = await deps.fetchRoomMeta(roomId)
    roomMetaCache.set(roomId, { meta, fetchedAt: Date.now() })
    return meta
  }

  /** 방장 여부 재확인 (L2). 개발 모드에서는 join 때 값 유지 */
  async function refreshHost(client: Client): Promise<boolean> {
    if (!client.roomId || insecureDevMode) return client.isHost
    if (!client.userId) {
      client.isHost = false
      return false
    }
    const meta = await getRoomMeta(client.roomId)
    client.isHost = !!meta && meta.hostId === client.userId
    return client.isHost
  }

  /** 메트로놈 정지를 같은 방의 다른 연주자에게 알린다(켠 방장 제외) */
  function broadcastMetronomeStop(roomId: string, metro: { hostId: string; state: MetronomeState }) {
    const stopped = { type: 'metronome', from: metro.hostId, state: { ...metro.state, running: false } }
    for (const peerId of rooms.get(roomId) || []) {
      if (peerId === metro.hostId) continue
      const peer = clients.get(peerId)
      if (peer && isPerformer(peer)) send(peer.ws, stopped)
    }
  }

  /**
   * 방장 이양(PR #36 L1): 지금 DB 방장(`currentHostUserId`, 방이 없으면 null)이 메트로놈을 켠 사용자가 아니면
   * 정지를 알리고 상태를 지운다. 옛 방장에게는 metronome-host-only를 보내 그쪽 기준 클릭도 끄게 한다.
   * 조작 권한은 따로 넘길 것이 없다: metronome 메시지마다 DB로 방장을 다시 확인하므로 새 방장은 바로 켤 수 있다.
   * 지웠으면 true.
   */
  function dropMetronomeIfHostChanged(roomId: string, currentHostUserId: string | null): boolean {
    const metro = metronomes.get(roomId)
    if (!metro || metro.hostUserId === undefined || metro.hostUserId === currentHostUserId) return false
    console.log(`[METRONOME] host changed in room ${roomId.slice(0, 8)} - stopping`)
    metronomes.delete(roomId)
    broadcastMetronomeStop(roomId, metro)
    const oldHost = clients.get(metro.hostId)
    if (oldHost && oldHost.roomId === roomId) {
      send(oldHost.ws, { type: 'error', code: 'metronome-host-only', message: '방장이 바뀌어 메트로놈을 껐습니다.' })
    }
    return true
  }

  /**
   * 엔진을 (다시) 켠 연주자에게 방장이 켠 메트로놈을 다시 보낸다(늦게 들어온 연주자, 엔진 재시작).
   * 보내기 전에 켠 사람이 아직 DB 방장인지 확인한다(이양 뒤 옛 방장 계획을 보내지 않게, PR #36 L1).
   * DB 조회가 실패하면 판단을 미루고 그대로 보낸다(일시 장애로 합주를 끊지 않음).
   */
  async function replayMetronome(c: Client, roomId: string) {
    const metro = metronomes.get(roomId)
    if (!metro || metro.hostId === c.id || clients.get(metro.hostId)?.roomId !== roomId) return
    if (deps.hasBackend && metro.hostUserId !== undefined) {
      let meta: RoomMeta | null | undefined
      try {
        meta = await getRoomMeta(roomId, limits.metronomeHostCheckMs)
      } catch (err) {
        console.warn('[METRONOME] host check failed:', err instanceof Error ? err.message : err)
      }
      if (meta !== undefined && dropMetronomeIfHostChanged(roomId, meta?.hostId ?? null)) return
    }
    // DB 조회 동안 상태가 바뀌었을 수 있다
    const now = metronomes.get(roomId)
    if (!now || now.hostId === c.id || clients.get(c.id) !== c || c.roomId !== roomId || !c.nativeActive) return
    if (clients.get(now.hostId)?.roomId !== roomId) return
    send(c.ws, { type: 'metronome', from: now.hostId, state: now.state })
  }

  /** 참가자 수 DB 갱신 디바운스 (M3) */
  function scheduleParticipantUpdate(roomId: string) {
    if (!deps.hasBackend || participantTimers.has(roomId)) return
    const t = setTimeout(() => {
      participantTimers.delete(roomId)
      void deps.updateParticipants(roomId, rooms.get(roomId)?.size ?? 0)
    }, PARTICIPANT_UPDATE_DEBOUNCE_MS)
    t.unref?.()
    participantTimers.set(roomId, t)
  }

  function hasApproval(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const exp = approvals.get(roomId)?.get(userId)
    if (exp === undefined) return false
    if (exp === null || exp > Date.now()) return true
    approvals.get(roomId)?.delete(userId)
    return false
  }

  function setApproval(roomId: string, userId: string, expiresAt: number | null) {
    if (!approvals.has(roomId)) approvals.set(roomId, new Map())
    approvals.get(roomId)!.set(userId, expiresAt)
  }

  function clearApproval(roomId: string, userId: string | undefined) {
    if (!userId) return
    const m = approvals.get(roomId)
    m?.delete(userId)
    if (m && m.size === 0) approvals.delete(roomId)
  }

  function handleLeave(client: Client, roomId: string, reason: LeaveReason) {
    const id = client.id
    const set = rooms.get(roomId)
    if (!set) return

    set.delete(id)
    console.log(`[LEAVE] Client ${id.slice(0, 8)} left room (${reason}). Room size: ${set.size}`)
    scheduleParticipantUpdate(roomId)

    // 승인 기록: 명시적 퇴장/방 이동은 즉시 삭제, 연결 끊김은 재접속 유예 후 만료, 중복 연결 정리는 유지
    if (client.userId) {
      if (reason === 'leave' || reason === 'move') {
        clearApproval(roomId, client.userId)
        clearSeatHold(roomId, client.userId)
      } else if (reason === 'close' && approvals.get(roomId)?.has(client.userId)) {
        setApproval(roomId, client.userId, Date.now() + APPROVAL_GRACE_MS)
      }
      // 연주 중이었거나 승인을 가진 사용자가 끊기면 승인 유예와 같은 시간 동안 자리를 예약한다 (PR #23 M2)
      if (reason === 'close' && (client.isPerforming || approvals.get(roomId)?.has(client.userId))) {
        if (!seatHolds.has(roomId)) seatHolds.set(roomId, new Map())
        seatHolds.get(roomId)!.set(client.userId, Date.now() + APPROVAL_GRACE_MS)
      }
      if (reason === 'close') {
        if (!recentlyLeft.has(roomId)) recentlyLeft.set(roomId, new Map())
        recentlyLeft.get(roomId)!.set(client.userId, Date.now() + REJOIN_WINDOW_MS)
      }
    }

    const host = findRoomHost(roomId)

    const requests = performRequests.get(roomId)
    if (requests) {
      const idx = requests.findIndex(r => r.oderId === id)
      if (idx !== -1) {
        requests.splice(idx, 1)
        if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
      }
    }

    const changeRequests = instrumentChangeRequests.get(roomId)
    if (changeRequests) {
      const idx = changeRequests.findIndex(r => r.oderId === id)
      if (idx !== -1) {
        changeRequests.splice(idx, 1)
        if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
      }
    }

    for (const oderId of set) {
      const peer = clients.get(oderId)
      if (peer) send(peer.ws, { type: 'participant-left', oderId: id })
    }
    // 방장이 나가면 메트로놈 시각 기준(방장 엔진 시계)이 사라진다. 따르는 쪽은 participant-left로 스스로 멈춘다
    if (metronomes.get(roomId)?.hostId === id) metronomes.delete(roomId)

    if (set.size === 0) {
      rooms.delete(roomId)
      performRequests.delete(roomId)
      metronomes.delete(roomId)
      instrumentChangeRequests.delete(roomId)
      roomMetaCache.delete(roomId)
    }
  }

  function consumeRejoin(roomId: string, userId: string | undefined): boolean {
    if (!userId) return false
    const m = recentlyLeft.get(roomId)
    const exp = m?.get(userId)
    if (exp === undefined) return false
    m!.delete(userId)
    if (m!.size === 0) recentlyLeft.delete(roomId)
    return exp > Date.now()
  }

  function scheduleExpiryCheck(client: Client) {
    if (client.expiryTimer) clearTimeout(client.expiryTimer)
    client.expiryTimer = undefined
    if (client.tokenExp === null) return
    const delay = Math.min(Math.max(client.tokenExp + TOKEN_EXPIRY_GRACE_MS - Date.now(), 0), 2 ** 31 - 1)
    client.expiryTimer = setTimeout(() => {
      if (client.tokenExp !== null && client.tokenExp + TOKEN_EXPIRY_GRACE_MS <= Date.now()) {
        console.log(`[AUTH] ${client.id.slice(0, 8)} token expired - closing`)
        client.ws.close(4001, 'Token expired')
      }
    }, delay)
    client.expiryTimer.unref?.()
  }

  // ---------- heartbeat (L4) ----------

  const heartbeat = setInterval(() => {
    const now = Date.now()
    for (const [roomId, holds] of seatHolds) {
      for (const [uid, exp] of holds) if (exp <= now) holds.delete(uid)
      if (holds.size === 0) seatHolds.delete(roomId)
    }
    for (const c of clients.values()) {
      if (suspended.has(c.id)) continue // 이미 끊긴 연결 (재개 대기)
      if (!c.isAlive) {
        console.log(`[WS] ${c.id.slice(0, 8)} heartbeat timeout - terminating`)
        c.ws.terminate()
        continue
      }
      c.isAlive = false
      try {
        c.ws.ping()
      } catch {
        // 무시
      }
    }
  }, limits.heartbeatMs)
  heartbeat.unref?.()

  wss.on('close', () => {
    clearInterval(heartbeat)
    for (const t of participantTimers.values()) clearTimeout(t)
    for (const c of suspended.values()) if (c.suspendTimer) clearTimeout(c.suspendTimer)
  })

  wss.on('error', (err) => {
    console.error('[WS] server error:', err.message)
  })

  wss.on('connection', (ws, req) => {
    const ip = clientIp(req, env)
    const ipCount = (connectionsPerIp.get(ip) || 0) + 1
    if (ipCount > limits.maxConnectionsPerIp) {
      console.warn(`[WS] too many connections from ${ip}`)
      ws.close(1008, 'Too many connections')
      return
    }
    connectionsPerIp.set(ip, ipCount)

    // 세션 재개 시 이전 Client로 교체되므로 let (모든 핸들러는 이 변수를 통해 현재 Client를 본다)
    let id = uuidv4()
    let client: Client = {
      id,
      ws,
      ip,
      nickname: `User${id.slice(0, 4)}`,
      isPerforming: false,
      isHost: false,
      authState: 'pending',
      tokenExp: null,
      nativeActive: false,
      chain: Promise.resolve(),
      pending: 0,
      isAlive: true,
      msgBucket: new TokenBucket(limits.msgBurst, limits.msgPerSec),
      chatBucket: new TokenBucket(limits.chatBurst, limits.chatPerSec),
      joinBucket: new TokenBucket(limits.joinBurst, limits.joinPerSec),
      dropped: 0,
      lastRateLimitNotice: 0,
    }
    clients.set(id, client)
    console.log(`[CONNECT] Client ${id.slice(0, 8)} connected. Total clients: ${clients.size}`)
    send(ws, { type: 'welcome', clientId: id })

    ws.on('pong', () => { client.isAlive = true })

    // 5초 내 auth가 없으면 익명 관람자로 진행
    const authTimer = setTimeout(() => {
      if (client.authState === 'pending') {
        client.authState = 'anonymous'
        console.log(`[AUTH] ${id.slice(0, 8)} no auth within ${AUTH_TIMEOUT_MS}ms - anonymous viewer`)
        send(ws, { type: 'auth-result', authenticated: false })
      }
    }, AUTH_TIMEOUT_MS)
    authTimer.unref?.()

    const sendAuthResult = () => {
      send(ws, { type: 'auth-result', authenticated: isAuthenticated(client) || (insecureDevMode && client.authState === 'authenticated'), userId: client.userId ?? null })
    }

    /** 현재 방 상태를 이 연결에 전송 (방장이면 대기 중인 요청도) */
    const sendRoomState = (roomId: string) => {
      const others = getRoomParticipants(roomId).filter(p => p.oderId !== id)
      send(ws, {
        type: 'room-state',
        participants: others,
        peerIds: others.map(p => p.oderId),
        isHost: client.isHost,
      })
      if (client.isHost) {
        for (const request of performRequests.get(roomId) || []) send(ws, { type: 'perform-request-received', request })
        for (const request of instrumentChangeRequests.get(roomId) || []) send(ws, { type: 'instrument-change-request-received', request })
      }
    }

    /**
     * 세션 재개: 이 연결을 유예 중이던 이전 Client(clientId/방/연주 상태)에 붙인다.
     * 방 참가자에게는 leave/joined를 보내지 않는다(그들 입장에서는 끊긴 적이 없음).
     */
    const resumeSession = (old: Client, token: string) => {
      const temp = client
      if (old.suspendTimer) clearTimeout(old.suspendTimer)
      old.suspendTimer = undefined
      suspended.delete(old.id)

      old.ws = ws
      old.ip = temp.ip
      old.isAlive = true
      // 이 연결에서 이미 큐에 쌓인 메시지 순서를 유지하기 위해 체인/카운터를 넘겨받는다
      old.chain = temp.chain
      old.pending = temp.pending
      old.msgBucket = temp.msgBucket
      old.chatBucket = temp.chatBucket
      old.joinBucket = temp.joinBucket
      old.dropped = 0
      old.authState = 'authenticated'
      old.tokenExp = jwtExpiryMs(token)

      if (temp.expiryTimer) clearTimeout(temp.expiryTimer)
      clients.delete(temp.id)
      client = old
      id = old.id
      scheduleExpiryCheck(client)

      console.log(`[RESUME] ${id.slice(0, 8)} resumed session (user ${client.userId?.slice(0, 8)}, room ${client.roomId?.slice(0, 8) || 'none'}, performing: ${client.isPerforming})`)
      send(ws, { type: 'welcome', clientId: id, resumed: true })
      send(ws, { type: 'auth-result', authenticated: true, userId: client.userId ?? null, resumed: true })
      if (client.roomId && rooms.has(client.roomId)) sendRoomState(client.roomId)
    }

    const handleAuth = async (msg: Msg) => {
      const token = msg.accessToken

      // 재인증 (L1): 같은 사용자의 갱신 토큰이면 만료 연장, 방 밖의 익명 연결은 로그인으로 승격
      if (client.authState === 'authenticated' || client.authState === 'anonymous') {
        if (insecureDevMode || !isNonEmptyString(token, 8192)) return
        const uid = await deps.verifyToken(token)
        if (!uid) return
        if (client.authState === 'authenticated' && uid === client.userId) {
          client.tokenExp = jwtExpiryMs(token)
          scheduleExpiryCheck(client)
        } else if (client.authState === 'anonymous' && !client.roomId) {
          client.userId = uid
          client.authState = 'authenticated'
          client.tokenExp = jwtExpiryMs(token)
          scheduleExpiryCheck(client)
          console.log(`[AUTH] ${id.slice(0, 8)} upgraded to user ${uid.slice(0, 8)}`)
        }
        sendAuthResult()
        return
      }

      client.authState = 'verifying'
      if (isNonEmptyString(token, 8192) && deps.hasBackend) {
        const uid = await deps.verifyToken(token)
        // 세션 재개: 이전 clientId의 주인이 같은 userId이고 유예시간 안(suspended에 남아 있음)일 때만
        const resumeId = msg.resumeClientId
        if (uid && isNonEmptyString(resumeId, 64)) {
          const old = suspended.get(resumeId)
          if (old && old.userId === uid && clients.get(old.id) === old) {
            resumeSession(old, token)
            return
          }
        }
        if (uid) {
          client.userId = uid
          client.authState = 'authenticated'
          client.tokenExp = jwtExpiryMs(token)
          scheduleExpiryCheck(client)
        }
      } else if (insecureDevMode && isNonEmptyString(token, 8192)) {
        // 개발 모드: 검증 불가. userId는 join 시 클라이언트 값 사용
        client.authState = 'authenticated'
      }

      if (client.authState !== 'authenticated') client.authState = 'anonymous'
      console.log(`[AUTH] ${id.slice(0, 8)} ${client.authState}${client.userId ? ` user ${client.userId.slice(0, 8)}` : ''}`)
      sendAuthResult()
    }

    /** 방 비밀번호 검증 + 실패 횟수 제한. 통과하면 true, 아니면 에러를 보내고 false */
    const checkRoomPassword = async (roomId: string, hash: string, password: unknown): Promise<boolean> => {
      if (typeof password !== 'string' || password.length === 0) {
        send(ws, { type: 'error', code: 'password-required', roomId, message: '비밀번호가 필요한 방입니다.' })
        return false
      }
      // 익명은 IP(/64) 단위로 묶는다 (재접속·주소 회전으로 횟수를 초기화하지 못하게)
      const subject = client.userId ? `u:${client.userId}` : `anon:${ipBucket(client.ip)}`
      const attempt = passwordLimiter.begin(roomId, subject, client.ip)
      if (!attempt.ok) {
        send(ws, { type: 'error', code: 'password-rate-limited', roomId, retryAfterMs: attempt.retryAfterMs, message: '비밀번호를 너무 많이 틀렸습니다. 잠시 후 다시 시도하세요.' })
        return false
      }
      let valid: boolean
      try {
        valid = await verifyRoomPassword(password, hash, scryptGate)
      } catch (err) {
        if (!(err instanceof ScryptBusyError)) throw err
        passwordLimiter.cancelled(roomId, subject, client.ip)
        send(ws, { type: 'error', code: 'password-busy', roomId, retryAfterMs: 2000, message: '서버가 혼잡합니다. 잠시 후 다시 시도하세요.' })
        return false
      }
      if (valid) {
        passwordLimiter.succeeded(roomId, subject, client.ip)
        return true
      }
      console.warn(`[JOIN] ${id.slice(0, 8)} wrong password for room ${roomId.slice(0, 8)} (remaining ${attempt.remaining})`)
      send(ws, { type: 'error', code: 'password-invalid', roomId, remainingAttempts: attempt.remaining, message: '비밀번호가 올바르지 않습니다.' })
      return false
    }

    const handleJoin = async (msg: Msg) => {
      const roomId = msg.roomId
      if (!isNonEmptyString(roomId, MAX_ROOM_ID_LENGTH)) return

      // 개발 모드에서만 클라이언트 userId 사용 (그 외에는 auth로 확정된 값만)
      if (insecureDevMode && client.authState === 'authenticated' && !client.userId && isNonEmptyString(msg.userId, 128)) {
        client.userId = msg.userId
      }

      // 방 존재 확인 + 방장 판정 (백엔드가 있으면 DB 기준)
      let isHost = false
      let passwordHash: string | null = null
      let capacity = limits.maxRoomParticipants
      if (deps.hasBackend) {
        const meta = await getRoomMeta(roomId, 0)
        if (!meta) {
          send(ws, { type: 'error', code: 'room-not-found', message: '방을 찾을 수 없습니다.' })
          return
        }
        isHost = !!client.userId && meta.hostId === client.userId
        // 방금 읽은 DB 방장이 메트로놈을 켠 사람과 다르면(이양) 진행 중인 메트로놈을 멈춘다(PR #36 L1)
        dropMetronomeIfHostChanged(roomId, meta.hostId ?? null)
        passwordHash = meta.passwordHash ?? null
        if (meta.maxParticipants !== undefined) capacity = Math.min(capacity, meta.maxParticipants)
        // 비밀번호 방으로 표시됐는데 해시가 아직 없음(생성·변경 중): 방장 외 입장 보류 (fail-closed, 리뷰 L1)
        if (meta.hasPassword && !passwordHash && !isHost) {
          send(ws, { type: 'error', code: 'password-busy', roomId, retryAfterMs: 1000, message: '합주실을 준비 중입니다. 잠시 후 다시 시도하세요.' })
          return
        }
      } else if (insecureDevMode && client.authState === 'authenticated') {
        isHost = msg.isHost === true
      }
      // DB 조회 중 연결이 끊겼으면 중단
      if (clients.get(id) !== client) return

      // 이미 이 방에 있음 (세션 재개 후 클라이언트의 rejoin 등): 상태만 다시 보내고 브로드캐스트 없음
      if (client.roomId === roomId && rooms.get(roomId)?.has(id)) {
        client.isHost = isHost
        const nickname = sanitizeNickname(msg.nickname)
        if (nickname) client.nickname = nickname
        sendRoomState(roomId)
        return
      }

      // 비밀번호 방: 방장이 아니면 검증 (실패하면 지금 있는 방은 그대로 둔다)
      if (passwordHash && !isHost) {
        const ok = await checkRoomPassword(roomId, passwordHash, msg.password)
        if (!ok || clients.get(id) !== client) return
      }

      // 정원(관람 포함, P3-12b): 방장은 항상 입장. 마지막 await 뒤부터 방에 추가할 때까지 동기 구간이라 동시 입장 경합이 없다.
      // 유예 중인 세션은 rooms에 남아 자리를 유지하고, 같은 사용자의 기존 연결(아래에서 대체됨)은 새 자리로 세지 않는다.
      // 승인 보유자·자리 예약(유예 밖으로 끊긴 연주자, M2)은 정원 검사를 건너뛴다. 로그인 사용자가 익명보다 우선이다(H1).
      // 실패해도 지금 있는 방은 그대로 둔다.
      const reserved = hasApproval(roomId, client.userId) || hasSeatHold(roomId, client.userId)
      const rejected = isHost || reserved ? null : checkCapacity(roomId, client, capacity)
      if (rejected) {
        console.warn(`[JOIN] ${id.slice(0, 8)} rejected: room ${roomId.slice(0, 8)} ${rejected} (${capacity})`)
        const message = rejected === 'room-full'
          ? `합주실 정원(${capacity}명)이 가득 찼습니다.`
          : '로그인하지 않은 관람자 자리가 가득 찼습니다. 로그인하면 입장할 수 있습니다.'
        send(ws, { type: 'error', code: 'room-full', reason: rejected, roomId, maxParticipants: capacity, message })
        return
      }
      clearSeatHold(roomId, client.userId)

      // 다른 방에 있었다면 먼저 퇴장 처리
      if (client.roomId && client.roomId !== roomId) {
        handleLeave(client, client.roomId, 'move')
        client.roomId = undefined
        client.isPerforming = false
        client.instrument = undefined
        clearNative(client)
      }
      client.isHost = isHost

      const nickname = sanitizeNickname(msg.nickname)
      if (nickname) client.nickname = nickname

      // 같은 userId 중복 연결 제거 (서버 확정 userId 기준) — 상태 정리까지 수행 (L3)
      let replacedDuplicate = false
      if (client.userId) {
        for (const existingId of [...(rooms.get(roomId) || [])]) {
          if (existingId === id) continue
          const existing = clients.get(existingId)
          if (existing && existing.userId === client.userId) {
            console.log(`[JOIN] Removing duplicate connection for userId ${client.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`)
            handleLeave(existing, roomId, 'duplicate')
            existing.roomId = undefined // close 핸들러에서 중복 퇴장 처리 방지
            if (existing.suspendTimer) clearTimeout(existing.suspendTimer)
            existing.suspendTimer = undefined
            suspended.delete(existingId)
            clients.delete(existingId)
            existing.ws.close(1000, 'Duplicate connection')
            replacedDuplicate = true
          }
        }
      }

      // rejoin 여부는 서버가 판단: 같은 사용자가 직전에 이 방에서 끊겼거나 중복 연결을 대체한 경우만
      const isRejoin = msg.isRejoin === true && (consumeRejoin(roomId, client.userId) || replacedDuplicate)
      if (client.userId && approvals.get(roomId)?.has(client.userId) && hasApproval(roomId, client.userId)) {
        setApproval(roomId, client.userId, null) // 재접속: 승인 유예 해제
      }

      // 이 clientId가 이미 방에 있었는지 (세션 재개 후 클라이언트의 rejoin은 같은 id라서 true)
      const alreadyInRoom = rooms.get(roomId)?.has(id) ?? false
      client.roomId = roomId
      if (!rooms.has(roomId)) rooms.set(roomId, new Set())
      rooms.get(roomId)!.add(id)

      const participants = getRoomParticipants(roomId)
      console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.nickname}) joined room ${roomId.slice(0, 8)}. Room size: ${participants.length}${client.isHost ? ' [HOST]' : ''}${client.authState === 'anonymous' ? ' [ANON]' : ''}`)

      scheduleParticipantUpdate(roomId)

      // 새 clientId가 방에 들어오면 rejoin이어도 항상 알린다 (H1: 다른 클라이언트가 새 id를 peers/관람자 목록에 넣어야
      // 방송 담당이 offer를 보낼 수 있다). rejoin 플래그는 UI 입장 알림 억제용으로만 함께 보낸다.
      if (!alreadyInRoom) {
        broadcastToRoom(roomId, id, {
          type: 'participant-joined',
          participant: { oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: client.isPerforming, isHost: client.isHost },
          rejoin: isRejoin,
        })
      }

      sendRoomState(roomId)
    }

    /** 연주 시작 허용 여부 (H1): 방장, free_join 방, 또는 방장이 승인한 사용자 */
    const canStartPerforming = async (roomId: string): Promise<boolean> => {
      if (!isAuthenticated(client) && !(insecureDevMode && client.authState === 'authenticated')) return false
      if (insecureDevMode) return true
      if (client.isHost) return true
      if (hasApproval(roomId, client.userId)) return true
      const meta = await getRoomMeta(roomId)
      return !!meta && meta.freeJoin
    }

    const handleMessage = async (msg: Msg) => {
      if (msg.type === 'auth') {
        await handleAuth(msg)
        return
      }

      // auth 전 메시지는 거부
      if (client.authState === 'pending' || client.authState === 'verifying') {
        send(ws, { type: 'error', code: 'auth-required', message: 'auth 메시지를 먼저 보내야 합니다.', rejectedType: msg.type })
        return
      }

      if (msg.type === 'join') {
        await handleJoin(msg)
        return
      }

      const roomId = client.roomId

      if (msg.type === 'start-performing' && roomId) {
        if (!isAuthenticated(client) && !(insecureDevMode && client.authState === 'authenticated')) {
          send(ws, { type: 'error', code: 'login-required', message: '연주하려면 로그인이 필요합니다.' })
          return
        }
        if (!(await canStartPerforming(roomId)) || client.roomId !== roomId) {
          send(ws, { type: 'error', code: 'perform-not-approved', message: '방장의 승인이 필요합니다.' })
          return
        }
        // 연주자 수 제한 (이미 연주 중인 본인의 재전송은 허용)
        if (!client.isPerforming) {
          let performers = 0
          for (const pid of rooms.get(roomId) || []) {
            if (pid !== id && clients.get(pid)?.isPerforming) performers++
          }
          if (performers >= limits.maxPerformers) {
            send(ws, { type: 'error', code: 'room-full-performers', message: `연주자는 최대 ${limits.maxPerformers}명입니다.` })
            return
          }
        }
        client.isPerforming = true
        client.instrument = typeof msg.instrument === 'string' ? msg.instrument.slice(0, 50) : undefined
        const nickname = sanitizeNickname(msg.nickname)
        if (nickname) client.nickname = nickname
        // 클라이언트 isHost는 무시 (서버 판정)
        console.log(`[START-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) started performing: ${client.instrument}, isHost: ${client.isHost}`)
        broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: true, isHost: client.isHost })
        return
      }

      if (msg.type === 'stop-performing' && roomId) {
        client.isPerforming = false
        client.instrument = undefined
        // 연주자가 아니게 되면 ssrc를 잊는다(토큰 갱신 불가 → 기존 토큰 만료와 함께 릴레이 슬롯 해제)
        if (!isPerformer(client)) client.nativeSsrc = undefined
        console.log(`[STOP-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) stopped performing`)
        broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: undefined, isPerforming: false })
        return
      }

      // WebRTC 시그널링: 같은 방 안에서, 양쪽 중 한쪽 이상이 연주자일 때만 (M1)
      if (msg.type === 'offer' || msg.type === 'answer' || msg.type === 'ice-candidate') {
        const target = getRoomPeer(client, msg.to)
        if (target && (isPerformer(client) || isPerformer(target))) {
          console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${target.id.slice(0, 8)}`)
          send(target.ws, { ...msg, from: id })
        }
        return
      }

      // 네이티브 엔진 시그널링: 같은 방 연주자끼리만
      if (msg.type === 'native-descriptor') {
        if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
        const descriptor = msg.descriptor
        if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) return
        const d = descriptor as Record<string, unknown>
        // 서버가 아는 필드(ssrc, key, candidates)를 모두 검증하고 그 값만으로 다시 구성한다(PR #48 M1)
        const key = parseEngineKey(d.key)
        const ssrc = d.ssrc
        const candidates = parseCandidates(d.candidates)
        if (!key || !isValidSsrc(ssrc) || !candidates) {
          send(ws, { type: 'error', code: 'descriptor-invalid', message: '엔진 연결 정보가 올바르지 않습니다.' })
          return
        }
        if (keyTakenInRoom(client, key)) {
          // 같은 방의 다른 사용자가 이미 이 공개키로 등록했다: 위장·선점 시도. 중계하지 않는다.
          // 정상 클라이언트가 이걸 받으면(키를 먼저 빼앗긴 경우) 엔진을 새 키로 다시 시작한다.
          console.warn(`[NATIVE] key-conflict from ${id.slice(0, 8)} in room ${roomId.slice(0, 8)}`)
          send(ws, { type: 'error', code: 'key-conflict', message: '같은 방의 다른 사용자가 이미 쓰는 엔진 키입니다.' })
          return
        }
        if (ssrcTakenInRoom(client, ssrc)) {
          send(ws, { type: 'error', code: 'ssrc-conflict', message: '같은 방에서 이미 쓰는 ssrc입니다. 엔진을 다시 시작하세요.' })
          return
        }
        const prevSsrc = client.nativeSsrc
        client.nativeActive = true
        client.nativeKey = key // 같은 사용자의 새 키(엔진 재시작)는 옛 바인딩을 덮어쓴다
        client.nativeSsrc = ssrc
        sendRelayToken(client)
        // 방장 엔진이 native-leave 없이 새로 떴다(ssrc가 바뀜, 예: ssrc-conflict 재시작): 메트로놈 startUs는 옛 엔진 시계
        // 기준이라 무의미하다 → 지우고 연주자들에게 정지를 알린다(PR #36 M1). 같은 엔진의 재알림(WS 재개·회신)은 ssrc가 같다.
        const hostMetro = metronomes.get(roomId)
        if (hostMetro && hostMetro.hostId === id && prevSsrc !== undefined && ssrc !== prevSsrc) {
          metronomes.delete(roomId)
          broadcastMetronomeStop(roomId, hostMetro)
        }
        // 엔진이 (다시) 떴다: 방장이 켠 메트로놈이 있으면 지금 상태를 알려 준다(늦게 들어온 연주자, 엔진 재시작).
        // 방장 확인(DB)은 비동기라 descriptor 중계를 늦추지 않게 따로 돈다
        void replayMetronome(client, roomId)
        // 서버가 아는 필드만 다시 구성한다(클라이언트가 descriptor 안에 넣은 from/userId 등은 버림). from은 서버의 clientId
        const out = { type: 'native-descriptor', from: id, descriptor: { ssrc, key, candidates } }
        if (msg.to !== undefined) {
          const target = getRoomPeer(client, msg.to)
          if (target && isPerformer(target)) send(target.ws, out)
          return
        }
        for (const peerId of rooms.get(roomId) || []) {
          if (peerId === id) continue
          const peer = clients.get(peerId)
          if (peer && isPerformer(peer)) send(peer.ws, out)
        }
        return
      }

      // 공유 메트로놈(P2-3): 방장(DB 판정)이 자기 엔진을 켠 상태에서만. 같은 방 연주자에게 중계(from은 서버가 붙임)
      if (msg.type === 'metronome') {
        if (!roomId || !isAuthenticated(client) || !client.nativeActive || !isPerformer(client)) return
        const state = parseMetronomeState(msg.state)
        if (!state) {
          send(ws, { type: 'error', code: 'metronome-invalid', message: '메트로놈 설정이 올바르지 않습니다.' })
          return
        }
        if (!(await refreshHost(client)) || client.roomId !== roomId) {
          // 메트로놈을 켰던 연결이 더 이상 방장이 아니다(이양): 진행 중인 메트로놈도 멈춘다(PR #36 L1)
          // (그때 보낸 metronome-host-only가 이 요청의 거부 응답도 된다)
          if (client.roomId === roomId && metronomes.get(roomId)?.hostId === id &&
              dropMetronomeIfHostChanged(roomId, roomMetaCache.get(roomId)?.meta?.hostId ?? null)) return
          send(ws, { type: 'error', code: 'metronome-host-only', message: '메트로놈은 방장만 조작할 수 있습니다.' })
          return
        }
        // 개발 모드(DB 없음)는 방장 확인을 할 수 없어 hostUserId를 비운다(이양 검사 생략)
        if (state.running) metronomes.set(roomId, { hostId: id, hostUserId: deps.hasBackend ? client.userId : undefined, state })
        else metronomes.delete(roomId)
        for (const peerId of rooms.get(roomId) || []) {
          if (peerId === id) continue
          const peer = clients.get(peerId)
          if (peer && isPerformer(peer)) send(peer.ws, { type: 'metronome', from: id, state })
        }
        return
      }

      if (msg.type === 'relay-token-request') {
        if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
        sendRelayToken(client)
        return
      }

      if (msg.type === 'native-leave') {
        if (!roomId || !isAuthenticated(client) || !(isPerformer(client) || client.nativeActive)) return
        clearNative(client)
        // 방장 엔진이 내려가면 메트로놈도 끝난다(따르는 쪽은 native-leave를 받아 스스로 멈춘다)
        if (metronomes.get(roomId)?.hostId === id) metronomes.delete(roomId)
        for (const peerId of rooms.get(roomId) || []) {
          if (peerId === id) continue
          const peer = clients.get(peerId)
          if (peer && (isPerformer(peer) || peer.nativeActive)) send(peer.ws, { type: 'native-leave', from: id })
        }
        return
      }

      // 채팅: 서버가 필드를 새로 구성 (M4)
      if (msg.type === 'chat' && roomId) {
        if (typeof msg.message !== 'string') return
        const message = msg.message.trim().slice(0, MAX_CHAT_LENGTH)
        if (!message) return
        broadcastToRoom(roomId, id, { type: 'chat', message, nickname: client.nickname, timestamp: Date.now() })
        return
      }

      if (msg.type === 'request-perform' && roomId && isNonEmptyString(msg.instrument, 50)) {
        if (!isAuthenticated(client)) {
          send(ws, { type: 'error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
          send(ws, { type: 'request-perform-error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
          return
        }
        if (!performRequests.has(roomId)) performRequests.set(roomId, [])
        const requests = performRequests.get(roomId)!
        const host = findRoomHost(roomId)
        const existing = requests.find(r => r.oderId === id)
        if (existing) {
          // 재전송(화면 새로고침 등): 대기 상태만 다시 알려준다
          send(ws, { type: 'perform-request-sent', instrument: existing.instrument, hostOnline: !!host })
          return
        }
        const request: PerformRequest = { oderId: id, nickname: client.nickname, instrument: msg.instrument, timestamp: Date.now() }
        requests.push(request)
        console.log(`[REQUEST-PERFORM] ${id.slice(0, 8)} (${client.nickname}) requested to perform: ${msg.instrument}`)
        if (host) send(host.ws, { type: 'perform-request-received', request })
        // hostOnline=false면 방장이 입장할 때 room-state와 함께 요청이 전달된다
        send(ws, { type: 'perform-request-sent', instrument: msg.instrument, hostOnline: !!host })
        return
      }

      // 승인 계열: 방장 여부를 다시 확인 (L2)
      const isHostAction = ['approve-perform', 'reject-perform', 'approve-instrument-change', 'reject-instrument-change'].includes(msg.type)
      if (isHostAction) {
        if (!roomId || typeof msg.targetId !== 'string') return
        if (!(await refreshHost(client)) || client.roomId !== roomId) return
      }

      if (msg.type === 'approve-perform' && roomId && typeof msg.targetId === 'string') {
        const requests = performRequests.get(roomId)
        const target = getRoomPeer(client, msg.targetId)
        if (!requests || !target) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        if (target.userId) setApproval(roomId, target.userId, null)
        console.log(`[APPROVE-PERFORM] Host approved ${target.id.slice(0, 8)} (${request.nickname}) for ${request.instrument}`)
        send(target.ws, { type: 'perform-request-approved', instrument: request.instrument })
        return
      }

      if (msg.type === 'reject-perform' && roomId && typeof msg.targetId === 'string') {
        const requests = performRequests.get(roomId)
        if (!requests) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        console.log(`[REJECT-PERFORM] Host rejected ${msg.targetId.slice(0, 8)} (${request.nickname})`)
        const target = getRoomPeer(client, msg.targetId)
        const reason = (typeof msg.reason === 'string' ? msg.reason.trim().slice(0, 200) : '') || '방장이 요청을 거절했습니다.'
        if (target) send(target.ws, { type: 'perform-request-rejected', reason, instrument: request.instrument })
        return
      }

      if (msg.type === 'cancel-perform-request' && roomId) {
        const requests = performRequests.get(roomId)
        const host = findRoomHost(roomId)
        if (requests) {
          const requestIndex = requests.findIndex(r => r.oderId === id)
          if (requestIndex !== -1) {
            requests.splice(requestIndex, 1)
            if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
          }
        }
        return
      }

      if (msg.type === 'request-instrument-change' && roomId && client.isPerforming && isNonEmptyString(msg.newInstrument, 50)) {
        if (!instrumentChangeRequests.has(roomId)) instrumentChangeRequests.set(roomId, [])
        const requests = instrumentChangeRequests.get(roomId)!
        if (requests.find(r => r.oderId === id)) return
        const request: InstrumentChangeRequest = { oderId: id, nickname: client.nickname, currentInstrument: client.instrument || '', newInstrument: msg.newInstrument, timestamp: Date.now() }
        requests.push(request)
        const host = findRoomHost(roomId)
        if (host) send(host.ws, { type: 'instrument-change-request-received', request })
        send(ws, { type: 'instrument-change-request-sent', newInstrument: msg.newInstrument })
        return
      }

      if (msg.type === 'approve-instrument-change' && roomId && typeof msg.targetId === 'string') {
        const requests = instrumentChangeRequests.get(roomId)
        const target = getRoomPeer(client, msg.targetId)
        if (!requests || !target) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        target.instrument = request.newInstrument
        send(target.ws, { type: 'instrument-change-approved', newInstrument: request.newInstrument })
        for (const oderId of rooms.get(roomId) || []) {
          const peer = clients.get(oderId)
          if (peer && peer.id !== target.id) send(peer.ws, { type: 'participant-instrument-changed', oderId: target.id, instrument: request.newInstrument })
        }
        return
      }

      if (msg.type === 'reject-instrument-change' && roomId && typeof msg.targetId === 'string') {
        const requests = instrumentChangeRequests.get(roomId)
        if (!requests) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        requests.splice(requestIndex, 1)
        const target = getRoomPeer(client, msg.targetId)
        const reason = typeof msg.reason === 'string' ? msg.reason.slice(0, 200) : '방장이 악기 변경을 거절했습니다.'
        if (target) send(target.ws, { type: 'instrument-change-rejected', reason })
        return
      }

      if (msg.type === 'cancel-instrument-change-request' && roomId) {
        const requests = instrumentChangeRequests.get(roomId)
        const host = findRoomHost(roomId)
        if (requests) {
          const requestIndex = requests.findIndex(r => r.oderId === id)
          if (requestIndex !== -1) {
            requests.splice(requestIndex, 1)
            if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
          }
        }
        return
      }

      if (msg.type === 'leave' && roomId) {
        handleLeave(client, roomId, 'leave')
        client.roomId = undefined
        client.isPerforming = false
        client.instrument = undefined
        client.isHost = false
        clearNative(client)
        return
      }

      // 알 수 없는 type은 무시
    }

    /** 메시지 rate limit (M3). 통과하면 true */
    const allowMessage = (msg: Msg): boolean => {
      let ok = client.msgBucket.take()
      if (ok && msg.type === 'chat') ok = client.chatBucket.take()
      if (ok && (msg.type === 'join' || msg.type === 'leave')) ok = client.joinBucket.take()
      if (ok) return true

      client.dropped++
      if (client.dropped >= limits.maxDropped) {
        console.warn(`[WS] ${id.slice(0, 8)} exceeded rate limit repeatedly - closing`)
        ws.close(1008, 'Rate limit exceeded')
        return false
      }
      const now = Date.now()
      if (now - client.lastRateLimitNotice > 1000) {
        client.lastRateLimitNotice = now
        send(ws, { type: 'error', code: 'rate-limited', message: '메시지를 너무 많이 보냈습니다.', rejectedType: msg.type })
      }
      return false
    }

    ws.on('message', (raw: RawData, isBinary: boolean) => {
      if (isBinary) return
      let msg: Msg
      try {
        const parsed: unknown = JSON.parse(raw.toString())
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return
        if (typeof (parsed as { type?: unknown }).type !== 'string') return
        msg = parsed as Msg
      } catch {
        return // JSON 파싱 실패는 무시
      }

      if (!allowMessage(msg)) return

      if (client.pending >= MAX_PENDING_MESSAGES) {
        console.warn(`[WS] ${id.slice(0, 8)} too many pending messages - closing`)
        ws.close(1008, 'Too many pending messages')
        return
      }
      client.pending++
      // 메시지를 순차 처리 (auth 검증/DB 조회 같은 비동기 작업 중에도 순서 유지)
      client.chain = client.chain
        .then(() => (clients.get(id) === client ? handleMessage(msg) : undefined))
        .catch(err => console.error('[ERROR] Failed to handle message:', err))
        .finally(() => { client.pending-- })
    })

    // 소켓 에러(예: maxPayload 초과 1009)를 처리하지 않으면 프로세스가 죽는다
    ws.on('error', (err) => {
      console.warn(`[WS] ${id.slice(0, 8)} socket error: ${err.message}`)
    })

    ws.on('close', () => {
      clearTimeout(authTimer)
      if (client.expiryTimer) clearTimeout(client.expiryTimer)
      const n = (connectionsPerIp.get(ip) || 1) - 1
      if (n <= 0) connectionsPerIp.delete(ip)
      else connectionsPerIp.set(ip, n)

      // 이미 다른 연결로 대체/정리된 Client (중복 연결 제거 등)
      if (clients.get(id) !== client) return

      const roomId = client.roomId
      console.log(`[DISCONNECT] Client ${id.slice(0, 8)} disconnected. roomId: ${roomId?.slice(0, 8) || 'none'}`)

      // 인증된 사용자가 방 안에서 끊김: 유예시간 동안 상태 유지 (참가자에게 leave 안 보냄)
      if (roomId && rooms.has(roomId) && isAuthenticated(client) && limits.resumeGraceMs > 0) {
        const c = client
        suspended.set(c.id, c)
        c.suspendTimer = setTimeout(() => finalizeSuspended(c), limits.resumeGraceMs)
        c.suspendTimer.unref?.()
        console.log(`[RESUME] ${c.id.slice(0, 8)} suspended for ${limits.resumeGraceMs}ms`)
        return
      }

      if (roomId && rooms.has(roomId)) handleLeave(client, roomId, 'close')
      client.roomId = undefined
      clients.delete(id)
    })
  })

  return wss
}
