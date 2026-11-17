/**
 * 가짜 시그널링 서버 (server/src/services/wsService.ts의 메시지 순서를 따른다).
 *   연결 → welcome{clientId} → (클라이언트 auth) → auth-result{authenticated}
 *   join → 방 조회 실패(failNextRoomLookups)면 room-unavailable{retryAfterMs}, 방(rooms 행)이 없으면 room-not-found (서버 membership checkRoom)
 *        → 비밀번호 방이면 password-required / password-invalid{remainingAttempts} / password-rate-limited{retryAfterMs}
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
 * 관람 WebRTC(answer·ice-candidate)는 서버 중계 코드(server/src/signaling/viewerRelay.ts buildViewerSignal — 본문 형식·
 * UTF-8 크기 상한)를 그대로 써서 검사한다: 서버가 조용히 버릴 모양이면 위반이다(#184·#187 제한 아래에서 협상이 되는지).
 * offer는 연주자(start-performing을 보낸 연결)만 보낸다(서버가 연주자 아닌 쪽 offer를 버린다) — 관람자의 offer는 위반이다.
 *
 * 보내는 메시지는 서버의 송신 메시지 타입(server/src/signaling/protocol.ts ServerMessage)으로 검사한다:
 * 서버 계약에서 필드가 바뀌면 e2e tsc(npm run e2e:typecheck)가 실패한다(docs/server-architecture.md 3.3절).
 * 서버 쪽 사실은 server/test/fixtures/ws-transcript.json이 고정한다.
 */
import type { BrowserContext, WebSocketRoute } from '@playwright/test'
import {
  DEFAULT_PASSWORD_ATTEMPT_LIMITS,
  PasswordAttemptLimiter,
} from '../../../server/src/lib/roomPassword'
import type { ServerMessage } from '../../../server/src/signaling/protocol'
import { buildViewerSignal } from '../../../server/src/signaling/viewerRelay'
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
// 연주(데스크톱 앱, e2e/mocks/tauri.ts로 흉내 낼 때만 온다): 기록만 — 서버의 중계·응답은 흉내 내지 않는다
const HANDLED_TYPES = new Set(['auth', 'join', 'leave', 'chat', 'ping', 'start-performing', 'stop-performing', 'native-leave', 'native-descriptor', 'relay-token-request', 'metronome', 'demote-performer', 'offer', 'answer', 'ice-candidate'])

/** setDown이 연결을 끊을 때 쓰는 코드 (1012 Service Restart — 앱은 비정상 닫힘으로 보고 재연결한다) */
const SERVER_RESTART_CLOSE_CODE = 1012

/** 브라우저 연결의 가짜 IP (서버는 익명 사용자를 IP 단위로 센다) */
const FAKE_IP = '203.0.113.10'

interface Conn {
  ws: WebSocketRoute
  clientId: string
  userId: string | null
  roomId: string | null
  /** start-performing을 받았고 stop-performing은 아직 (room-state.isPerforming) */
  performing: boolean
}

export class FakeSignaling {
  /** 클라이언트가 보낸 메시지 전부 (도착 순) */
  readonly received: SignalMessage[] = []
  /**
   * 진단용 시간순 기록(P2-18f): 연결 수락·거부·닫힘, 받은 메시지(→), 보낸 메시지(←, error면 code), 테스트가 바꾼 mock 상태.
   * 실패한 테스트에 첨부된다(e2e/test.ts) — 비동기 순서(연결 닫힘·join 도착)가 얽힌 flaky를 trace 없이도 읽을 수 있게
   */
  private readonly events: string[] = []
  private readonly t0 = Date.now()
  private log(line: string) {
    this.events.push(`${String(Date.now() - this.t0).padStart(6)}ms ${line}`)
  }
  /** 진단용 시간순 기록 (한 줄에 하나) */
  timeline(): string {
    return this.events.join('\n')
  }
  private conns: Conn[] = []
  private rooms = new Map<string, FakeRoomConfig>()
  private limiters = new Map<string, PasswordAttemptLimiter>()
  private seq = 0
  /** 재개를 받아 줄 옛 clientId → 방·서버 기준 연주 상태 (dropForResume) */
  private resumable = new Map<string, { roomId: string; isPerforming: boolean }>()
  private down = false
  private lookupFailures = 0
  private lookupRetryAfterMs = 500
  /** down 동안 받자마자 닫은 연결 수 (앱의 재연결 시도) */
  refused = 0

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

  /** 그 방에 들어와 있는 연결의 clientId (관람 WebRTC offer의 to) */
  clientIdsIn(roomId: string): string[] {
    return this.conns.filter(c => c.roomId === roomId).map(c => c.clientId)
  }

  /**
   * 방 종료 흉내: rooms 행만 지운다(방장이 Supabase로 방 삭제). 실제 서버도 방 안 연결에 아무것도 보내지 않고 끊지도 않으므로,
   * 관람자는 다음 재연결의 rejoin join에서야 room-not-found를 받는다(재연결은 setDown 등으로 따로 일으킨다, #189 리뷰 L-3).
   */
  endRoom(roomId: string) {
    const rows = this.db.table('rooms')
    const i = rows.findIndex(r => r.id === roomId)
    if (i >= 0) rows.splice(i, 1)
  }

  /** 다음 join n번의 방 조회가 실패한다(DB 일시 장애) — 서버처럼 room-unavailable{retryAfterMs} (#189 리뷰 H-1) */
  failNextRoomLookups(n = 1, retryAfterMs = 500) {
    this.log(`[test] failNextRoomLookups(${n})`)
    this.lookupFailures = n
    this.lookupRetryAfterMs = retryAfterMs
  }

  /** 서버 송신 메시지 하나를 그 방의 모든 연결에 보낸다 (예: 방장이 나를 내렸을 때의 demoted-to-viewer, P2-9) */
  pushToRoom(roomId: string, msg: ServerMessage) {
    for (const c of this.conns) if (c.roomId === roomId) this.send(c, msg)
  }

  /**
   * 소켓만 끊긴 것처럼 지금 연결을 닫고, 같은 사용자의 다음 auth{resumeClientId}를 세션 재개로 받아 준다(서버 resume 흉내, P2-9 #180 M-1).
   * 재개 알림은 서버 순서대로 welcome{resumed} → auth-result{resumed} → room-state이고, room-state.isPerforming은 `isPerforming`
   * (예: 끊긴 사이 방장이 내렸는데 demoted-to-viewer가 유실된 경우 false).
   */
  dropForResume(isPerforming: boolean) {
    for (const c of this.conns) {
      if (c.roomId) this.resumable.set(c.clientId, { roomId: c.roomId, isPerforming })
      void c.ws.close({ code: SERVER_RESTART_CLOSE_CODE, reason: 'e2e drop' })
    }
    this.conns = []
  }

  /**
   * 서버 장애 흉내 (P1-15b 재연결 UX). down이면 지금 연결을 모두 비정상 코드로 닫고, 새 연결도 받자마자 닫는다.
   * up으로 되돌리면 다음 재연결부터 평소처럼 받는다(세션 재개는 흉내 내지 않는다 — 새 clientId + rejoin join → room-state).
   */
  setDown(down: boolean) {
    this.log(`[test] setDown(${down})`)
    this.down = down
    if (!down) return
    for (const c of this.conns) void c.ws.close({ code: SERVER_RESTART_CLOSE_CODE, reason: 'e2e down' })
    this.conns = []
  }

  async install(context: BrowserContext) {
    const host = new URL(SIGNALING_URL).host
    await context.routeWebSocket(url => url.host === host, ws => this.accept(ws))
  }

  private send(c: Conn, msg: ServerMessage) {
    this.log(`${c.clientId} ← ${msg.type}${msg.type === 'error' ? ` ${msg.code}` : ''}`)
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
    if (this.down) {
      this.refused++
      this.log('연결 거부(down)')
      void ws.close({ code: SERVER_RESTART_CLOSE_CODE, reason: 'e2e down' })
      return
    }
    const conn: Conn = { ws, clientId: `e2e-client-${++this.seq}`, userId: null, roomId: null, performing: false }
    this.conns.push(conn)
    this.log(`${conn.clientId} 연결`)
    ws.onClose(() => {
      this.log(`${conn.clientId} 닫힘`)
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
      this.log(`${conn.clientId} → ${msg.type}${msg.type === 'join' ? ` ${msg.isRejoin ? 'rejoin' : 'first'}` : ''}`)
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
        const resume = user && typeof msg.resumeClientId === 'string' ? this.resumable.get(msg.resumeClientId) : undefined
        if (resume && typeof msg.resumeClientId === 'string') {
          this.resumable.delete(msg.resumeClientId)
          conn.clientId = msg.resumeClientId
          conn.roomId = resume.roomId
          conn.performing = resume.isPerforming
          this.send(conn, { type: 'welcome', clientId: conn.clientId, resumed: true })
          this.send(conn, { type: 'auth-result', authenticated: true, userId: conn.userId, resumed: true })
          this.sendRoomState(conn, resume.roomId)
          return
        }
        this.send(conn, { type: 'auth-result', authenticated: user !== null, userId: conn.userId })
        return
      }
      case 'start-performing':
        conn.performing = true
        return
      case 'stop-performing':
        conn.performing = false
        return
      // 수신 없음 감지용 앱 수준 ping (P1-15, 서버 wsService와 같은 응답)
      case 'ping':
        this.send(conn, { type: 'pong' })
        return
      case 'join':
        return this.join(conn, msg)
      case 'leave':
        conn.roomId = null
        return
      case 'offer':
      case 'answer':
      case 'ice-candidate': {
        // 서버 중계와 같은 검사: 방 안에서, 서버가 다시 구성할 수 있는 모양이어야 하고, offer는 연주자만
        const to = typeof msg.to === 'string' ? msg.to : ''
        if (!conn.roomId || !to || !buildViewerSignal(msg, conn.clientId, to) || (msg.type === 'offer' && !conn.performing)) {
          this.violations.push(`서버가 버릴 관람 중계 메시지: ${msg.type} ${JSON.stringify(msg).slice(0, 200)}`)
        }
        return
      }
      default:
        // chat·연주 메시지: 기록만 (received)
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
    if (this.lookupFailures > 0) {
      this.lookupFailures--
      this.send(conn, { type: 'error', code: 'room-unavailable', roomId, retryAfterMs: this.lookupRetryAfterMs, message: '합주실 정보를 불러오지 못했습니다. 잠시 후 다시 시도합니다.' })
      return
    }
    const room = this.db.table('rooms').find(r => r.id === roomId)
    const config = this.rooms.get(roomId) ?? {}
    if (!room) {
      this.send(conn, { type: 'error', code: 'room-not-found', message: '방을 찾을 수 없습니다.' })
      return
    }
    const isHost = conn.userId !== null && room.host_id === conn.userId

    if (config.password && !isHost && !this.checkPassword(conn, roomId, config.password, msg.password)) return

    const participants = config.participants ?? []
    const max = typeof room.max_participants === 'number' ? Math.min(room.max_participants, 20) : 20
    if (!isHost && participants.length >= max) {
      this.send(conn, { type: 'error', code: 'room-full', reason: 'room-full', roomId, maxParticipants: max, message: `합주실 정원(${max}명)이 가득 찼습니다.` })
      return
    }

    conn.roomId = roomId
    this.sendRoomState(conn, roomId)
  }

  private sendRoomState(conn: Conn, roomId: string) {
    const room = this.db.table('rooms').find(r => r.id === roomId)
    const isHost = conn.userId !== null && room?.host_id === conn.userId
    const participants = this.rooms.get(roomId)?.participants ?? []
    this.send(conn, {
      type: 'room-state',
      // 서버는 isPerforming·isHost를 항상 채워 보낸다
      participants: participants.map(p => ({ isPerforming: false, isHost: false, ...p })),
      peerIds: participants.map(p => p.oderId),
      isHost,
      isPerforming: conn.performing,
    })
  }
}
