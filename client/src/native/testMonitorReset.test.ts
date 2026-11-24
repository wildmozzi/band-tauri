import { describe, expect, it } from 'vitest'
import { DEVICE_RESET_RETRY_DELAYS_MS } from './performMessages'
import { TEST_RESET_RETRY_DELAYS_MS, TestResetLimiter } from './testMonitorReset'

describe('TestResetLimiter — 테스트 모니터 리셋 재시작 반복 상한 (#263 리뷰 L-3)', () => {
  it('기본 60초에 8번까지 허용하고 9번째는 막는다', () => {
    const l = new TestResetLimiter()
    for (let i = 0; i < 8; i++) expect(l.allow(i * 1000)).toBe(true)
    expect(l.allow(8000)).toBe(false)
  })

  it('창(60초)을 벗어난 리셋은 빠진다', () => {
    const l = new TestResetLimiter(2, 60_000)
    expect(l.allow(0)).toBe(true)
    expect(l.allow(1000)).toBe(true)
    expect(l.allow(2000)).toBe(false)
    // 0·1000·2000이 모두 창 밖으로 나간 뒤
    expect(l.allow(62_000)).toBe(true)
    expect(l.allow(62_500)).toBe(true)
    expect(l.allow(63_000)).toBe(false)
  })

  it('다시 열기 대기는 합주의 리셋 재시작 기본값과 같다', () => {
    expect(TEST_RESET_RETRY_DELAYS_MS).toEqual(DEVICE_RESET_RETRY_DELAYS_MS)
  })
})
