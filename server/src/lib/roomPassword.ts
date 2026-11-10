import { randomBytes, scrypt as scryptCb, timingSafeEqual, type ScryptOptions } from 'crypto'

/**
 * 방 비밀번호 해시 (scrypt, Node 내장 — 네이티브 의존성 없음).
 * 형식: `scrypt$<N>$<r>$<p>$<salt base64url>$<hash base64url>` — 파라미터를 함께 저장해 나중에 강도를 올려도 기존 해시를 검증할 수 있다.
 * 평문은 저장·로그하지 않는다.
 */

export const ROOM_PASSWORD_MIN = 4
export const ROOM_PASSWORD_MAX = 64

const DEFAULT_PARAMS = { N: 1 << 15, r: 8, p: 1 }
const KEY_LEN = 32
const SALT_LEN = 16
/** N 상한 (저장된 해시가 조작돼도 메모리 폭주 방지) */
const MAX_N = 1 << 17

function scrypt(password: string, salt: Buffer, keylen: number, opts: ScryptOptions): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    scryptCb(password, salt, keylen, opts, (err, key) => (err ? reject(err) : resolve(key)))
  })
}

function maxmem(N: number, r: number): number {
  return 128 * N * r * 2
}

/** 비밀번호 형식 검증. 문제가 있으면 사용자에게 보일 메시지, 없으면 null */
export function validateRoomPassword(password: unknown): string | null {
  if (typeof password !== 'string') return '비밀번호는 문자열이어야 합니다.'
  if (password.length < ROOM_PASSWORD_MIN) return `비밀번호는 ${ROOM_PASSWORD_MIN}자 이상이어야 합니다.`
  if (password.length > ROOM_PASSWORD_MAX) return `비밀번호는 ${ROOM_PASSWORD_MAX}자 이하여야 합니다.`
  return null
}

export async function hashRoomPassword(password: string, params = DEFAULT_PARAMS): Promise<string> {
  const salt = randomBytes(SALT_LEN)
  const key = await scrypt(password, salt, KEY_LEN, { ...params, maxmem: maxmem(params.N, params.r) })
  return ['scrypt', params.N, params.r, params.p, salt.toString('base64url'), key.toString('base64url')].join('$')
}

/** 해시와 비교. 형식이 잘못된 해시는 false (예외 없음) */
export async function verifyRoomPassword(password: string, stored: string): Promise<boolean> {
  if (typeof password !== 'string' || password.length === 0 || password.length > ROOM_PASSWORD_MAX) return false
  const parts = stored.split('$')
  if (parts.length !== 6 || parts[0] !== 'scrypt') return false
  const N = Number(parts[1])
  const r = Number(parts[2])
  const p = Number(parts[3])
  if (![N, r, p].every(Number.isInteger) || N < 2 || N > MAX_N || (N & (N - 1)) !== 0 || r < 1 || r > 32 || p < 1 || p > 16) return false
  const salt = Buffer.from(parts[4], 'base64url')
  const expected = Buffer.from(parts[5], 'base64url')
  if (salt.length === 0 || expected.length === 0) return false
  try {
    const key = await scrypt(password, salt, expected.length, { N, r, p, maxmem: maxmem(N, r) })
    return timingSafeEqual(key, expected)
  } catch {
    return false
  }
}

export interface AttemptLimits {
  /** (방, 사용자) 단위 허용 실패 횟수 */
  maxFailures: number
  /** (방, IP) 단위 허용 실패 횟수 — 여러 계정으로 같은 IP에서 대입하는 경우 */
  maxFailuresPerIp: number
  /** 실패 기록 유지 시간. 첫 실패부터 이 시간이 지나면 초기화 */
  windowMs: number
}

interface Entry {
  count: number
  resetAt: number
}

export type AttemptCheck = { ok: true; remaining: number } | { ok: false; retryAfterMs: number }

/**
 * 방 비밀번호 실패 횟수 제한 (메모리, 서버 인스턴스별).
 * 검증(scrypt) 전에 시도를 먼저 기록하고 성공하면 되돌린다 — 동시에 여러 연결로 시도해도 한도를 넘지 못한다.
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
    return { user: `${roomId}|${subject}`, ip: `${roomId}|ip:${ip}` }
  }

  /** 시도 가능 여부. 가능하면 시도 1회를 기록한다 */
  begin(roomId: string, subject: string, ip: string): AttemptCheck {
    const k = this.keys(roomId, subject, ip)
    const u = this.get(k.user)
    const i = this.get(k.ip)
    const now = this.now()
    if (u && u.count >= this.limits.maxFailures) return { ok: false, retryAfterMs: u.resetAt - now }
    if (i && i.count >= this.limits.maxFailuresPerIp) return { ok: false, retryAfterMs: i.resetAt - now }
    this.bump(k.user, u)
    this.bump(k.ip, i)
    this.prune()
    const uc = this.entries.get(k.user)!.count
    const ic = this.entries.get(k.ip)!.count
    return { ok: true, remaining: Math.max(0, Math.min(this.limits.maxFailures - uc, this.limits.maxFailuresPerIp - ic)) }
  }

  /** 검증 성공: 이 사용자 기록은 지우고 IP 기록은 이번 시도분만 되돌린다 */
  succeeded(roomId: string, subject: string, ip: string) {
    const k = this.keys(roomId, subject, ip)
    this.entries.delete(k.user)
    const i = this.get(k.ip)
    if (i) {
      i.count--
      if (i.count <= 0) this.entries.delete(k.ip)
    }
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
