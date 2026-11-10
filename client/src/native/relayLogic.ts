/**
 * UDP 릴레이 연동 (relay-design.md 1·3.1절, P1-4b) — React/Tauri 비의존 로직.
 *
 * - 서버 `relay-token { token, addr, ssrc, expiresAt }` → 내 엔진 ssrc와 같을 때만 engine_set_relay.
 * - 만료 30초 전에 `relay-token-request`로 갱신(엔진은 같은 주소면 등록을 유지한 채 토큰만 바꾼다).
 * - 경로 표시: 'relay'면 "서버 경유 — 지연 증가". 'failed' 안내는 릴레이까지 실패했을 때의 문구.
 */
import type { PeerPath, PeerState, RelayConfig, RelayStats } from './engineApi'

export interface RelayTokenMessage extends RelayConfig {
  ssrc: number
  /** unix 초 */
  expiresAt: number
}

/** 만료 몇 초 전에 갱신을 요청할지 (relay-design.md 2.4) */
export const RELAY_REFRESH_BEFORE_SEC = 30
/** 갱신 요청 최소 간격(시계가 어긋나 이미 임박한 토큰이 와도 폭주하지 않게) */
export const RELAY_REFRESH_MIN_MS = 5_000

const BASE64URL = /^[A-Za-z0-9_-]+={0,2}$/

/** 서버 relay-token 메시지 검증. 형식이 틀리면 null */
export function parseRelayToken(payload: Record<string, unknown>): RelayTokenMessage | null {
  const { token, addr, ssrc, expiresAt } = payload
  if (typeof token !== 'string' || token.length < 60 || token.length > 400 || !BASE64URL.test(token)) return null
  if (typeof addr !== 'string' || addr.length === 0 || addr.length > 255 || !/:\d{1,5}$/.test(addr)) return null
  if (typeof ssrc !== 'number' || !Number.isInteger(ssrc) || ssrc < 0 || ssrc > 0xffffffff) return null
  if (typeof expiresAt !== 'number' || !Number.isFinite(expiresAt) || expiresAt <= 0) return null
  return { token, addr, ssrc, expiresAt }
}

/** 지금(nowMs)부터 갱신 요청까지 기다릴 시간(ms) */
export function relayRefreshDelayMs(expiresAt: number, nowMs: number): number {
  return Math.max(RELAY_REFRESH_MIN_MS, (expiresAt - RELAY_REFRESH_BEFORE_SEC) * 1000 - nowMs)
}

export function pathLabel(path: PeerPath | null | undefined): string | null {
  switch (path) {
    case 'lan':
      return 'LAN'
    case 'public':
      return '인터넷'
    case 'relay':
      return '릴레이'
    default:
      return null
  }
}

/** 경로가 릴레이일 때 보여줄 안내 */
export const RELAY_PATH_HINT = '서버 경유 — 지연 증가'

/**
 * 'failed' 상태 안내. 엔진은 직접 경로가 안 되면 릴레이로 폴백하므로, failed는 릴레이까지 실패했다는 뜻이다.
 * 릴레이가 설정되지 않았거나(서버에 릴레이 없음) 등록이 안 됐으면 그 사실을 알려 준다.
 */
export function failedPeerHint(relay: RelayStats | null | undefined): string {
  if (!relay) return '직접 연결 불가 — 네트워크(대칭형 NAT) 문제, 릴레이 서버 없음'
  switch (relay.state) {
    case 'bound':
      return '연결 불가 — 직접 연결과 릴레이 모두 실패 (상대 네트워크 확인)'
    case 'expired':
      return '연결 불가 — 릴레이 인증 만료 (연주를 다시 시작해 주세요)'
    case 'rejected':
      return '연결 불가 — 릴레이 서버가 연결을 거절했습니다'
    default:
      return '연결 불가 — 직접 연결 실패, 릴레이 서버에 닿지 않음'
  }
}

/** 피어 배지 보조 문구: 'failed'면 실패 안내, 릴레이 경로면 지연 안내, 그 밖에는 null */
export function peerPathHint(state: PeerState, path: PeerPath | null | undefined, relay: RelayStats | null | undefined): string | null {
  if (state === 'failed') return failedPeerHint(relay)
  if (path === 'relay') return RELAY_PATH_HINT
  return null
}
