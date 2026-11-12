/** descriptor 검증(순수): 엔진 공개키, 후보 주소. 설계: docs/overhaul/native-engine-design.md 5절 */
import type { EngineCandidate } from './protocol'

/** X25519 공개키 길이(바이트). descriptor.key는 이것의 표준 base64(패딩 포함 44자) */
const ENGINE_KEY_BYTES = 32
const ENGINE_KEY_B64_RE = /^[A-Za-z0-9+/]{43}=$/
/** descriptor 후보 개수 상한(엔진 net/peer.rs MAX_CANDIDATES와 같음) */
export const MAX_ENGINE_CANDIDATES = 8
/** 후보 주소 형식: IPv4 `a.b.c.d:port`만(엔진 소켓은 IPv4, 받는 쪽 엔진도 IPv6를 거부한다) */
const CANDIDATE_ADDR_RE = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3}):(\d{1,5})$/
const CANDIDATE_ADDR_MAX = 21 // '255.255.255.255:65535'

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
