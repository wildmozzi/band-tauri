/**
 * P3-5b: PaymentOrderService 단위 테스트 — 주문 발급·소유·만료·변조 거부, 상태 조회(조회 시 앞당긴 재조정 포함)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { SubscriptionService, RECONCILE_MIN_AGE_MS, type PaymentGateway } from '../src/services/subscriptionService'
import { PaymentOrderService, ORDER_TTL_MS, generateOrderId } from '../src/services/paymentOrderService'
import { ServiceError } from '../src/services/errors'
import type { PaymentResponse, TossPaymentLookup } from '../src/types/payment'
import { MemorySubscriptionRepository } from './fakes/memorySubscriptionRepository'
import { MemoryPaymentOrderRepository } from './fakes/memoryPaymentOrderRepository'

function setup(confirm: PaymentResponse = { success: true, outcome: 'approved' }) {
  // 메모리 저장소의 created_at은 실제 시각이므로 시계도 실제 시각에서 시작한다
  let now = new Date()
  const start = now.getTime()
  const clock = () => now
  const lookups: string[] = []
  let lookup: TossPaymentLookup | 'throw' = 'throw'
  const confirms: [string, string, number][] = []
  const g: PaymentGateway = {
    async confirmPayment(k, o, a) { confirms.push([k, o, a]); return confirm },
    async cancelPayment(k) { return { success: true, paymentKey: k } },
    async lookupPayment(k) {
      lookups.push(k)
      if (lookup === 'throw') throw new Error('lookup timeout')
      return lookup
    },
  }
  const subRepo = new MemorySubscriptionRepository()
  const orderRepo = new MemoryPaymentOrderRepository()
  const subs = new SubscriptionService(subRepo, g, { retryDelaysMs: [], now: clock })
  const svc = new PaymentOrderService(orderRepo, subs, { now: clock })
  return {
    svc, subRepo, orderRepo, confirms, lookups, start,
    advance: (ms: number) => { now = new Date(now.getTime() + ms) },
    setLookup: (l: TossPaymentLookup | 'throw') => { lookup = l },
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
