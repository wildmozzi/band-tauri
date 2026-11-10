/**
 * 가짜 시그널링 서버 (server/src/services/wsService.ts의 메시지 순서를 따른다).
 *   연결 → welcome{clientId} → (클라이언트 auth) → auth-result{authenticated}
 *   join → 비밀번호 방이면 password-required / password-invalid{remainingAttempts} / password-rate-limited{retryAfterMs}
 *        → 통과하면 room-state{participants, peerIds, isHost}
 *   chat → 기록만(서버는 보낸 사람에게 되돌려주지 않는다). 원격 채팅은 pushChat으로 주입
 * 방장 판정은 서버처럼 토큰의 사용자와 DB(rooms.host_id)로 한다.
 */
import type { BrowserContext, WebSocketRoute } from '@playwright/test'
import { SIGNALING_URL } from '../env'
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
  /** 비밀번호 실패 허용 횟수 (서버 기본 5) */
  maxAttempts?: number
  participants?: Participant[]
}

export type SignalMessage = Record<string, unknown> & { type: string }

export const RATE_LIMIT_RETRY_MS = 15 * 60_000

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
  private failures = new Map<string, number>()
  private seq = 0

  constructor(private readonly db: MemoryPostgrest) {}

  setRoom(roomId: string, config: FakeRoomConfig) {
    this.rooms.set(roomId, config)
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
    switch (msg.type) {
      case 'auth': {
        const user = userFromToken(typeof msg.accessToken === 'string' ? msg.accessToken : undefined)
        conn.userId = user?.id ?? null
        this.send(conn, { type: 'auth-result', authenticated: user !== null, userId: conn.userId })
        return
      }
      case 'join':
        return this.join(conn, msg)
      case 'leave':
        conn.roomId = null
        return
      default:
        // chat 등: 기록만 (received)
        return
    }
  }

  private join(conn: Conn, msg: SignalMessage) {
    const roomId = String(msg.roomId)
    const room = this.db.table('rooms').find(r => r.id === roomId)
    const config = this.rooms.get(roomId) ?? {}
    const isHost = conn.userId !== null && room?.host_id === conn.userId

    if (config.password && !isHost) {
      const max = config.maxAttempts ?? 5
      const failed = this.failures.get(roomId) ?? 0
      if (failed >= max) {
        return this.send(conn, { type: 'error', code: 'password-rate-limited', roomId, retryAfterMs: RATE_LIMIT_RETRY_MS, message: '비밀번호를 너무 많이 틀렸습니다.' })
      }
      if (typeof msg.password !== 'string' || msg.password === '') {
        return this.send(conn, { type: 'error', code: 'password-required', roomId, message: '비밀번호가 필요한 방입니다.' })
      }
      if (msg.password !== config.password) {
        const now = failed + 1
        this.failures.set(roomId, now)
        if (now >= max) {
          return this.send(conn, { type: 'error', code: 'password-rate-limited', roomId, retryAfterMs: RATE_LIMIT_RETRY_MS, message: '비밀번호를 너무 많이 틀렸습니다.' })
        }
        return this.send(conn, { type: 'error', code: 'password-invalid', roomId, remainingAttempts: max - now, message: '비밀번호가 올바르지 않습니다.' })
      }
    }

    conn.roomId = roomId
    const participants = config.participants ?? []
    this.send(conn, {
      type: 'room-state',
      participants,
      peerIds: participants.map(p => p.oderId),
      isHost,
    })
  }
}
