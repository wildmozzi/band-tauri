import { describe, expect, it } from 'vitest'
import {
  backoffDelay, DUPLICATE_CONNECTION_CLOSE_REASON, isAbnormalClose, isReplacedClose, ReconnectBudget, RECONNECT_BUDGET_MS,
} from './reconnectPolicy'

/** 지터 0 (random 0.5 → 배율 1) */
const noJitter = () => new ReconnectBudget({ random: () => 0.5 })
const drop = (b: ReconnectBudget, now: number, code = 1006, inRoom = true) => b.onClosed({ code, now, inRoom })

describe('reconnectPolicy', () => {
  it('백오프는 0.5 → 1 → 2 → 4 → 4 초', () => {
    expect([0, 1, 2, 3, 4, 10].map(backoffDelay)).toEqual([500, 1000, 2000, 4000, 4000, 4000])
    const b = noJitter()
    let now = 0
    const delays: number[] = []
    for (let i = 0; i < 6; i++) {
      const d = drop(b, now)
      if (d.action !== 'retry') throw new Error('retry여야 함')
      delays.push(d.delayMs)
      now += d.delayMs + 100 // 연결 시도 시간
    }
    expect(delays).toEqual([500, 1000, 2000, 4000, 4000, 4000])
  })

  it('지터는 ±20% 안', () => {
    expect(drop(new ReconnectBudget({ random: () => 0 }), 0)).toEqual({ action: 'retry', delayMs: 400 })
    expect(drop(new ReconnectBudget({ random: () => 0.9999 }), 0)).toEqual({ action: 'retry', delayMs: 600 })
  })

  it('첫 비정상 닫힘부터 35초 안에서는 재시도, 넘기면 give-up', () => {
    const b = noJitter()
    expect(drop(b, 1000).action).toBe('retry')
    expect(drop(b, 1000 + RECONNECT_BUDGET_MS - 1).action).toBe('retry')
    expect(drop(b, 1000 + RECONNECT_BUDGET_MS)).toEqual({ action: 'give-up' })
  })

  it('예산 끝을 넘겨 기다리지 않는다', () => {
    const b = noJitter()
    for (let i = 0; i < 5; i++) drop(b, 0) // 다음 기본 지연 4초
    expect(drop(b, RECONNECT_BUDGET_MS - 1000)).toEqual({ action: 'retry', delayMs: 1000 })
  })

  it('ready가 되면 예산과 백오프가 리셋된다', () => {
    const b = noJitter()
    drop(b, 0)
    drop(b, 1000)
    drop(b, 30_000)
    b.onReady()
    expect(b.dropping).toBe(false)
    // 리셋 뒤 새 단절은 0.5초부터, 새 35초 예산
    expect(drop(b, 40_000)).toEqual({ action: 'retry', delayMs: 500 })
    expect(drop(b, 40_000 + RECONNECT_BUDGET_MS - 1).action).toBe('retry')
  })

  it('ready 없이 열렸다 닫히기만 하면 예산은 이어진다 (서버 재시작 폭주 방지)', () => {
    const b = noJitter()
    drop(b, 0)
    // onopen은 정책에 알리지 않는다 → 같은 단절 구간
    expect(drop(b, RECONNECT_BUDGET_MS + 1)).toEqual({ action: 'give-up' })
  })

  it('정상 닫힘(1000/1005)·방 밖은 재연결하지 않고 단절 구간도 끝낸다', () => {
    const b = noJitter()
    expect(drop(b, 0, 1000)).toEqual({ action: 'none' })
    expect(drop(b, 0, 1005)).toEqual({ action: 'none' })
    drop(b, 0)
    expect(drop(b, 1000, 1006, false)).toEqual({ action: 'none' })
    expect(b.dropping).toBe(false)
  })

  it('방 안에서 중복 연결 사유의 정상 닫힘은 replaced (재연결하지 않음), 방 밖이면 none', () => {
    const b = noJitter()
    const reason = DUPLICATE_CONNECTION_CLOSE_REASON
    expect(b.onClosed({ code: 1000, now: 0, inRoom: true, reason })).toEqual({ action: 'replaced' })
    expect(b.dropping).toBe(false)
    expect(b.onClosed({ code: 1000, now: 0, inRoom: false, reason })).toEqual({ action: 'none' })
    // 다른 사유의 정상 닫힘은 그대로 none, 비정상 코드는 사유와 무관하게 재시도
    expect(b.onClosed({ code: 1000, now: 0, inRoom: true, reason: 'bye' })).toEqual({ action: 'none' })
    expect(b.onClosed({ code: 1006, now: 0, inRoom: true, reason }).action).toBe('retry')
  })

  it('중복 연결 사유는 서버 문자열과 같다 (server/src/signaling/membership.ts)', () => {
    expect(DUPLICATE_CONNECTION_CLOSE_REASON).toBe('Duplicate connection')
    expect(isReplacedClose(1000, 'Duplicate connection')).toBe(true)
    expect(isReplacedClose(1005, 'Duplicate connection')).toBe(true)
    expect(isReplacedClose(1008, 'Duplicate connection')).toBe(false)
    expect(isReplacedClose(1000, undefined)).toBe(false)
  })

  it('수신 없음 자가 닫힘(4000)은 비정상', () => {
    expect(isAbnormalClose(4000)).toBe(true)
    expect(isAbnormalClose(1006)).toBe(true)
    expect(isAbnormalClose(1000)).toBe(false)
    expect(isAbnormalClose(1005)).toBe(false)
  })
})
