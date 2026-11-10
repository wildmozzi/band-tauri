import { describe, it, expect } from 'vitest'
import {
  CHECKING_STORAGE_KEY,
  MAX_CHECK_MS,
  POLL_DELAYS_MS,
  isUncertainConfirm,
  loadChecking,
  pollDelay,
  pollOrder,
  runPaymentFlow,
  type ConfirmOutcome,
  type ConfirmParams,
  type KeyValueStorage,
  type OrderStatusResult,
  type PaymentFlowDeps,
} from './paymentFlow'

const PARAMS: ConfirmParams = { paymentKey: 'pk_1', orderId: 'ord_0123456789abcdef', amount: 6900, planType: 'pro' }

class MemoryStorage implements KeyValueStorage {
  map = new Map<string, string>()
  getItem(k: string) { return this.map.get(k) ?? null }
  setItem(k: string, v: string) { this.map.set(k, v) }
  removeItem(k: string) { this.map.delete(k) }
}

/** 가짜 시계·서버: sleep은 시계만 앞으로 돌린다 */
function harness(opts: {
  confirm?: ConfirmOutcome[]
  statuses?: (OrderStatusResult | ((t: number) => OrderStatusResult))[]
  storage?: MemoryStorage
}) {
  let t = 1_000_000
  const sleeps: number[] = []
  const confirms: ConfirmParams[] = []
  const statusCalls: string[] = []
  let checking = 0
  const storage = opts.storage ?? new MemoryStorage()
  const deps: PaymentFlowDeps = {
    async confirm(p) {
      confirms.push(p)
      const seq = opts.confirm ?? [{ httpStatus: 200, success: true }]
      return seq[Math.min(confirms.length - 1, seq.length - 1)]
    },
    async getStatus(id) {
      statusCalls.push(id)
      const seq = opts.statuses ?? [{ httpStatus: 200, status: 'processing' }]
      const s = seq[Math.min(statusCalls.length - 1, seq.length - 1)]
      return typeof s === 'function' ? s(t) : s
    },
    async sleep(ms) { sleeps.push(ms); t += ms },
    now: () => t,
    storage,
    onChecking: () => { checking++ },
  }
  return { deps, sleeps, confirms, statusCalls, storage, checkingCount: () => checking }
}

describe('backoff', () => {
  it('doubles up to a 30s cap', () => {
    expect([0, 1, 2, 3, 4, 5, 20].map(pollDelay)).toEqual([2000, 4000, 8000, 15000, 30000, 30000, 30000])
  })
  it('stays under the server payment rate limit (30/min)', () => {
    // 가장 빠른 첫 1분 동안의 조회 수
    let t = 0, n = 0
    for (let i = 0; t < 60_000; i++) { t += pollDelay(i); n++ }
    expect(n).toBeLessThan(30)
  })
  it('treats 503 / gateway errors / no response as uncertain, everything else as final', () => {
    for (const s of [0, 502, 503, 504]) expect(isUncertainConfirm(s)).toBe(true)
    for (const s of [200, 400, 401, 404, 409, 410, 429, 500]) expect(isUncertainConfirm(s)).toBe(false)
  })
})

describe('runPaymentFlow', () => {
  it('confirm 200 → paid without polling', async () => {
    const h = harness({})
    expect(await runPaymentFlow(PARAMS, h.deps)).toEqual({ kind: 'paid', planType: 'pro' })
    expect(h.statusCalls).toEqual([])
    expect(h.checkingCount()).toBe(0)
  })

  it('confirm 4xx → failed with the server message, no polling', async () => {
    const h = harness({ confirm: [{ httpStatus: 400, success: false, error: '주문 정보와 결제 정보가 일치하지 않습니다.' }] })
    expect(await runPaymentFlow(PARAMS, h.deps)).toEqual({ kind: 'failed', error: '주문 정보와 결제 정보가 일치하지 않습니다.' })
    expect(h.statusCalls).toEqual([])
  })

  it('503 → checking → polls with exponential backoff → paid', async () => {
    const h = harness({
      confirm: [{ httpStatus: 503, success: false, error: '결제 결과를 확인하고 있습니다.' }],
      statuses: [
        { httpStatus: 200, status: 'processing' },
        { httpStatus: 0 }, // 네트워크 오류는 계속 조회
        { httpStatus: 429 },
        { httpStatus: 200, status: 'processing' },
        { httpStatus: 200, status: 'paid' },
      ],
    })
    const r = await runPaymentFlow(PARAMS, h.deps)
    expect(r).toEqual({ kind: 'paid', planType: 'pro' })
    expect(h.checkingCount()).toBe(1)
    expect(h.sleeps).toEqual(POLL_DELAYS_MS.slice(0, 5))
    expect(h.statusCalls.every(id => id === PARAMS.orderId)).toBe(true)
    expect(h.confirms).toHaveLength(1) // 조회 중엔 다시 승인하지 않는다
    expect(h.storage.getItem(CHECKING_STORAGE_KEY)).toBeNull() // 끝나면 지운다
  })

  it('503 → checking state is persisted while polling (survives reload)', async () => {
    let seen: unknown = null
    const storage = new MemoryStorage()
    const h = harness({
      storage,
      confirm: [{ httpStatus: 503, success: false }],
      statuses: [() => { seen = loadChecking(storage); return { httpStatus: 200, status: 'paid' } }],
    })
    await runPaymentFlow(PARAMS, h.deps)
    expect(seen).toMatchObject(PARAMS)
  })

  it('response lost (fetch failed) is treated like 503', async () => {
    const h = harness({ confirm: [{ httpStatus: 0, success: false }], statuses: [{ httpStatus: 200, status: 'paid' }] })
    expect((await runPaymentFlow(PARAMS, h.deps)).kind).toBe('paid')
    expect(h.checkingCount()).toBe(1)
  })

  it('reload while checking: restores and polls without re-sending confirm', async () => {
    const storage = new MemoryStorage()
    storage.setItem(CHECKING_STORAGE_KEY, JSON.stringify({ ...PARAMS, startedAt: 1 }))
    const h = harness({ storage, statuses: [{ httpStatus: 200, status: 'processing' }, { httpStatus: 200, status: 'paid' }] })
    expect(await runPaymentFlow(PARAMS, h.deps)).toEqual({ kind: 'paid', planType: 'pro' })
    expect(h.confirms).toHaveLength(0)
    expect(h.checkingCount()).toBe(1)
    expect(storage.getItem(CHECKING_STORAGE_KEY)).toBeNull()
  })

  it('stored state for a different order is ignored (new payment confirms normally)', async () => {
    const storage = new MemoryStorage()
    storage.setItem(CHECKING_STORAGE_KEY, JSON.stringify({ ...PARAMS, orderId: 'ord_other_order_1', startedAt: 1 }))
    const h = harness({ storage })
    expect((await runPaymentFlow(PARAMS, h.deps)).kind).toBe('paid')
    expect(h.confirms).toHaveLength(1)
  })

  it('corrupt stored state is ignored', () => {
    const s = new MemoryStorage()
    s.setItem(CHECKING_STORAGE_KEY, '{not json')
    expect(loadChecking(s)).toBeNull()
    s.setItem(CHECKING_STORAGE_KEY, JSON.stringify({ orderId: 'x', planType: 'enterprise' }))
    expect(loadChecking(s)).toBeNull()
    expect(loadChecking(null)).toBeNull()
  })
})

describe('pollOrder final states', () => {
  for (const [status, msg] of [['failed', '완료되지 않았습니다'], ['cancelled', '취소'], ['expired', '만료']] as const) {
    it(`${status} → failed with a message, state cleared`, async () => {
      const storage = new MemoryStorage()
      storage.setItem(CHECKING_STORAGE_KEY, JSON.stringify({ ...PARAMS, startedAt: 1 }))
      const h = harness({ storage, statuses: [{ httpStatus: 200, status }] })
      const r = await pollOrder(PARAMS, h.deps)
      expect(r.kind).toBe('failed')
      expect(r.kind === 'failed' && r.error).toContain(msg)
      expect(storage.getItem(CHECKING_STORAGE_KEY)).toBeNull()
    })
  }

  it('404 (not my order / unknown) → failed', async () => {
    const h = harness({ statuses: [{ httpStatus: 404, error: '주문을 찾을 수 없습니다.' }] })
    expect(await pollOrder(PARAMS, h.deps)).toEqual({ kind: 'failed', error: '주문을 찾을 수 없습니다.' })
  })

  it('created (server never received confirm) → re-sends confirm once', async () => {
    const h = harness({
      confirm: [{ httpStatus: 200, success: true }],
      statuses: [{ httpStatus: 200, status: 'created' }],
    })
    expect(await pollOrder(PARAMS, h.deps)).toEqual({ kind: 'paid', planType: 'pro' })
    expect(h.confirms).toEqual([PARAMS])
  })

  it('times out after MAX_CHECK_MS but keeps the state for a later re-check', async () => {
    const storage = new MemoryStorage()
    storage.setItem(CHECKING_STORAGE_KEY, JSON.stringify({ ...PARAMS, startedAt: 1 }))
    const h = harness({ storage })
    expect(await pollOrder(PARAMS, h.deps)).toEqual({ kind: 'timeout' })
    const waited = h.sleeps.reduce((a, b) => a + b, 0)
    expect(waited).toBeGreaterThanOrEqual(MAX_CHECK_MS)
    expect(waited).toBeLessThan(MAX_CHECK_MS + 30_001)
    expect(storage.getItem(CHECKING_STORAGE_KEY)).not.toBeNull()
  })

  it('stops when the screen is left', async () => {
    const h = harness({})
    let left = false
    h.deps.isCancelled = () => left
    h.deps.sleep = async () => { left = true }
    expect(await pollOrder(PARAMS, h.deps)).toEqual({ kind: 'aborted' })
    expect(h.statusCalls).toEqual([])
  })
})
