/**
 * UDP 릴레이 연동 (relay-design.md 1·3.1절, P1-4b) — React/Tauri 비의존 로직.
 *
 * - 서버 `relay-token { token, addr, ssrc, expiresAt, expiresIn, serverNow }` → 내 엔진 ssrc와 같을 때만 engine_set_relay.
 * - 만료·갱신은 내 PC 시계가 아니라 **받은 시각 + expiresIn**으로 판단한다(시계 오차, PR #10 M1).
 *   만료 60초 전(수명의 절반 이하)에 `relay-token-request`로 갱신한다 — WS가 잠시 끊겨도 버틸 여유(L4).
 * - 엔진이 릴레이를 `expired`/`unreachable`로 보고하면(토큰 거부 포함) 곧바로 새 토큰을 요청한다.
 * - 경로 표시: 'relay'면 "서버 경유 — 지연 증가". 'failed' 안내는 릴레이까지 실패했을 때의 문구.
 */
import type { PeerPath, PeerState, RelayConfig, RelayStats } from './engineApi'

export interface RelayTokenMessage extends RelayConfig {
  ssrc: number
  /** unix 초(서버 시계) */
  expiresAt: number
  /** 받은 시점 기준 남은 수명(초) */
  expiresIn: number
}

/** 만료 몇 초 전에 갱신을 요청할지(수명의 절반을 넘지 않게) */
export const RELAY_REFRESH_BEFORE_SEC = 60
/** 갱신 요청 최소 간격(이미 임박한 토큰이 와도 폭주하지 않게) */
export const RELAY_REFRESH_MIN_MS = 5_000
/** expiresIn 상한(이상값 방어) */
const MAX_EXPIRES_IN_SEC = 24 * 3600

const BASE64URL = /^[A-Za-z0-9_-]+={0,2}$/

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v)

/**
 * 서버 relay-token 메시지 검증. 형식이 틀리면 null.
 * 남은 수명은 expiresIn → (expiresAt - serverNow) → (expiresAt - 내 시계, 옛 서버) 순으로 정한다.
 */
export function parseRelayToken(payload: Record<string, unknown>, nowMs: number = Date.now()): RelayTokenMessage | null {
  const { token, addr, ssrc, expiresAt, expiresIn, serverNow } = payload
  if (typeof token !== 'string' || token.length < 60 || token.length > 400 || !BASE64URL.test(token)) return null
  if (typeof addr !== 'string' || addr.length === 0 || addr.length > 255 || !/:\d{1,5}$/.test(addr)) return null
  if (typeof ssrc !== 'number' || !Number.isInteger(ssrc) || ssrc < 0 || ssrc > 0xffffffff) return null
  if (!isNum(expiresAt) || expiresAt <= 0) return null
  let left: number
  if (isNum(expiresIn) && expiresIn > 0 && expiresIn <= MAX_EXPIRES_IN_SEC) left = expiresIn
  else if (isNum(serverNow) && serverNow > 0) left = expiresAt - serverNow
  else left = expiresAt - nowMs / 1000
  left = Math.min(MAX_EXPIRES_IN_SEC, Math.max(0, Math.floor(left)))
  return { token, addr, ssrc, expiresAt, expiresIn: left, expiresInSec: left }
}

/** 받은 지금부터 갱신 요청까지 기다릴 시간(ms). 만료 60초 전, 짧은 수명이면 절반 지점 */
export function relayRefreshDelayMs(expiresInSec: number): number {
  const lead = Math.min(RELAY_REFRESH_BEFORE_SEC, expiresInSec / 2)
  return Math.max(RELAY_REFRESH_MIN_MS, (expiresInSec - lead) * 1000)
}

/** 엔진 릴레이 상태가 새 토큰으로 풀릴 수 있는 상태인지(만료, 또는 릴레이가 토큰을 버려 응답 없음) */
export function relayNeedsNewToken(relay: RelayStats | null | undefined): boolean {
  return relay?.state === 'expired' || relay?.state === 'unreachable'
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
/** 릴레이 대역 상한 근접(PCM16 + 릴레이 피어 여럿) */
export const RELAY_OVER_BUDGET_HINT = '릴레이 대역 초과 — 소리가 끊길 수 있습니다. 코덱을 Opus로 바꿔 주세요'

/**
 * 'failed' 상태 안내. 엔진은 직접 경로가 안 되면 릴레이로 폴백하므로, failed는 릴레이까지 실패했다는 뜻이다.
 * 릴레이가 설정되지 않았거나(서버에 릴레이 없음·설정 실패) 등록이 안 됐으면 그 사실을 알려 준다.
 */
export function failedPeerHint(relay: RelayStats | null | undefined, relayError?: string | null): string {
  if (!relay) {
    if (relayError) return `연결 불가 — 직접 연결 실패, 릴레이 설정 실패(${relayError})`
    return '직접 연결 불가 — 네트워크(대칭형 NAT) 문제, 릴레이 서버 없음'
  }
  switch (relay.state) {
    case 'bound':
      return '연결 불가 — 직접 연결과 릴레이 모두 실패 (상대 네트워크 확인)'
    case 'expired':
      return '연결 불가 — 릴레이 인증 만료 (새 인증 요청 중)'
    case 'rejected':
      return '연결 불가 — 릴레이 서버가 연결을 거절했습니다'
    default:
      return '연결 불가 — 직접 연결 실패, 릴레이 서버에 닿지 않음'
  }
}

/** 피어 배지 보조 문구: 'failed'면 실패 안내, 릴레이 경로면 지연(대역 초과면 그 경고) 안내, 그 밖에는 null */
export function peerPathHint(
  state: PeerState,
  path: PeerPath | null | undefined,
  relay: RelayStats | null | undefined,
  relayError?: string | null,
): string | null {
  if (state === 'failed') return failedPeerHint(relay, relayError)
  if (path === 'relay') return relay?.overBudget ? RELAY_OVER_BUDGET_HINT : RELAY_PATH_HINT
  return null
}
