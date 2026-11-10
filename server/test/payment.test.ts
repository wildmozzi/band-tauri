/**
 * H2 회귀 테스트: 결제 라우트 인증, 서버 확정 userId, 서버 가격표 금액
 * P3-5b: orderId 서버 발급 — 남의 주문·만료 주문·금액/플랜 변조·클라이언트 생성 orderId 거부, 주문 상태 조회
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { bearer, testVerifier } from './helpers'
import type { MemoryPaymentOrderRepository } from './fakes/memoryPaymentOrderRepository'

let server: Server
let url = ''
let orderRepo: MemoryPaymentOrderRepository
const confirmed: { paymentKey: string; orderId: string; amount: number }[] = []
const cancelled: string[] = []
/** 다음 토스 confirm 응답을 바꾼다 (기본: 승인) */
let nextConfirm: null | { success: false; outcome: 'unknown' | 'rejected'; error: string } = null

const post = (path: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(`${url}${path}`, { method: 'POST', headers: { 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) })

/** 서버에서 주문 발급 */
async function newOrder(user: string, planType: 'standard' | 'pro'): Promise<{ orderId: string; amount: number; planType: string; expiresAt: string }> {
  const r = await post('/api/payment/orders', { planType }, bearer(user))
  assert.equal(r.status, 201)
  return ((await r.json()) as any).order
}

const orderStatus = async (user: string, orderId: string) =>
  fetch(`${url}/api/payment/orders/${orderId}`, { headers: bearer(user) })

before(async () => {
  const auth = await import('../src/lib/auth')
  auth.setTokenVerifier(testVerifier)
  const { paymentService } = await import('../src/services/paymentService')
  // 토스 API 호출 대신 기록만
  paymentService.confirmPayment = async (paymentKey: string, orderId: string, amount: number) => {
    confirmed.push({ paymentKey, orderId, amount })
    if (nextConfirm) { const r = nextConfirm; nextConfirm = null; return r }
    return { success: true, paymentKey, orderId }
  }
  paymentService.cancelPayment = async (paymentKey: string) => {
    cancelled.push(paymentKey)
    return { success: true, paymentKey }
  }
  // 조회도 목: 결과 불명 테스트용 (진행 중)
  paymentService.lookupPayment = async () => { throw new Error('lookup timeout') }
  // 구독·주문 저장소는 DB 대신 메모리 목 (repository 계층 경계에서 교체)
  const { SubscriptionService } = await import('../src/services/subscriptionService')
  const { PaymentOrderService } = await import('../src/services/paymentOrderService')
  const { MemorySubscriptionRepository } = await import('./fakes/memorySubscriptionRepository')
  const { MemoryPaymentOrderRepository } = await import('./fakes/memoryPaymentOrderRepository')
  const subscriptionService = new SubscriptionService(new MemorySubscriptionRepository(), paymentService, { retryDelaysMs: [] })
  orderRepo = new MemoryPaymentOrderRepository()
  const paymentOrderService = new PaymentOrderService(orderRepo, subscriptionService)
  const { createApp } = await import('../src/app')
  server = createApp({ subscriptionService, paymentOrderService, rateLimits: false }).listen(0)
  await new Promise(r => server.once('listening', r))
  url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
})

after(() => server?.close())

describe('payment routes require auth', () => {
  it('/plans is public', async () => {
    const r = await fetch(`${url}/api/payment/plans`)
    assert.equal(r.status, 200)
  })
  for (const [method, path] of [
    ['POST', '/api/payment/orders'],
    ['GET', '/api/payment/orders/ord_0123456789'],
    ['POST', '/api/payment/confirm'],
    ['POST', '/api/payment/cancel'],
    ['POST', '/api/payment/change-plan'],
    ['GET', '/api/payment/subscription'],
    ['GET', '/api/payment/subscription/victim'],
  ] as const) {
    it(`${method} ${path} → 401 without token`, async () => {
      const r = await fetch(`${url}${path}`, { method, headers: { 'Content-Type': 'application/json' }, body: method === 'POST' ? '{}' : undefined })
      assert.equal(r.status, 401)
    })
    it(`${method} ${path} → 401 with invalid token`, async () => {
      const r = await fetch(`${url}${path}`, { method, headers: { 'Content-Type': 'application/json', Authorization: 'Bearer forged' }, body: method === 'POST' ? '{}' : undefined })
      assert.equal(r.status, 401)
    })
  }
})

describe('orders', () => {
  it('issues an order with the server price, bound to the user, expiring in 30 min', async () => {
    const before = Date.now()
    const o = await newOrder('alice', 'pro')
    assert.match(o.orderId, /^[A-Za-z0-9_-]{6,64}$/)
    assert.equal(o.amount, 6900)
    assert.equal(o.planType, 'pro')
    const ttl = new Date(o.expiresAt).getTime() - before
    assert.ok(ttl > 29 * 60_000 && ttl <= 30 * 60_000 + 1000, `ttl=${ttl}`)
    assert.equal(orderRepo.orders.find(x => x.orderId === o.orderId)?.userId, 'alice')
    const st = await (await orderStatus('alice', o.orderId)).json() as any
    assert.equal(st.order.status, 'created')
  })

  it('ignores body amount/userId when issuing', async () => {
    const r = await post('/api/payment/orders', { planType: 'standard', amount: 1, userId: 'victim' }, bearer('alice'))
    const o = ((await r.json()) as any).order
    assert.equal(o.amount, 2900)
    assert.equal(orderRepo.orders.find(x => x.orderId === o.orderId)?.userId, 'alice')
  })

  it('rejects free/unknown plans', async () => {
    for (const planType of ['free', 'enterprise', undefined]) {
      const r = await post('/api/payment/orders', { planType }, bearer('alice'))
      assert.equal(r.status, 400)
    }
  })

  it("cannot read another user's order (404)", async () => {
    const o = await newOrder('alice', 'pro')
    assert.equal((await orderStatus('mallory', o.orderId)).status, 404)
    assert.equal((await orderStatus('alice', 'ord_doesnotexist')).status, 404)
    assert.equal((await orderStatus('alice', 'bad id!')).status, 404)
  })
})

describe('confirm', () => {
  it('confirms an own order using the server price and records the subscription', async () => {
    const o = await newOrder('alice', 'pro')
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_alice', orderId: o.orderId, planType: 'pro', amount: 6900, userId: 'victim' }, bearer('alice'))
    assert.equal(r.status, 200)
    assert.deepEqual(confirmed.at(-1), { paymentKey: 'pk_alice', orderId: o.orderId, amount: 6900 })

    const mine = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('alice') })).json() as any
    assert.equal(mine.user.id, 'alice')
    assert.equal(mine.user.planType, 'pro')
    assert.equal(mine.user.subscription.amount, 6900)

    const victim = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('victim') })).json() as any
    assert.equal(victim.user.planType, 'free')

    const st = await (await orderStatus('alice', o.orderId)).json() as any
    assert.equal(st.order.status, 'paid')
    assert.equal(st.order.subscription.planType, 'pro')
  })

  it('rejects a client-generated orderId (no server order) before calling Toss', async () => {
    const calls = confirmed.length
    for (const orderId of ['order-alice-pro-12345678-abcd', 'ord_ffffffffffffffffffffffffffffffff']) {
      const r = await post('/api/payment/confirm', { paymentKey: 'pk_x', orderId, planType: 'pro', amount: 6900 }, bearer('alice'))
      assert.equal(r.status, 404)
    }
    assert.equal(confirmed.length, calls)
  })

  it("rejects confirming another user's order (404) before calling Toss", async () => {
    const o = await newOrder('alice', 'standard')
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_mal', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('mallory'))
    assert.equal(r.status, 404)
    assert.equal(confirmed.length, calls)
    const mal = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('mallory') })).json() as any
    assert.equal(mal.user.planType, 'free')
    // 원래 주인은 여전히 결제할 수 있다
    assert.equal((await (await orderStatus('alice', o.orderId)).json() as any).order.status, 'created')
  })

  it('rejects a tampered amount or plan (400) before calling Toss', async () => {
    const o = await newOrder('carol', 'pro')
    const calls = confirmed.length
    for (const body of [
      { paymentKey: 'pk_c', orderId: o.orderId, planType: 'pro', amount: 100 },
      { paymentKey: 'pk_c', orderId: o.orderId, planType: 'pro', amount: 2900 },
      { paymentKey: 'pk_c', orderId: o.orderId, planType: 'standard', amount: 2900 },
      { paymentKey: 'pk_c', orderId: o.orderId, planType: 'standard', amount: 6900 },
    ]) {
      const r = await post('/api/payment/confirm', body, bearer('carol'))
      assert.equal(r.status, 400, JSON.stringify(body))
    }
    // amount 누락도 거부 (스키마)
    assert.equal((await post('/api/payment/confirm', { paymentKey: 'pk_c', orderId: o.orderId, planType: 'pro' }, bearer('carol'))).status, 400)
    assert.equal(confirmed.length, calls)
  })

  it('rejects an expired order (410) before calling Toss', async () => {
    const o = await newOrder('dave', 'standard')
    const row = orderRepo.orders.find(x => x.orderId === o.orderId)!
    row.expiresAt = new Date(Date.now() - 1000)
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_d', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('dave'))
    assert.equal(r.status, 410)
    assert.equal(confirmed.length, calls)
    assert.equal((await (await orderStatus('dave', o.orderId)).json() as any).order.status, 'expired')
  })

  it('rejects free/unknown plans', async () => {
    for (const planType of ['free', 'enterprise', undefined]) {
      const r = await post('/api/payment/confirm', { paymentKey: 'pk', orderId: 'ord_0123456789', planType, amount: 0 }, bearer('alice'))
      assert.equal(r.status, 400)
    }
  })

  it('uncertain result → 503, order shows processing (client polls status)', async () => {
    const o = await newOrder('erin', 'standard')
    nextConfirm = { success: false, outcome: 'unknown', error: 'timeout' }
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_e', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('erin'))
    assert.equal(r.status, 503)
    assert.equal((await (await orderStatus('erin', o.orderId)).json() as any).order.status, 'processing')
    // 만료 뒤에도 같은 결제의 재시도는 허용 (결제 기록이 이미 있으므로) → 이번엔 승인
    orderRepo.orders.find(x => x.orderId === o.orderId)!.expiresAt = new Date(Date.now() - 1000)
    const again = await post('/api/payment/confirm', { paymentKey: 'pk_e', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('erin'))
    assert.equal(again.status, 200)
    assert.equal((await (await orderStatus('erin', o.orderId)).json() as any).order.status, 'paid')
  })
})

describe('subscription lookup', () => {
  it("cannot read another user's subscription via URL", async () => {
    const r = await fetch(`${url}/api/payment/subscription/alice`, { headers: bearer('mallory') })
    assert.equal(r.status, 403)
  })
  it('can read own subscription via URL (backward compatible)', async () => {
    const r = await fetch(`${url}/api/payment/subscription/alice`, { headers: bearer('alice') })
    assert.equal(r.status, 200)
  })
})

describe('change-plan', () => {
  it('cannot upgrade to a paid plan without payment', async () => {
    const r = await post('/api/payment/change-plan', { userId: 'alice', newPlan: 'pro' }, bearer('mallory'))
    assert.equal(r.status, 403)
    const r2 = await post('/api/payment/change-plan', { newPlan: 'standard' }, bearer('alice'))
    assert.equal(r2.status, 403)
  })
})

describe('cancel', () => {
  it("cannot cancel using another user's paymentKey or affect another user", async () => {
    const o = await newOrder('mallory', 'standard')
    await post('/api/payment/confirm', { paymentKey: 'pk_mallory', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('mallory'))
    const r = await post('/api/payment/cancel', { paymentKey: 'pk_alice', userId: 'alice' }, bearer('mallory'))
    assert.equal(r.status, 403)
    assert.ok(!cancelled.includes('pk_alice'))
    const alice = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('alice') })).json() as any
    assert.equal(alice.user.planType, 'pro')
  })

  it('cancels own subscription', async () => {
    const r = await post('/api/payment/cancel', { paymentKey: 'pk_mallory' }, bearer('mallory'))
    assert.equal(r.status, 200)
    assert.deepEqual(cancelled, ['pk_mallory'])
    const m = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('mallory') })).json() as any
    assert.equal(m.user.planType, 'free')
  })

  it('returns 404 without an active subscription', async () => {
    const r = await post('/api/payment/cancel', {}, bearer('nobody'))
    assert.equal(r.status, 404)
  })
})

describe('idempotency', () => {
  it('replaying the same confirm returns the existing subscription without charging again', async () => {
    const o = await newOrder('bob', 'standard')
    const body = { paymentKey: 'pk_bob', orderId: o.orderId, planType: 'standard', amount: 2900 }
    const first = await (await post('/api/payment/confirm', body, bearer('bob'))).json() as any
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', body, bearer('bob'))
    assert.equal(r.status, 200)
    const again = await r.json() as any
    assert.equal(again.subscription.id, first.subscription.id)
    assert.equal(confirmed.length, calls, 'Toss confirm must not be called again')
  })

  it("rejects reusing another user's paymentKey with 409 before calling Toss", async () => {
    const o = await newOrder('eve', 'standard')
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_bob', orderId: o.orderId, planType: 'standard', amount: 2900 }, bearer('eve'))
    assert.equal(r.status, 409)
    assert.equal(confirmed.length, calls)
  })
})
