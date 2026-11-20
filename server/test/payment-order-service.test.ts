/**
 * P3-5b: PaymentOrderService 단위 테스트 — 주문 발급·소유·만료·변조 거부, 상태 조회(조회 시 앞당긴 재조정 포함)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { SubscriptionService, RECONCILE_MIN_AGE_MS, type PaymentGateway } from '../src/services/subscriptionService'
import {
  PaymentOrderService, ORDER_TTL_MS, ORDER_RETENTION_MS, MAX_OPEN_ORDERS, MAX_ORDERS_PER_MINUTE, generateOrderId,
} from '../src/services/paymentOrderService'
import { RepositoryError } from '../src/repositories/errors'
import { PLAN_PRICES } from '../src/types/payment'
import { ServiceError } from '../src/services/errors'
import type { PaymentResponse, TossPaymentLookup } from '../src/types/payment'
import { MemorySubscriptionRepository } from './fakes/memorySubscriptionRepository'
import { MemoryPaymentOrderRepository } from './fakes/memoryPaymentOrderRepository'

function setup(initialConfirm: PaymentResponse = { success: true, outcome: 'approved' }) {
  let confirm = initialConfirm
  /** 토스 승인 호출 중에 실행 (경쟁 재현용) */
  let onConfirm: (() => Promise<void>) | null = null
  // 서비스와 메모리 저장소(created_at/updated_at)가 같은 가상 시계를 쓴다.
  // 저장소가 실제 시각을 쓰면 실행 지연(수 ms)만큼 created_at이 가상 시계보다 앞서 경계값 비교가 흔들린다.
  let now = new Date()
  const start = now.getTime()
  const clock = () => now
  const lookups: string[] = []
  let lookup: TossPaymentLookup | 'throw' = 'throw'
  const confirms: [string, string, number][] = []
  const g: PaymentGateway = {
    async confirmPayment(k, o, a) {
      confirms.push([k, o, a])
      if (onConfirm) await onConfirm()
      return confirm
    },
    async cancelPayment(k) { return { success: true, paymentKey: k } },
    async lookupPayment(k) {
      lookups.push(k)
      if (lookup === 'throw') throw new Error('lookup timeout')
      return lookup
    },
  }
  const subRepo = new MemorySubscriptionRepository(clock)
  const orderRepo = new MemoryPaymentOrderRepository()
  const subs = new SubscriptionService(subRepo, g, { retryDelaysMs: [], now: clock })
  const svc = new PaymentOrderService(orderRepo, subs, { now: clock })
  return {
    svc, subRepo, orderRepo, confirms, lookups, start,
    advance: (ms: number) => { now = new Date(now.getTime() + ms) },
    setLookup: (l: TossPaymentLookup | 'throw') => { lookup = l },
    setConfirm: (c: PaymentResponse) => { confirm = c },
    setOnConfirm: (f: (() => Promise<void>) | null) => { onConfirm = f },
  }
}

async function rejectsWith(p: Promise<unknown>, status: number) {
  await assert.rejects(p, (e: unknown) => e instanceof ServiceError && e.status === status)
}

describe('generateOrderId', () => {
  it('matches the Toss orderId rule and is unique', () => {
    const ids = new Set(Array.from({ length: 200 }, generateOrderId))
    assert.equal(ids.size, 200)
    for (const id of ids) assert.match(id, /^[A-Za-z0-9_-]{6,64}$/)
  })
})

describe('PaymentOrderService', () => {
  it('confirm uses the order (server) amount and marks the attempt', async () => {
    const t = setup()
    const o = await t.svc.createOrder('u1', 'standard')
    assert.equal(o.amount, 2900)
    assert.equal(o.expiresAt.getTime() - t.start, ORDER_TTL_MS)
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'standard', amount: 2900 })
    assert.deepEqual(t.confirms, [['pk1', o.orderId, 2900]])
    assert.ok(t.orderRepo.orders[0].confirmAttemptedAt)
    const st = await t.svc.getOrderStatus('u1', o.orderId)
    assert.equal(st.status, 'paid')
    assert.equal(st.subscription?.planType, 'standard')
  })

  it("other user's order, unknown order → 404; tampering → 400; expired → 410 (Toss never called)", async () => {
    const t = setup()
    const o = await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.confirm('u2', { paymentKey: 'pk', orderId: o.orderId, planType: 'pro', amount: 6900 }), 404)
    await rejectsWith(t.svc.getOrderStatus('u2', o.orderId), 404)
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk', orderId: 'order-client-made', planType: 'pro', amount: 6900 }), 404)
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk', orderId: o.orderId, planType: 'pro', amount: 1 }), 400)
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk', orderId: o.orderId, planType: 'standard', amount: 6900 }), 400)
    t.advance(ORDER_TTL_MS)
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk', orderId: o.orderId, planType: 'pro', amount: 6900 }), 410)
    assert.equal(t.confirms.length, 0)
    assert.equal(t.orderRepo.orders[0].confirmAttemptedAt, null, 'rejected requests do not count as attempts')
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'expired')
  })

  it('rejected payment → failed once the pending row is discarded', async () => {
    const t = setup({ success: false, outcome: 'rejected', error: '카드 거절' })
    const o = await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 }), 400)
    // pending 행은 재조정이 지울 때까지 남는다 → processing
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'processing')
    t.setLookup({ found: false })
    t.advance(RECONCILE_MIN_AGE_MS + 1)
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'failed')
  })

  it('503 (unknown) → polling status reconciles early once old enough → paid', async () => {
    const t = setup({ success: false, outcome: 'unknown', error: 'timeout' })
    const o = await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 }), 503)
    const lookupsAfterConfirm = t.lookups.length

    // 진행 중인 요청과 겹치지 않게: 2분 전에는 조회하지 않는다
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'processing')
    assert.equal(t.lookups.length, lookupsAfterConfirm)

    // 2분 뒤: 조회 실패 → 여전히 processing, 마지막 확인 시각 갱신
    t.advance(RECONCILE_MIN_AGE_MS + 1)
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'processing')
    assert.equal(t.lookups.length, lookupsAfterConfirm + 1)

    // 30초 안의 재조회는 토스에 다시 묻지 않는다
    t.setLookup({ found: true, payment: { paymentKey: 'pk1', orderId: o.orderId, status: 'DONE', totalAmount: 6900 } })
    t.advance(5_000)
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'processing')
    assert.equal(t.lookups.length, lookupsAfterConfirm + 1)

    t.advance(30_000)
    const st = await t.svc.getOrderStatus('u1', o.orderId)
    assert.equal(st.status, 'paid')
    assert.equal(st.subscription?.planType, 'pro')
  })
})

describe('리뷰 #8 M1: 사용자당 주문 발급 제한 + 만료 주문 정리', () => {
  it(`more than ${MAX_ORDERS_PER_MINUTE} orders per minute → 429, allowed again after a minute; other users unaffected`, async () => {
    const t = setup()
    for (let i = 0; i < MAX_ORDERS_PER_MINUTE; i++) await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.createOrder('u1', 'pro'), 429)
    await t.svc.createOrder('u2', 'pro')
    t.advance(60_001)
    await t.svc.createOrder('u1', 'pro')
  })

  it(`more than ${MAX_OPEN_ORDERS} open (unexpired, unattempted) orders → 429; attempted/expired ones don't count`, async () => {
    const t = setup()
    const ids: string[] = []
    for (let i = 0; i < MAX_OPEN_ORDERS; i++) {
      ids.push((await t.svc.createOrder('u1', 'standard')).orderId)
      t.advance(61_000) // 분당 제한은 피한다
    }
    await rejectsWith(t.svc.createOrder('u1', 'standard'), 429)
    // 하나를 결제 시도하면 미결에서 빠진다
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: ids[0], planType: 'standard', amount: 2900 })
    await t.svc.createOrder('u1', 'standard')
    await rejectsWith(t.svc.createOrder('u1', 'standard'), 429)
    // 만료되면 다시 발급 가능
    t.advance(ORDER_TTL_MS)
    await t.svc.createOrder('u1', 'standard')
  })

  it('cleanup removes only unattempted orders expired for over 24h', async () => {
    const t = setup()
    const stale = await t.svc.createOrder('u1', 'pro')
    const attempted = await t.svc.createOrder('u1', 'pro')
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: attempted.orderId, planType: 'pro', amount: 6900 })
    t.advance(ORDER_TTL_MS + ORDER_RETENTION_MS + 1)
    const fresh = await t.svc.createOrder('u1', 'pro')
    assert.equal(await t.svc.cleanupStaleOrders(), 1)
    const left = t.orderRepo.orders.map(o => o.orderId)
    assert.ok(!left.includes(stale.orderId))
    assert.ok(left.includes(attempted.orderId), 'attempted orders are kept for status lookups')
    assert.ok(left.includes(fresh.orderId))
  })
})

describe('리뷰 #8 M2: confirm 시도 표시는 pending 결제 행 뒤에 → failed 오판 없음', () => {
  it('transient DB error before the pending row → order stays created (not failed) and a retry succeeds', async () => {
    const t = setup()
    const o = await t.svc.createOrder('u1', 'pro')
    const orig = t.subRepo.createPendingPayment.bind(t.subRepo)
    t.subRepo.createPendingPayment = async () => { throw new RepositoryError('connection reset') }
    await assert.rejects(t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 }), RepositoryError)
    assert.equal(t.confirms.length, 0, 'Toss not called')
    assert.equal(t.orderRepo.orders[0].confirmAttemptedAt, null)
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'created')
    t.subRepo.createPendingPayment = orig
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 })
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'paid')
  })

  it('status polled while confirm is in flight (after the pending row) → processing, never failed', async () => {
    const t = setup()
    const o = await t.svc.createOrder('u1', 'pro')
    const seen: string[] = []
    t.setOnConfirm(async () => {
      seen.push((await t.svc.getOrderStatus('u1', o.orderId)).status)
      assert.ok(t.orderRepo.orders[0].confirmAttemptedAt, 'attempt marked before Toss is called')
    })
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 })
    assert.deepEqual(seen, ['processing'])
  })

  it('pending row exists but the attempt mark fails → payment still proceeds', async () => {
    const t = setup()
    const o = await t.svc.createOrder('u1', 'pro')
    t.orderRepo.markConfirmAttempted = async () => { throw new RepositoryError('timeout') }
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 })
    assert.equal((await t.svc.getOrderStatus('u1', o.orderId)).status, 'paid')
  })

  it('conflicting paymentKey (409) does not mark the order as attempted', async () => {
    const t = setup()
    const a = await t.svc.createOrder('u1', 'pro')
    await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: a.orderId, planType: 'pro', amount: 6900 })
    const b = await t.svc.createOrder('u2', 'pro')
    await rejectsWith(t.svc.confirm('u2', { paymentKey: 'pk1', orderId: b.orderId, planType: 'pro', amount: 6900 }), 409)
    assert.equal((await t.svc.getOrderStatus('u2', b.orderId)).status, 'created')
  })
})

describe('리뷰 #8 L3/L4', () => {
  it('price table change: blocks new payments, but a retry of an in-flight payment uses the order amount', async () => {
    const t = setup({ success: false, outcome: 'unknown', error: 'timeout' })
    const inflight = await t.svc.createOrder('u1', 'pro')
    const fresh = await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk1', orderId: inflight.orderId, planType: 'pro', amount: 6900 }), 503)
    const orig = PLAN_PRICES.pro
    PLAN_PRICES.pro = 7900
    try {
      await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk2', orderId: fresh.orderId, planType: 'pro', amount: 6900 }), 400)
      t.setConfirm({ success: true, outcome: 'approved' })
      const sub = await t.svc.confirm('u1', { paymentKey: 'pk1', orderId: inflight.orderId, planType: 'pro', amount: 6900 })
      assert.equal(sub.amount, 6900)
      assert.deepEqual(t.confirms.at(-1), ['pk1', inflight.orderId, 6900])
    } finally {
      PLAN_PRICES.pro = orig
    }
  })

  it('poll-triggered recheck stays rate-limited even if touching the pending row fails', async () => {
    const t = setup({ success: false, outcome: 'unknown', error: 'timeout' })
    const o = await t.svc.createOrder('u1', 'pro')
    await rejectsWith(t.svc.confirm('u1', { paymentKey: 'pk1', orderId: o.orderId, planType: 'pro', amount: 6900 }), 503)
    t.subRepo.touchPendingPayment = async () => { throw new RepositoryError('touch failed') }
    t.advance(RECONCILE_MIN_AGE_MS + 1)
    const base = t.lookups.length
    await t.svc.getOrderStatus('u1', o.orderId)
    await t.svc.getOrderStatus('u1', o.orderId)
    await t.svc.getOrderStatus('u1', o.orderId)
    assert.equal(t.lookups.length, base + 1)
    t.advance(30_000)
    await t.svc.getOrderStatus('u1', o.orderId)
    assert.equal(t.lookups.length, base + 2)
  })
})
