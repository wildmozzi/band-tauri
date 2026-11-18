import { createHmac, timingSafeEqual } from 'crypto'

/**
 * UDP 릴레이 토큰 (계약: docs/overhaul/relay-design.md 3절, 검증 구현: relay/src/token.rs)
 *
 * ```
 * off  len  필드 (빅엔디안)
 * 0    1    version = 1
 * 1    8    expires_at (unix 초)
 * 9    4    ssrc (u32)
 * 13   1    room_len (1..=128)
 * 14   n    room_id (UTF-8)
 * ..   1    client_len (1..=64)
 * ..   m    member_id (UTF-8, 서버가 검증한 userId)
 * ..   32   HMAC-SHA256(RELAY_SECRET, 앞의 모든 바이트)
 * ```
 * WS로는 base64url 문자열로 전달하고, 클라이언트는 디코드한 바이트를 RelayBind 패킷에 그대로 싣는다.
 */

export const RELAY_TOKEN_VERSION = 1
const MAC_BYTES = 32
const MAX_ROOM_BYTES = 128
const MAX_CLIENT_BYTES = 64
/**
 * 기본 토큰 수명. 클라이언트는 만료 60초 전에 `relay-token-request`로 갱신한다. 내리기·연주 중지·퇴장 때는 회수(RelayRevoke,
 * relay-design 2.5)로 바로 끊고, 회수가 유실되거나 릴레이가 재시작해 기록을 잊은 경우에만 이 수명이 상한이 된다
 */
export const DEFAULT_RELAY_TOKEN_TTL_SEC = 120
export const MIN_RELAY_SECRET_BYTES = 32

export interface RelayTokenClaims {
  roomId: string
  memberId: string
  ssrc: number
  /** unix 초 */
  expiresAt: number
}

export interface RelayConfig {
  secret: string
  /** 클라이언트에게 알려줄 릴레이 주소 (host:port) */
  addr: string
  ttlSec: number
}

export function isValidSsrc(v: unknown): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= 0xffffffff
}

export function signRelayToken(claims: RelayTokenClaims, secret: string): Buffer {
  const room = Buffer.from(claims.roomId, 'utf8')
  const client = Buffer.from(claims.memberId, 'utf8')
  if (room.length < 1 || room.length > MAX_ROOM_BYTES) throw new Error('roomId length out of range')
  if (client.length < 1 || client.length > MAX_CLIENT_BYTES) throw new Error('memberId length out of range')
  if (!isValidSsrc(claims.ssrc)) throw new Error('invalid ssrc')
  if (!Number.isInteger(claims.expiresAt) || claims.expiresAt < 0) throw new Error('invalid expiresAt')

  const body = Buffer.alloc(14 + room.length + 1 + client.length)
  body[0] = RELAY_TOKEN_VERSION
  body.writeBigUInt64BE(BigInt(claims.expiresAt), 1)
  body.writeUInt32BE(claims.ssrc, 9)
  body[13] = room.length
  room.copy(body, 14)
  body[14 + room.length] = client.length
  client.copy(body, 15 + room.length)
  const mac = createHmac('sha256', Buffer.from(secret, 'utf8')).update(body).digest()
  return Buffer.concat([body, mac])
}

/** Bind 키 파생 라벨 (relay-design 2.1.1, relay/src/token.rs BIND_KEY_LABEL) */
export const RELAY_BIND_KEY_LABEL = 'band-relay bind v1'

/**
 * 토큰별 Bind 키 = HMAC-SHA256(HMAC-SHA256(RELAY_SECRET, "band-relay bind v1"), 토큰 전체).
 * WS(TLS)로 토큰과 함께 그 연주자에게만 보낸다(`relay-token.bindKey`). 엔진은 RelayBind 끝에
 * HMAC(bindKey, Bind)[..16]을 붙이고, 릴레이는 비밀값으로 같은 키를 다시 계산해 검증한다.
 * 경로상 관찰자는 UDP로 오가는 토큰을 볼 수 있지만 이 키는 모르므로 남의 등록을 가로챌 수 없다(4.1).
 */
export function deriveRelayBindKey(token: Buffer, secret: string): Buffer {
  const root = createHmac('sha256', Buffer.from(secret, 'utf8')).update(RELAY_BIND_KEY_LABEL).digest()
  return createHmac('sha256', root).update(token).digest()
}

/** 회수 키 파생 라벨 (relay-design 2.5, relay/src/token.rs REVOKE_KEY_LABEL) */
export const RELAY_REVOKE_KEY_LABEL = 'band-relay revoke v1'
/** RelayRevoke 패킷 type (relay-design 2.5, relay/src/packet.rs TYPE_RELAY_REVOKE) */
export const RELAY_REVOKE_TYPE = 10

export interface RelayRevokeClaims {
  roomId: string
  memberId: string
  /** 이 멤버의 토큰 중 expires_at ≤ cutoff(unix 초)인 것을 모두 회수한다 */
  cutoff: number
}

function revokeKey(secret: string): Buffer {
  return createHmac('sha256', Buffer.from(secret, 'utf8')).update(RELAY_REVOKE_KEY_LABEL).digest()
}

/**
 * RelayRevoke(type 10) — 시그널링 서버 → 릴레이 UDP. 릴레이는 그 멤버의 등록 중 회수 대상 토큰으로 된 것을 바로 지우고
 * cutoff까지 같은 토큰의 Bind를 받지 않는다. 응답은 없다.
 * ```
 * magic "BN" | version 1 | type 10 | cutoff(u64) | room_len | room | member_len | member | HMAC-SHA256(K_revoke, 앞 전부)
 * K_revoke = HMAC-SHA256(RELAY_SECRET, "band-relay revoke v1")
 * ```
 * 키를 토큰 MAC(비밀값 그대로)과 나눠 토큰과 회수 메시지가 서로의 서명으로 쓰일 수 없다. 리플레이는 같은 토큰들을 다시
 * 회수할 뿐이다(회수 뒤 발급 토큰은 만료가 cutoff보다 늦다 — nativeSession).
 */
export function signRelayRevoke(claims: RelayRevokeClaims, secret: string): Buffer {
  const room = Buffer.from(claims.roomId, 'utf8')
  const member = Buffer.from(claims.memberId, 'utf8')
  if (room.length < 1 || room.length > MAX_ROOM_BYTES) throw new Error('roomId length out of range')
  if (member.length < 1 || member.length > MAX_CLIENT_BYTES) throw new Error('memberId length out of range')
  if (!Number.isInteger(claims.cutoff) || claims.cutoff < 0) throw new Error('invalid cutoff')
  const body = Buffer.alloc(12 + 1 + room.length + 1 + member.length)
  body[0] = 0x42
  body[1] = 0x4e
  body[2] = 1
  body[3] = RELAY_REVOKE_TYPE
  body.writeBigUInt64BE(BigInt(claims.cutoff), 4)
  body[12] = room.length
  room.copy(body, 13)
  body[13 + room.length] = member.length
  member.copy(body, 14 + room.length)
  return Buffer.concat([body, createHmac('sha256', revokeKey(secret)).update(body).digest()])
}

/** RelayRevoke 검증 (테스트용. 운영 검증은 릴레이 `token::verify_revoke`) */
export function verifyRelayRevoke(pkt: Buffer, secret: string): RelayRevokeClaims | null {
  if (pkt.length < 12 + 4 + MAC_BYTES || pkt[0] !== 0x42 || pkt[1] !== 0x4e || pkt[2] !== 1 || pkt[3] !== RELAY_REVOKE_TYPE) return null
  const bodyLen = pkt.length - MAC_BYTES
  const roomLen = pkt[12]
  if (roomLen < 1 || roomLen > MAX_ROOM_BYTES || 14 + roomLen > bodyLen) return null
  const memberLen = pkt[13 + roomLen]
  if (memberLen < 1 || memberLen > MAX_CLIENT_BYTES || 14 + roomLen + memberLen !== bodyLen) return null
  const mac = createHmac('sha256', revokeKey(secret)).update(pkt.subarray(0, bodyLen)).digest()
  if (!timingSafeEqual(mac, pkt.subarray(bodyLen))) return null
  return {
    roomId: pkt.subarray(13, 13 + roomLen).toString('utf8'),
    memberId: pkt.subarray(14 + roomLen, bodyLen).toString('utf8'),
    cutoff: Number(pkt.readBigUInt64BE(4)),
  }
}

/** 검증 (테스트/진단용. 운영 검증은 릴레이가 한다) */
export function verifyRelayToken(token: Buffer, secret: string, nowSec: number): RelayTokenClaims | null {
  if (token.length < 14 + 3 + MAC_BYTES || token[0] !== RELAY_TOKEN_VERSION) return null
  const bodyLen = token.length - MAC_BYTES
  const body = token.subarray(0, bodyLen)
  const roomLen = body[13]
  if (roomLen < 1 || roomLen > MAX_ROOM_BYTES || 15 + roomLen > bodyLen) return null
  const clientLen = body[14 + roomLen]
  if (clientLen < 1 || clientLen > MAX_CLIENT_BYTES || 15 + roomLen + clientLen !== bodyLen) return null
  const mac = createHmac('sha256', Buffer.from(secret, 'utf8')).update(body).digest()
  if (!timingSafeEqual(mac, token.subarray(bodyLen))) return null
  const expiresAt = Number(body.readBigUInt64BE(1))
  if (expiresAt <= nowSec) return null
  return {
    expiresAt,
    ssrc: body.readUInt32BE(9),
    roomId: body.subarray(14, 14 + roomLen).toString('utf8'),
    memberId: body.subarray(15 + roomLen, bodyLen).toString('utf8'),
  }
}

/** env에서 릴레이 설정. RELAY_SECRET(≥32바이트)과 RELAY_ADDR가 모두 있어야 활성화 */
export function relayConfigFromEnv(env: Record<string, string | undefined>): RelayConfig | null {
  const secret = env.RELAY_SECRET
  const addr = env.RELAY_ADDR?.trim()
  if (!secret && !addr) return null
  if (!secret || Buffer.byteLength(secret, 'utf8') < MIN_RELAY_SECRET_BYTES || !addr) {
    console.warn('[RELAY] RELAY_SECRET(32바이트 이상)과 RELAY_ADDR가 모두 필요합니다 - 릴레이 토큰 발급 끔')
    return null
  }
  const ttl = Number(env.RELAY_TOKEN_TTL_SEC)
  return { secret, addr, ttlSec: Number.isInteger(ttl) && ttl > 0 ? ttl : DEFAULT_RELAY_TOKEN_TTL_SEC }
}
