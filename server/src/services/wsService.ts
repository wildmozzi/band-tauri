import { WebSocketServer, type RawData, type WebSocket } from 'ws'
import { Server as HttpServer } from 'http'
import { Server as HttpsServer } from 'https'
import { v4 as uuidv4 } from 'uuid'
import { SupabaseClient } from '@supabase/supabase-js'
import { isInsecureDevMode } from '../lib/securityConfig'
import type { RoomMeta } from '../repositories/roomRepository'
import { defaultScryptGate, ipBucket } from '../lib/roomPassword'
import { AUTH_TIMEOUT_MS, MAX_MESSAGE_BYTES, MAX_PENDING_MESSAGES, MAX_PENDING_RELAY_BYTES, MAX_PENDING_RELAY_MESSAGES, MAX_RELAY_MESSAGE_BYTES, type Msg } from '../signaling/protocol'
import { DEFAULT_LIMITS, type Client, type WsDeps, type WsLimits, type WsOptions } from '../signaling/types'
import { TokenBucket } from '../signaling/rateLimit'
import { createMetronome } from '../signaling/metronome'
import { clientIp } from '../signaling/connection'
import { supabaseDeps } from '../signaling/supabaseDeps'
import { createRegistry, send, type ActivityCounts } from '../signaling/registry'
import { createRoomMeta } from '../signaling/roomMeta'
import { createRequests } from '../signaling/requests'
import { createCapacity } from '../signaling/capacity'
import { createPassword } from '../signaling/password'
import { createPerformers } from '../signaling/performers'
import { createNativeSession } from '../signaling/nativeSession'
import { handleLeave } from '../signaling/membership'
import { createResume, suspendOnClose } from '../signaling/resume'
import { AUTH_RATE_LIMITED, cachedTokenVerifier } from '../signaling/auth'
import { dispatch, type ConnCtx, type SignalingServices } from '../signaling/dispatch'
import { RELAY_MESSAGE_TYPES, takeRelayMessage } from '../signaling/viewerRelay'

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

/** 수신 프레임 원문 바이트(ws RawData: Buffer | ArrayBuffer | Buffer[]) */
function rawBytes(raw: RawData): number {
  if (Buffer.isBuffer(raw)) return raw.length
  if (Array.isArray(raw)) return raw.reduce((a, b) => a + b.length, 0)
  return raw.byteLength
}

// S1(docs/server-architecture.md 3절): 클로저 밖 순수 코드는 signaling/*로 옮겼다. 기존 import 경로를 유지하는 재수출
export type { RoomMeta }
export type { WsDeps, WsLimits, WsOptions } from '../signaling/types'
export type { EngineCandidate, MetronomeState } from '../signaling/protocol'
export { MAX_ENGINE_CANDIDATES, parseCandidates, parseEngineKey } from '../signaling/descriptor'
export { parseMetronomeState } from '../signaling/metronome'
export { jwtExpiryMs } from '../signaling/auth'
export { roomDeps } from '../signaling/roomDeps'
export type { ActivityCounts } from '../signaling/registry'

/** 서버별 운영 집계 함수 (P1-17①). 상태 맵은 내보내지 않고 숫자만 돌려주는 함수를 건다 */
const activityByServer = new WeakMap<WebSocketServer, () => ActivityCounts>()

/** 이 WS 서버의 지금 활성 방·연주자·관람자 수 (배포 전 확인용, GET /api/ops/activity). 모르는 서버면 null */
export function wsActivity(wss: WebSocketServer): ActivityCounts | null {
  return activityByServer.get(wss)?.() ?? null
}


/** 받아들이지 않는 연결: 닫힘 코드는 보내되(클라이언트가 사유를 안다) 상대가 close에 답하지 않아도 곧 끊는다.
 *  close만 하면 ws가 최대 30초 wss.clients에 남겨 전체 정원을 차지한다(#187 리뷰 L) */
const REJECT_TERMINATE_MS = 1000
function rejectConnection(ws: WebSocket, code: number, reason: string) {
  ws.close(code, reason)
  setTimeout(() => ws.terminate(), REJECT_TERMINATE_MS).unref?.()
}

export function initializeWebSocket(
  server: HttpServer | HttpsServer,
  supabase: SupabaseClient | null,
  options: WsOptions = {},
): WebSocketServer {
  const env = options.env ?? process.env
  const baseDeps: WsDeps = { ...supabaseDeps(supabase), ...options.deps }
  // 같은 토큰의 getUser 반복·동시 호출을 한 번으로 (서버 보안 점검: auth 증폭 방지)
  const deps: WsDeps = baseDeps
  const tokenVerifier = cachedTokenVerifier(deps.verifyToken)
  /** IP(/64) → auth getUser 버킷. 하트비트가 가득 찬(오래 안 쓴) 것을 지운다 */
  const authIpBuckets = new Map<string, TokenBucket>()
  /** 캐시 미스(실제 getUser)일 때만 연결별·IP별 버킷을 쓴다 (#187 리뷰 M) */
  const verifyAuth = (client: Client, token: string): Promise<string | null | typeof AUTH_RATE_LIMITED> =>
    tokenVerifier(token, () => {
      const key = ipBucket(client.conn.ip)
      let bucket = authIpBuckets.get(key)
      if (!bucket) authIpBuckets.set(key, bucket = new TokenBucket(limits.authIpBurst, limits.authIpPerSec))
      return client.conn.authBucket.take() && bucket.take()
    })
  const limits: WsLimits = { ...DEFAULT_LIMITS, ...options.limits }
  const scryptGate = options.scryptGate ?? defaultScryptGate

  // 서버 인스턴스별 상태
  const insecureDevMode = isInsecureDevMode(deps.hasBackend, env)
  // S2(docs/server-architecture.md 3절): 상태 소유 모듈. S3: 메시지 처리는 signaling/dispatch 표의 handle*가 한다
  const registry = createRegistry()
  const { getClient, clientCount, allClients, hasRoom, addClient, removeClient } = registry
  const roomMeta = createRoomMeta(deps, insecureDevMode)
  const requests = createRequests()
  const capacity = createCapacity(registry, limits)
  const password = createPassword(limits, scryptGate)
  const performers = createPerformers(roomMeta, insecureDevMode)
  const metronome = createMetronome(registry, roomMeta, deps, limits)
  const nativeSession = createNativeSession(registry, env)
  const participantTimers = new Map<string, ReturnType<typeof setTimeout>>()
  /** IP(IPv4) 또는 IPv6 /64 → 동시 연결 수. IPv6는 한 사용자가 /64를 통째로 받아 주소를 바꿔 가며 상한을 넘을 수 있어 묶는다 */
  const connectionsPerIp = new Map<string, number>()
  const resume = createResume(registry)

  /**
   * 공용 서비스 객체 하나 (S3). 모든 연결의 ctx.svc가 이 객체를 가리킨다(복사하지 않음).
   * 상태 맵은 위에서 만든 객체 그대로다 — svc를 거쳐도 같은 Map이다.
   */
  const svc: SignalingServices = {
    env, deps, limits, insecureDevMode,
    registry, roomMeta, requests, capacity, password, performers, metronome, nativeSession, resume,
    participantTimers, verifyAuth,
  }

  // maxPayload 초과 시 ws가 연결을 1009로 끊는다
  const wss = new WebSocketServer({ server, maxPayload: MAX_MESSAGE_BYTES })
  activityByServer.set(wss, () => registry.activityCounts(resume.isSuspended))

  if (insecureDevMode) {
    console.warn('[WS][SECURITY] INSECURE dev mode (NODE_ENV=development, ALLOW_INSECURE_WS=1, no Supabase): client-sent userId/isHost are trusted.')
  } else if (!deps.hasBackend) {
    console.warn('[WS][SECURITY] Supabase not configured - all clients are anonymous viewers.')
  }

  // ---------- heartbeat (L4) ----------

  const heartbeat = setInterval(() => {
    const now = Date.now()
    capacity.sweep(now)
    performers.sweep(now)
    for (const [key, bucket] of authIpBuckets) if (bucket.isFull()) authIpBuckets.delete(key)
    roomMeta.sweep(now, hasRoom)
    for (const c of allClients()) {
      if (resume.isSuspended(c.id)) continue // 이미 끊긴 연결 (재개 대기)
      if (!c.conn.isAlive) {
        console.log(`[WS] ${c.id.slice(0, 8)} heartbeat timeout - terminating`)
        c.conn.ws.terminate()
        continue
      }
      c.conn.isAlive = false
      try {
        c.conn.ws.ping()
      } catch {
        // 무시
      }
    }
  }, limits.heartbeatMs)
  heartbeat.unref?.()

  // ---------- 전체 송신 대기열 상한 (#184 리뷰 L) ----------
  const bufferWatch = setInterval(() => {
    let total = 0
    for (const ws of wss.clients) total += ws.bufferedAmount
    if (total <= limits.maxTotalBufferedBytes) return
    const heavy = [...wss.clients].filter(ws => ws.bufferedAmount > 0).sort((a, b) => b.bufferedAmount - a.bufferedAmount)
    for (const ws of heavy) {
      if (total <= limits.maxTotalBufferedBytes) break
      total -= ws.bufferedAmount
      console.warn(`[WS] total send buffer over ${limits.maxTotalBufferedBytes} bytes - terminating a connection holding ${ws.bufferedAmount}`)
      ws.terminate()
    }
  }, 1000)
  bufferWatch.unref?.()

  wss.on('close', () => {
    clearInterval(heartbeat)
    clearInterval(bufferWatch)
    for (const t of participantTimers.values()) clearTimeout(t)
    resume.clearTimers()
  })

  wss.on('error', (err) => {
    console.error('[WS] server error:', err.message)
  })

  wss.on('connection', (ws, req) => {
    // wss.clients에는 이 연결이 이미 들어 있다
    if (wss.clients.size > limits.maxConnections) {
      console.warn(`[WS] server full (${limits.maxConnections} connections)`)
      rejectConnection(ws, 1013, 'Server busy')
      return
    }
    const ip = clientIp(req, env)
    const ipKey = ipBucket(ip)
    const ipCount = (connectionsPerIp.get(ipKey) || 0) + 1
    if (ipCount > limits.maxConnectionsPerIp) {
      console.warn(`[WS] too many connections from ${ipKey}`)
      rejectConnection(ws, 1008, 'Too many connections')
      return
    }
    connectionsPerIp.set(ipKey, ipCount)

    // 연결 문맥 (S3). 세션 재개 시 ctx.client·ctx.id가 이전 Client로 교체되므로 모든 핸들러·콜백은 호출 시점에 ctx에서 읽는다
    const newId = uuidv4()
    const ctx: ConnCtx = {
      ws,
      ip,
      id: newId,
      client: {
        id: newId,
        conn: {
          ws,
          ip,
          chain: Promise.resolve(),
          pending: 0,
          pendingRelay: 0,
          pendingRelayBytes: 0,
          isAlive: true,
          msgBucket: new TokenBucket(limits.msgBurst, limits.msgPerSec),
          relayBucket: new TokenBucket(limits.relayMsgBurst, limits.relayMsgPerSec),
          chatBucket: new TokenBucket(limits.chatBurst, limits.chatPerSec),
          joinBucket: new TokenBucket(limits.joinBurst, limits.joinPerSec),
          authBucket: new TokenBucket(limits.authBurst, limits.authPerSec),
          dropped: 0,
          lastRateLimitNotice: 0,
        },
        auth: { state: 'pending', tokenExp: null },
        resume: {},
        member: { nickname: `User${newId.slice(0, 4)}`, isHost: false },
        perform: { isPerforming: false },
        native: { active: false },
      },
      svc,
    }
    addClient(ctx.client)
    console.log(`[CONNECT] Client ${ctx.id.slice(0, 8)} connected. Total clients: ${clientCount()}`)
    send(ws, { type: 'welcome', clientId: ctx.id })

    ws.on('pong', () => { ctx.client.conn.isAlive = true })

    // 5초 내 auth가 없으면 익명 관람자로 진행
    const authTimer = setTimeout(() => {
      if (ctx.client.auth.state === 'pending') {
        ctx.client.auth.state = 'anonymous'
        console.log(`[AUTH] ${ctx.id.slice(0, 8)} no auth within ${AUTH_TIMEOUT_MS}ms - anonymous viewer`)
        send(ws, { type: 'auth-result', authenticated: false })
      }
    }, AUTH_TIMEOUT_MS)
    authTimer.unref?.()

    /** 메시지 rate limit (M3). 통과하면 true */
    const allowMessage = (msg: Msg): boolean => {
      const client = ctx.client
      // 관람 중계는 일반 버킷 대신 중계 버킷(방송 담당은 방 인원에 비례) — 쌍별·대상별 전달 한도는 viewerRelay에서 그대로
      let ok = RELAY_MESSAGE_TYPES.has(msg.type) ? takeRelayMessage(client, registry, limits) : client.conn.msgBucket.take()
      if (ok && msg.type === 'chat') ok = client.conn.chatBucket.take()
      if (ok && (msg.type === 'join' || msg.type === 'leave')) ok = client.conn.joinBucket.take()
      if (ok) return true
      noteDropped(msg.type)
      return false
    }

    /** 속도 제한으로 버린 메시지 하나: dropped를 세고(maxDropped면 1008), 1초에 한 번 rate-limited로 알린다 */
    const noteDropped = (type: string): void => {
      const client = ctx.client
      client.conn.dropped++
      if (client.conn.dropped >= limits.maxDropped) {
        console.warn(`[WS] ${ctx.id.slice(0, 8)} exceeded rate limit repeatedly - closing`)
        ws.close(1008, 'Rate limit exceeded')
        return
      }
      const now = Date.now()
      if (now - client.conn.lastRateLimitNotice > 1000) {
        client.conn.lastRateLimitNotice = now
        send(ws, { type: 'error', code: 'rate-limited', message: '메시지를 너무 많이 보냈습니다.', rejectedType: type })
      }
    }

    /**
     * 처리하지 않는 프레임(바이너리·깨진 JSON·type 없음)도 일반 버킷을 쓴다. 전에는 rate limit 앞에서 조용히 버려
     * 한 연결이 64KB 깨진 JSON을 초당 1500개씩 보내도 끊기지 않고 이벤트 루프를 수백 ms씩 막았다(서버 보안 점검).
     */
    const ignoreFrame = (): void => {
      const client = ctx.client
      if (client.conn.msgBucket.take()) return
      client.conn.dropped++
      if (client.conn.dropped >= limits.maxDropped) {
        console.warn(`[WS] ${ctx.id.slice(0, 8)} flooded invalid frames - closing`)
        ws.close(1008, 'Rate limit exceeded')
      }
    }

    ws.on('message', (raw: RawData, isBinary: boolean) => {
      if (ws.readyState !== ws.OPEN) return // 닫는 중(속도 제한 1008 등): 상대가 close를 무시하고 계속 보내도 파싱하지 않는다
      if (isBinary) return ignoreFrame()
      let msg: Msg
      try {
        const parsed: unknown = JSON.parse(raw.toString())
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return ignoreFrame()
        if (typeof (parsed as { type?: unknown }).type !== 'string') return ignoreFrame()
        msg = parsed as Msg
      } catch {
        return ignoreFrame() // JSON 파싱 실패는 무시(속도 제한에는 센다)
      }

      const relay = RELAY_MESSAGE_TYPES.has(msg.type)
      const bytes = rawBytes(raw)
      // 중계 원문 크기 상한(#215 리뷰 H1): 본문 상한(16KB)을 넘는 중계는 어차피 버려지지만, 줄에 서면 체인이 풀릴 때까지 객체째 남는다
      if (relay && bytes > MAX_RELAY_MESSAGE_BYTES) return noteDropped(msg.type)

      if (!allowMessage(msg)) return

      // 관람 중계는 처리 대기 상한(MAX_PENDING_MESSAGES)에 세지 않는다: 방송 담당이 관람자 여럿에게 offer·ICE를 한꺼번에 보내면
      // 한 번 읽기에 100개를 넘어 1008로 끊겼다(서버 부하 발굴). 대신 중계 전용 대기 상한(개수·원문 바이트, #215 리뷰 H1)을 쓰고,
      // 넘으면 끊지 않고 버린다(속도 제한 버림으로 세어 maxDropped면 1008). 순서는 그대로 같은 체인에서 지킨다
      if (relay && (ctx.client.conn.pendingRelay >= MAX_PENDING_RELAY_MESSAGES || ctx.client.conn.pendingRelayBytes + bytes > MAX_PENDING_RELAY_BYTES)) {
        return noteDropped(msg.type)
      }
      const counted = !relay
      if (counted && ctx.client.conn.pending >= MAX_PENDING_MESSAGES) {
        console.warn(`[WS] ${ctx.id.slice(0, 8)} too many pending messages - closing`)
        ws.close(1008, 'Too many pending messages')
        return
      }
      if (counted) ctx.client.conn.pending++
      else {
        ctx.client.conn.pendingRelay++
        ctx.client.conn.pendingRelayBytes += bytes
      }
      // 메시지를 순차 처리 (auth 검증/DB 조회 같은 비동기 작업 중에도 순서 유지).
      // 콜백 안의 ctx.client·ctx.id는 실행 시점 값이다(원래 클로저 변수와 같음: 재개 뒤면 옛 Client)
      // 세션 재개(P1-16)로 Client가 새 연결로 넘어갔으면 옛 연결에 남은 메시지는 처리하지 않는다(conn.ws가 이 소켓이 아님)
      ctx.client.conn.chain = ctx.client.conn.chain
        .then(() => (getClient(ctx.id) === ctx.client && ctx.client.conn.ws === ws ? dispatch(ctx, msg) : undefined))
        .catch(err => console.error('[ERROR] Failed to handle message:', err))
        .finally(() => {
          if (ctx.client.conn.ws !== ws) return
          if (counted) ctx.client.conn.pending--
          else {
            ctx.client.conn.pendingRelay--
            ctx.client.conn.pendingRelayBytes -= bytes
          }
        }) // 넘어간 뒤의 옛 연결 몫은 새 연결 카운터에서 빼지 않는다
    })

    // 소켓 에러(예: maxPayload 초과 1009)를 처리하지 않으면 프로세스가 죽는다
    ws.on('error', (err) => {
      console.warn(`[WS] ${ctx.id.slice(0, 8)} socket error: ${err.message}`)
    })

    ws.on('close', () => {
      const client = ctx.client
      const id = ctx.id
      // 세션 재개(P1-16)가 이 Client를 새 연결로 가져갔다: 옛 소켓의 close는 새 연결의 타이머·유예·퇴장을 건드리지 않는다
      const takenOver = client.conn.ws !== ws
      clearTimeout(authTimer)
      if (!takenOver && client.auth.expiryTimer) clearTimeout(client.auth.expiryTimer)
      const n = (connectionsPerIp.get(ipKey) || 1) - 1
      if (n <= 0) connectionsPerIp.delete(ipKey)
      else connectionsPerIp.set(ipKey, n)

      // 이미 다른 연결로 대체/정리된 Client (중복 연결 제거, 세션 재개로 넘어감 등)
      if (takenOver || getClient(id) !== client) return

      const roomId = client.member.roomId
      console.log(`[DISCONNECT] Client ${id.slice(0, 8)} disconnected. roomId: ${roomId?.slice(0, 8) || 'none'}`)

      // 인증된 사용자가 방 안에서 끊김: 유예시간 동안 상태 유지 (참가자에게 leave 안 보냄)
      if (suspendOnClose(svc, client)) return

      if (roomId && hasRoom(roomId)) handleLeave(svc, client, roomId, 'close')
      client.member.roomId = undefined
      removeClient(id)
    })
  })

  return wss
}
