/**
 * 가짜 시그널링 서버 (server/src/services/wsService.ts의 메시지 순서를 따른다).
 *   연결 → welcome{clientId} → (클라이언트 auth) → auth-result{authenticated}
 *   join → 비밀번호 방이면 password-required / password-invalid{remainingAttempts} / password-rate-limited{retryAfterMs}
 *        → 정원(rooms.max_participants, 관람 포함, 서버 상한 20)이 찼으면 room-full{maxParticipants} (방장은 예외, P3-12b)
 *        → 통과하면 room-state{participants, peerIds, isHost}
 *   chat → 기록만(서버는 보낸 사람에게 되돌려주지 않는다). 원격 채팅은 pushChat으로 주입
 * 방장 판정은 서버처럼 토큰의 사용자와 DB(rooms.host_id)로 한다.
 *
 * 비밀번호 실패 제한은 서버 코드(server/src/lib/roomPassword.ts의 PasswordAttemptLimiter와 운영 기본값)를
 * 그대로 import해 쓴다 — mock이 서버와 다른 시점에 잠그지 않게(리뷰 #19 M1).
 *   서버 의미: 시도를 먼저 세고 remaining = max - count. 5번째 오답은 password-invalid{remainingAttempts: 0},
 *   6번째 시도에서 password-rate-limited.
 * 처리하지 않는 메시지 타입은 위반으로 기록한다(앱이 새 메시지를 보내기 시작하면 테스트가 알게).
 */
import type { BrowserContext, WebSocketRoute } from '@playwright/test'
import {
  DEFAULT_PASSWORD_ATTEMPT_LIMITS,
  PasswordAttemptLimiter,
} from '../../../server/src/lib/roomPassword'
import { SIGNALING_URL } from '../env'
import type { Violations } from './http'
import type { MemoryPostgrest } from './postgrest'
import { userFromToken } from './supabase'

export interface Participant {
  oderId: string
  nickname: string
  instrument?: string
  isPerforming?: boolean
  isHost?: boolean
}

export interface FakeRoomConfig {
  password?: string
  /** (방, 사용자) 비밀번호 실패 허용 횟수 — 기본은 서버 운영값 */
  maxAttempts?: number
  participants?: Participant[]
}

export type SignalMessage = Record<string, unknown> & { type: string }

/** 서버 운영값과 같은 잠금 시간 */
export const RATE_LIMIT_RETRY_MS = DEFAULT_PASSWORD_ATTEMPT_LIMITS.windowMs

/** 이 mock이 처리하는 클라이언트 → 서버 메시지 */
const HANDLED_TYPES = new Set(['auth', 'join', 'leave', 'chat', 'ping'])

/** 브라우저 연결의 가짜 IP (서버는 익명 사용자를 IP 단위로 센다) */
const FAKE_IP = '203.0.113.10'

interface Conn {
  ws: WebSocketRoute
  clientId: string
  userId: string | null
  roomId: string | null
}

export class FakeSignaling {
  /** 클라이언트가 보낸 메시지 전부 (도착 순) */
  readonly received: SignalMessage[] = []
  private conns: Conn[] = []
  private rooms = new Map<string, FakeRoomConfig>()
  private limiters = new Map<string, PasswordAttemptLimiter>()
  private seq = 0

  constructor(private readonly db: MemoryPostgrest, private readonly violations: Violations) {}

  setRoom(roomId: string, config: FakeRoomConfig) {
    this.rooms.set(roomId, config)
    this.limiters.delete(roomId)
  }

  /** type이 같은 받은 메시지 */
  messages(type: string): SignalMessage[] {
    return this.received.filter(m => m.type === type)
  }

  /** 원격 참가자의 채팅을 그 방의 모든 연결에 보낸다 (서버가 oderId/from을 붙이는 형식) */
  pushChat(roomId: string, from: Participant, message: string) {
    for (const c of this.conns) {
      if (c.roomId !== roomId) continue
      this.send(c, { type: 'chat', message, nickname: from.nickname, timestamp: Date.now(), oderId: from.oderId, from: from.oderId })
    }
  }

  async install(context: BrowserContext) {
    const host = new URL(SIGNALING_URL).host
    await context.routeWebSocket(url => url.host === host, ws => this.accept(ws))
  }

  private send(c: Conn, msg: SignalMessage) {
    c.ws.send(JSON.stringify(msg))
  }

  private limiter(roomId: string): PasswordAttemptLimiter {
    let l = this.limiters.get(roomId)
    if (!l) {
      const max = this.rooms.get(roomId)?.maxAttempts
      l = new PasswordAttemptLimiter(max === undefined ? DEFAULT_PASSWORD_ATTEMPT_LIMITS : { ...DEFAULT_PASSWORD_ATTEMPT_LIMITS, maxFailures: max })
      this.limiters.set(roomId, l)
    }
    return l
  }

  private accept(ws: WebSocketRoute) {
    const conn: Conn = { ws, clientId: `e2e-client-${++this.seq}`, userId: null, roomId: null }
    this.conns.push(conn)
    ws.onClose(() => {
      this.conns = this.conns.filter(c => c !== conn)
    })
    this.send(conn, { type: 'welcome', clientId: conn.clientId })
    ws.onMessage(raw => {
      if (typeof raw !== 'string') return
      let msg: SignalMessage
      try {
        msg = JSON.parse(raw)
      } catch {
        return
      }
      this.received.push(msg)
      this.handle(conn, msg)
    })
  }

  private handle(conn: Conn, msg: SignalMessage) {
    if (!HANDLED_TYPES.has(msg.type)) {
      this.violations.push(`시그널링 mock 미처리 메시지: ${msg.type}`)
      return
    }
    switch (msg.type) {
      case 'auth': {
        const user = userFromToken(typeof msg.accessToken === 'string' ? msg.accessToken : undefined)
        conn.userId = user?.id ?? null
        this.send(conn, { type: 'auth-result', authenticated: user !== null, userId: conn.userId })
        return
      }
      // 수신 없음 감지용 앱 수준 ping (P1-15, 서버 wsService와 같은 응답)
      case 'ping':
        this.send(conn, { type: 'pong' })
        return
      case 'join':
        return this.join(conn, msg)
      case 'leave':
        conn.roomId = null
        return
      default:
        // chat: 기록만 (received)
        return
    }
  }

  /** wsService.checkRoomPassword와 같은 순서·응답 */
  private checkPassword(conn: Conn, roomId: string, expected: string, password: unknown): boolean {
    if (typeof password !== 'string' || password.length === 0) {
      this.send(conn, { type: 'error', code: 'password-required', roomId, message: '비밀번호가 필요한 방입니다.' })
      return false
    }
    const subject = conn.userId ? `u:${conn.userId}` : `anon:${FAKE_IP}`
    const limiter = this.limiter(roomId)
    const attempt = limiter.begin(roomId, subject, FAKE_IP)
    if (!attempt.ok) {
      this.send(conn, { type: 'error', code: 'password-rate-limited', roomId, retryAfterMs: attempt.retryAfterMs, message: '비밀번호를 너무 많이 틀렸습니다. 잠시 후 다시 시도하세요.' })
      return false
    }
    // 서버는 NFC 정규화 후 해시 비교
    if (password.normalize('NFC') === expected.normalize('NFC')) {
      limiter.succeeded(roomId, subject, FAKE_IP)
      return true
    }
    this.send(conn, { type: 'error', code: 'password-invalid', roomId, remainingAttempts: attempt.remaining, message: '비밀번호가 올바르지 않습니다.' })
    return false
  }

  private join(conn: Conn, msg: SignalMessage) {
    const roomId = String(msg.roomId)
    const room = this.db.table('rooms').find(r => r.id === roomId)
    const config = this.rooms.get(roomId) ?? {}
    const isHost = conn.userId !== null && room?.host_id === conn.userId

    if (config.password && !isHost && !this.checkPassword(conn, roomId, config.password, msg.password)) return

    const participants = config.participants ?? []
    const max = typeof room?.max_participants === 'number' ? Math.min(room.max_participants, 20) : 20
    if (!isHost && participants.length >= max) {
      this.send(conn, { type: 'error', code: 'room-full', roomId, maxParticipants: max, message: `합주실 정원(${max}명)이 가득 찼습니다.` })
      return
    }

    conn.roomId = roomId
    this.send(conn, {
      type: 'room-state',
      participants,
      peerIds: participants.map(p => p.oderId),
      isHost,
    })
  }
}
