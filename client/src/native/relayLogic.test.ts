import { describe, expect, it } from 'vitest'
import {
  RELAY_PATH_HINT,
  RELAY_REFRESH_MIN_MS,
  failedPeerHint,
  parseRelayToken,
  pathLabel,
  peerPathHint,
  relayRefreshDelayMs,
} from './relayLogic'

const token = 'AQAAAABlU_EAAQIDBAhyb29tLWFiYyQxMTExMTExMS0yMjIyLTMzMzMtNDQ0NC01NTU1NTU1NTU1NTU3yyub49sZSgAYhJ_Z3wmUV9vcnzmu9RxfYhugLY8FtA'

describe('parseRelayToken', () => {
  it('서버 relay-token 메시지를 받는다', () => {
    const m = parseRelayToken({ type: 'relay-token', token, addr: 'relay.example.com:3478', ssrc: 16909060, expiresAt: 1700000000 })
    expect(m).toEqual({ token, addr: 'relay.example.com:3478', ssrc: 16909060, expiresAt: 1700000000 })
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
  it('만료 30초 전, 최소 간격 보장', () => {
    const now = 1_000_000_000
    expect(relayRefreshDelayMs(now / 1000 + 120, now)).toBe(90_000)
    expect(relayRefreshDelayMs(now / 1000 + 31, now)).toBe(RELAY_REFRESH_MIN_MS)
    expect(relayRefreshDelayMs(now / 1000 - 10, now)).toBe(RELAY_REFRESH_MIN_MS)
  })
})

describe('경로 표시', () => {
  it('pathLabel', () => {
    expect(pathLabel('lan')).toBe('LAN')
    expect(pathLabel('public')).toBe('인터넷')
    expect(pathLabel('relay')).toBe('릴레이')
    expect(pathLabel(null)).toBeNull()
  })

  it('릴레이 경로면 지연 안내, failed는 릴레이까지 실패했을 때의 안내', () => {
    const bound = { state: 'bound' as const, addr: '1.2.3.4:3478', rttMs: 20 }
    expect(peerPathHint('connected', 'relay', bound)).toBe(RELAY_PATH_HINT)
    expect(peerPathHint('connected', 'lan', bound)).toBeNull()
    expect(peerPathHint('connecting', null, bound)).toBeNull()
    expect(peerPathHint('failed', null, bound)).toContain('릴레이 모두 실패')
    expect(peerPathHint('failed', null, null)).toContain('릴레이 서버 없음')
    expect(failedPeerHint({ ...bound, state: 'unreachable' })).toContain('닿지 않음')
    expect(failedPeerHint({ ...bound, state: 'expired' })).toContain('만료')
    expect(failedPeerHint({ ...bound, state: 'rejected' })).toContain('거절')
  })
})
