import { describe, expect, it } from 'vitest'
import {
  RELAY_OVER_BUDGET_HINT,
  RELAY_PATH_HINT,
  RELAY_REFRESH_MIN_MS,
  failedPeerHint,
  parseRelayToken,
  pathLabel,
  peerPathHint,
  relayNeedsNewToken,
  relayRefreshDelayMs,
} from './relayLogic'
import type { RelayStats } from './engineApi'

const token = 'AQAAAABlU_EAAQIDBAhyb29tLWFiYyQxMTExMTExMS0yMjIyLTMzMzMtNDQ0NC01NTU1NTU1NTU1NTU3yyub49sZSgAYhJ_Z3wmUV9vcnzmu9RxfYhugLY8FtA'

describe('parseRelayToken', () => {
  it('서버 relay-token 메시지를 받는다(남은 수명 = expiresIn)', () => {
    const m = parseRelayToken({ type: 'relay-token', token, addr: 'relay.example.com:3478', ssrc: 16909060, expiresAt: 1700000120, expiresIn: 120, serverNow: 1700000000 })
    expect(m).toEqual({ token, addr: 'relay.example.com:3478', ssrc: 16909060, expiresAt: 1700000120, expiresIn: 120, expiresInSec: 120 })
  })

  it('남은 수명은 내 시계와 무관: expiresIn → expiresAt - serverNow → (옛 서버) 내 시계', () => {
    const base = { token, addr: '1.2.3.4:3478', ssrc: 1, expiresAt: 1_700_000_120 }
    const skewedNow = (1_700_000_120 + 600) * 1000 // 내 시계가 10분 빠름: 내 시계로는 이미 만료
    expect(parseRelayToken({ ...base, expiresIn: 120 }, skewedNow)?.expiresIn).toBe(120)
    expect(parseRelayToken({ ...base, serverNow: 1_700_000_000 }, skewedNow)?.expiresIn).toBe(120)
    expect(parseRelayToken(base, skewedNow)?.expiresIn).toBe(0)
    expect(parseRelayToken(base, 1_700_000_000_000)?.expiresIn).toBe(120)
    // 이상값 expiresIn은 무시하고 다음 방법으로
    expect(parseRelayToken({ ...base, expiresIn: -5, serverNow: 1_700_000_100 }, skewedNow)?.expiresIn).toBe(20)
    expect(parseRelayToken({ ...base, expiresIn: 1e9, serverNow: 1_700_000_100 }, skewedNow)?.expiresIn).toBe(20)
  })

  it('형식이 틀리면 null', () => {
    const ok = { token, addr: '1.2.3.4:3478', ssrc: 1, expiresAt: 1 }
    expect(parseRelayToken(ok)).not.toBeNull()
    expect(parseRelayToken({ ...ok, token: 'short' })).toBeNull()
    expect(parseRelayToken({ ...ok, token: `${token}!` })).toBeNull()
    expect(parseRelayToken({ ...ok, token: 1 })).toBeNull()
    expect(parseRelayToken({ ...ok, addr: '1.2.3.4' })).toBeNull()
    expect(parseRelayToken({ ...ok, addr: '' })).toBeNull()
    expect(parseRelayToken({ ...ok, ssrc: -1 })).toBeNull()
    expect(parseRelayToken({ ...ok, ssrc: 2 ** 32 })).toBeNull()
    expect(parseRelayToken({ ...ok, ssrc: 1.5 })).toBeNull()
    expect(parseRelayToken({ ...ok, expiresAt: 'soon' })).toBeNull()
  })
})

describe('relayRefreshDelayMs', () => {
  it('만료 60초 전(짧은 수명이면 절반), 최소 간격 보장', () => {
    expect(relayRefreshDelayMs(120)).toBe(60_000)
    expect(relayRefreshDelayMs(600)).toBe(540_000)
    expect(relayRefreshDelayMs(60)).toBe(30_000)
    expect(relayRefreshDelayMs(8)).toBe(RELAY_REFRESH_MIN_MS)
    expect(relayRefreshDelayMs(0)).toBe(RELAY_REFRESH_MIN_MS)
  })
})

describe('경로 표시', () => {
  const bound: RelayStats = { state: 'bound', addr: '1.2.3.4:3478', rttMs: 20, txKbps: 160, overBudget: false }

  it('pathLabel', () => {
    expect(pathLabel('lan')).toBe('LAN')
    expect(pathLabel('public')).toBe('인터넷')
    expect(pathLabel('relay')).toBe('릴레이')
    expect(pathLabel(null)).toBeNull()
  })

  it('릴레이 경로면 지연 안내, failed는 릴레이까지 실패했을 때의 안내', () => {
    expect(peerPathHint('connected', 'relay', bound)).toBe(RELAY_PATH_HINT)
    expect(peerPathHint('connected', 'lan', bound)).toBeNull()
    expect(peerPathHint('connecting', null, bound)).toBeNull()
    expect(peerPathHint('failed', null, bound)).toContain('릴레이 모두 실패')
    expect(peerPathHint('failed', null, null)).toContain('릴레이 서버 없음')
    expect(failedPeerHint({ ...bound, state: 'unreachable' })).toContain('닿지 않음')
    expect(failedPeerHint({ ...bound, state: 'expired' })).toContain('만료')
    expect(failedPeerHint({ ...bound, state: 'rejected' })).toContain('거절')
  })

  it('릴레이 설정 실패는 "릴레이 서버 없음"과 구분한다(L5)', () => {
    expect(peerPathHint('failed', null, null, '허용되지 않는 릴레이 주소')).toContain('릴레이 설정 실패(허용되지 않는 릴레이 주소)')
  })

  it('릴레이 대역 초과면 경고(M4)', () => {
    expect(peerPathHint('connected', 'relay', { ...bound, overBudget: true })).toBe(RELAY_OVER_BUDGET_HINT)
    expect(peerPathHint('connected', 'lan', { ...bound, overBudget: true })).toBeNull()
  })

  it('새 토큰이 필요한 상태', () => {
    expect(relayNeedsNewToken({ ...bound, state: 'expired' })).toBe(true)
    expect(relayNeedsNewToken({ ...bound, state: 'unreachable' })).toBe(true)
    expect(relayNeedsNewToken(bound)).toBe(false)
    expect(relayNeedsNewToken({ ...bound, state: 'rejected' })).toBe(false)
    expect(relayNeedsNewToken(null)).toBe(false)
  })
})
