/**
 * H2 회귀 테스트: 결제 라우트 인증, 서버 확정 userId, 서버 가격표 금액
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { bearer, testVerifier } from './helpers'

let server: Server
let url = ''
const confirmed: { paymentKey: string; orderId: string; amount: number }[] = []
const cancelled: string[] = []

const post = (path: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(`${url}${path}`, { method: 'POST', headers: { 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) })

before(async () => {
  const auth = await import('../src/lib/auth')
  auth.setTokenVerifier(testVerifier)
  const { paymentService } = await import('../src/services/paymentService')
  // 토스 API 호출 대신 기록만
  paymentService.confirmPayment = async (paymentKey: string, orderId: string, amount: number) => {
    confirmed.push({ paymentKey, orderId, amount })
    return { success: true, paymentKey, orderId }
  }
  paymentService.cancelPayment = async (paymentKey: string) => {
    cancelled.push(paymentKey)
    return { success: true, paymentKey }
  }
  // 구독 저장소는 DB 대신 메모리 목 (repository 계층 경계에서 교체)
  const { SubscriptionService } = await import('../src/services/subscriptionService')
  const { MemorySubscriptionRepository } = await import('./fakes/memorySubscriptionRepository')
  const subscriptionService = new SubscriptionService(new MemorySubscriptionRepository(), paymentService)
  const { createApp } = await import('../src/app')
  server = createApp({ subscriptionService }).listen(0)
  await new Promise(r => server.once('listening', r))
  url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
})

after(() => server?.close())

describe('payment routes require auth', () => {
  it('/plans is public', async () => {
    const r = await fetch(`${url}/api/payment/plans`)
    assert.equal(r.status, 200)
  })
  for (const [method, path] of [['POST', '/api/payment/confirm'], ['POST', '/api/payment/cancel'], ['POST', '/api/payment/change-plan'], ['GET', '/api/payment/subscription'], ['GET', '/api/payment/subscription/victim']] as const) {
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

describe('confirm', () => {
  it('ignores body userId/amount and uses the server price table', async () => {
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_alice', orderId: 'o1', planType: 'pro', amount: 1, userId: 'victim' }, bearer('alice'))
    assert.equal(r.status, 200)
    assert.deepEqual(confirmed.at(-1), { paymentKey: 'pk_alice', orderId: 'o1', amount: 6900 })

    const mine = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('alice') })).json() as any
    assert.equal(mine.user.id, 'alice')
    assert.equal(mine.user.planType, 'pro')
    assert.equal(mine.user.subscription.amount, 6900)

    const victim = await (await fetch(`${url}/api/payment/subscription`, { headers: bearer('victim') })).json() as any
    assert.equal(victim.user.planType, 'free')
  })

  it('rejects free/unknown plans', async () => {
    for (const planType of ['free', 'enterprise', undefined]) {
      const r = await post('/api/payment/confirm', { paymentKey: 'pk', orderId: 'o', planType }, bearer('alice'))
      assert.equal(r.status, 400)
    }
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
    await post('/api/payment/confirm', { paymentKey: 'pk_mallory', orderId: 'o2', planType: 'standard' }, bearer('mallory'))
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
    const first = await (await post('/api/payment/confirm', { paymentKey: 'pk_bob', orderId: 'o3', planType: 'standard' }, bearer('bob'))).json() as any
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_bob', orderId: 'o3', planType: 'standard' }, bearer('bob'))
    assert.equal(r.status, 200)
    const again = await r.json() as any
    assert.equal(again.subscription.id, first.subscription.id)
    assert.equal(confirmed.length, calls, 'Toss confirm must not be called again')
  })

  it("rejects another user's paymentKey/orderId with 409 before calling Toss", async () => {
    const calls = confirmed.length
    const r = await post('/api/payment/confirm', { paymentKey: 'pk_bob', orderId: 'o-other', planType: 'standard' }, bearer('eve'))
    assert.equal(r.status, 409)
    assert.equal(confirmed.length, calls)
  })
})
