import { isIPv6 } from 'net'
import { randomBytes, scrypt as scryptCb, timingSafeEqual, type ScryptOptions } from 'crypto'

/**
 * 방 비밀번호 해시 (scrypt, Node 내장 — 네이티브 의존성 없음).
 * 형식: `scrypt$<N>$<r>$<p>$<salt base64url>$<hash base64url>` — 파라미터를 함께 저장해 나중에 강도를 바꿔도 기존 해시를 검증할 수 있다.
 * 평문은 저장·로그하지 않는다. 입력은 NFC로 정규화한다(한글 NFC/NFD 입력 차이).
 *
 * 파라미터 (PR #9 리뷰 H1): N=2^14, r=8, p=5
 * - OWASP Password Storage Cheat Sheet의 scrypt 동등 설정 목록 중 하나(N=2^17·p=1 … N=2^14·p=5 … N=2^13·p=10).
 * - 메모리는 128·N·r = 16MiB/회(p는 순차 계산이라 메모리를 늘리지 않음), 계산 ~125ms.
 * - 동시 실행은 ScryptGate로 2개까지 → 추가 메모리 최대 32MiB (256MB fly VM에서 안전).
 */

export const ROOM_PASSWORD_MIN = 6
export const ROOM_PASSWORD_MAX = 64

export const SCRYPT_PARAMS = { N: 1 << 14, r: 8, p: 5 }
const KEY_LEN = 32
const SALT_LEN = 16
/** 저장된 해시가 조작돼도 쓰는 메모리 상한 (128·N·r ≤ 32MiB) */
export const MAX_SCRYPT_MEMORY = 32 * 1024 * 1024

/** scrypt 1회 메모리 (바이트) */
export function scryptMemory(N: number, r: number): number {
  return 128 * N * r
}

function scrypt(password: string, salt: Buffer, keylen: number, opts: ScryptOptions & { N: number; r: number }): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    // maxmem은 실제 필요량보다 약간 크게(OpenSSL 내부 버퍼) — 그 이상은 OpenSSL이 거부
    scryptCb(password, salt, keylen, { ...opts, maxmem: scryptMemory(opts.N, opts.r) + 1024 * 1024 }, (err, key) => (err ? reject(err) : resolve(key)))
  })
}

/** 서버가 비교하는 형태 (NFC) */
export function normalizeRoomPassword(password: string): string {
  return password.normalize('NFC')
}

/** 비밀번호 형식 검증. 문제가 있으면 사용자에게 보일 메시지, 없으면 null */
export function validateRoomPassword(password: unknown): string | null {
  if (typeof password !== 'string') return '비밀번호는 문자열이어야 합니다.'
  const pw = normalizeRoomPassword(password)
  if (pw.length < ROOM_PASSWORD_MIN) return `비밀번호는 ${ROOM_PASSWORD_MIN}자 이상이어야 합니다.`
  if (pw.length > ROOM_PASSWORD_MAX) return `비밀번호는 ${ROOM_PASSWORD_MAX}자 이하여야 합니다.`
  return null
}

// ---------------------------------------------------------------- 동시 실행 제한

/** 대기열이 가득 차 scrypt를 실행하지 않고 거부함 */
export class ScryptBusyError extends Error {
  constructor() {
    super('password hashing is busy')
    this.name = 'ScryptBusyError'
  }
}

/**
 * scrypt 동시 실행 세마포어 (H1). maxConcurrent개까지 실행, maxQueue개까지 대기, 그 이상은 즉시 ScryptBusyError.
 * libuv 스레드풀(기본 4)을 scrypt가 다 차지하지 않게 해 DNS/fs 작업도 보호한다.
 */
export class ScryptGate {
  private active = 0
  private queue: (() => void)[] = []
  /** 테스트/관측용: 동시에 실행된 최대 개수 */
  peak = 0

  constructor(readonly maxConcurrent = 2, readonly maxQueue = 16) {}

  async run<T>(fn: () => Promise<T>): Promise<T> {
    if (this.active >= this.maxConcurrent) {
      if (this.queue.length >= this.maxQueue) throw new ScryptBusyError()
      await new Promise<void>((resolve) => this.queue.push(resolve))
    } else {
      this.active++
    }
    this.peak = Math.max(this.peak, this.active)
    try {
      return await fn()
    } finally {
      const next = this.queue.shift()
      if (next) next() // 슬롯을 그대로 넘긴다 (active 유지)
      else this.active--
    }
  }

  get pending(): number {
    return this.queue.length
  }

  get running(): number {
    return this.active
  }
}

/** 프로세스 전체 공용 게이트 (WS 검증 + HTTP 해시 생성) */
export const defaultScryptGate = new ScryptGate(2, 16)

export async function hashRoomPassword(password: string, params = SCRYPT_PARAMS, gate: ScryptGate = defaultScryptGate): Promise<string> {
  const salt = randomBytes(SALT_LEN)
  const key = await gate.run(() => scrypt(normalizeRoomPassword(password), salt, KEY_LEN, params))
  return ['scrypt', params.N, params.r, params.p, salt.toString('base64url'), key.toString('base64url')].join('$')
}

/** 저장 형식 파싱. 허용 범위 밖이면 null */
function parseHash(stored: string) {
  const parts = stored.split('$')
  if (parts.length !== 6 || parts[0] !== 'scrypt') return null
  const N = Number(parts[1])
  const r = Number(parts[2])
  const p = Number(parts[3])
  if (![N, r, p].every(Number.isInteger) || N < 2 || (N & (N - 1)) !== 0 || r < 1 || r > 32 || p < 1 || p > 16) return null
  if (scryptMemory(N, r) > MAX_SCRYPT_MEMORY) return null
  const salt = Buffer.from(parts[4], 'base64url')
  const expected = Buffer.from(parts[5], 'base64url')
  if (salt.length === 0 || expected.length === 0 || expected.length > 64) return null
  return { N, r, p, salt, expected }
}

/**
 * 해시와 비교. 형식이 잘못된 해시는 false (예외 없음).
 * 게이트가 가득 차면 ScryptBusyError를 던진다(호출자가 "잠시 후 다시" 처리).
 */
export async function verifyRoomPassword(password: string, stored: string, gate: ScryptGate = defaultScryptGate): Promise<boolean> {
  if (typeof password !== 'string' || password.length === 0 || password.length > ROOM_PASSWORD_MAX * 4) return false
  const parsed = parseHash(stored)
  if (!parsed) return false
  const { N, r, p, salt, expected } = parsed
  let key: Buffer
  try {
    key = await gate.run(() => scrypt(normalizeRoomPassword(password), salt, expected.length, { N, r, p }))
  } catch (err) {
    if (err instanceof ScryptBusyError) throw err
    return false
  }
  return timingSafeEqual(key, expected)
}

// ---------------------------------------------------------------- 실패 횟수 제한

/** IP를 제한 단위로: IPv4는 그대로, IPv6는 /64 (한 사용자가 /64를 통째로 받는 경우가 많다) */
export function ipBucket(ip: string): string {
  const v4mapped = ip.match(/^::ffff:(\d+\.\d+\.\d+\.\d+)$/i)
  if (v4mapped) return v4mapped[1]
  const bare = ip.split('%')[0]
  if (!isIPv6(bare)) return ip
  const [head, tail = ''] = bare.split('::')
  const h = head ? head.split(':') : []
  const t = tail ? tail.split(':') : []
  // 끝에 IPv4 표기가 붙은 경우는 하위 64비트라 /64 계산에 영향 없음
  const groups = bare.includes('::') ? [...h, ...Array(8 - h.length - t.length).fill('0'), ...t] : h
  return groups.slice(0, 4).map(g => parseInt(g || '0', 16).toString(16)).join(':') + '::/64'
}

export interface AttemptLimits {
  /** (방, 사용자) 단위 허용 실패 횟수 */
  maxFailures: number
  /** (방, IP 또는 IPv6 /64) 단위 허용 실패 횟수 — 여러 계정으로 같은 네트워크에서 대입하는 경우 */
  maxFailuresPerIp: number
  /** (방) 전체 허용 실패 횟수 — 여러 네트워크에서 분산 대입하는 경우. 넘으면 방장 외 새 입장이 창 동안 막힌다 */
  maxFailuresPerRoom: number
  /** 실패 기록 유지 시간. 첫 실패부터 이 시간이 지나면 초기화 */
  windowMs: number
}

/** 운영 기본값 (wsService가 쓰고, 웹 E2E의 시그널링 mock도 같은 값을 import한다) */
export const DEFAULT_PASSWORD_ATTEMPT_LIMITS: AttemptLimits = {
  maxFailures: 5,
  maxFailuresPerIp: 20,
  maxFailuresPerRoom: 100,
  windowMs: 15 * 60_000,
}

interface Entry {
  count: number
  resetAt: number
}

export type AttemptCheck =
  | { ok: true; remaining: number }
  | { ok: false; retryAfterMs: number; scope: 'user' | 'ip' | 'room' }

/**
 * 방 비밀번호 실패 횟수 제한 (메모리, 서버 인스턴스별). 3단: (방, 사용자) · (방, IP/64) · (방 전체).
 * 검증(scrypt) 전에 시도를 먼저 기록하고, 성공하거나 검증을 못 했으면(게이트 포화) 되돌린다.
 */
export class PasswordAttemptLimiter {
  private entries = new Map<string, Entry>()

  constructor(private limits: AttemptLimits, private now: () => number = Date.now) {}

  private get(key: string): Entry | null {
    const e = this.entries.get(key)
    if (!e) return null
    if (e.resetAt <= this.now()) {
      this.entries.delete(key)
      return null
    }
    return e
  }

  private keys(roomId: string, subject: string, ip: string) {
    return { user: `${roomId}|${subject}`, ip: `${roomId}|ip:${ipBucket(ip)}`, room: `${roomId}|room` }
  }

  /** 시도 가능 여부. 가능하면 시도 1회를 기록한다 */
  begin(roomId: string, subject: string, ip: string): AttemptCheck {
    const k = this.keys(roomId, subject, ip)
    const u = this.get(k.user)
    const i = this.get(k.ip)
    const r = this.get(k.room)
    const now = this.now()
    if (u && u.count >= this.limits.maxFailures) return { ok: false, retryAfterMs: u.resetAt - now, scope: 'user' }
    if (i && i.count >= this.limits.maxFailuresPerIp) return { ok: false, retryAfterMs: i.resetAt - now, scope: 'ip' }
    if (r && r.count >= this.limits.maxFailuresPerRoom) return { ok: false, retryAfterMs: r.resetAt - now, scope: 'room' }
    this.bump(k.user, u)
    this.bump(k.ip, i)
    this.bump(k.room, r)
    this.prune()
    const left = (key: string, max: number) => max - this.entries.get(key)!.count
    return {
      ok: true,
      remaining: Math.max(0, Math.min(left(k.user, this.limits.maxFailures), left(k.ip, this.limits.maxFailuresPerIp), left(k.room, this.limits.maxFailuresPerRoom))),
    }
  }

  /** 검증 성공: 이 사용자 기록은 지우고 IP·방 기록은 이번 시도분만 되돌린다 */
  succeeded(roomId: string, subject: string, ip: string) {
    const k = this.keys(roomId, subject, ip)
    this.entries.delete(k.user)
    this.refund(k.ip)
    this.refund(k.room)
  }

  /** 검증하지 못함(서버 혼잡): 이번 시도를 전부 되돌린다 */
  cancelled(roomId: string, subject: string, ip: string) {
    const k = this.keys(roomId, subject, ip)
    this.refund(k.user)
    this.refund(k.ip)
    this.refund(k.room)
  }

  private refund(key: string) {
    const e = this.get(key)
    if (!e) return
    e.count--
    if (e.count <= 0) this.entries.delete(key)
  }

  private bump(key: string, e: Entry | null) {
    if (e) e.count++
    else this.entries.set(key, { count: 1, resetAt: this.now() + this.limits.windowMs })
  }

  /** 오래된 기록 정리 (맵이 커질 때만) */
  private prune() {
    if (this.entries.size < 10_000) return
    const now = this.now()
    for (const [key, e] of this.entries) if (e.resetAt <= now) this.entries.delete(key)
  }
}
