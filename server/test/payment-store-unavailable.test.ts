/**
 * P3-5: 구독 저장소(마이그레이션 007 미실행 또는 Supabase 미설정)를 쓸 수 없으면
 * 결제 API는 503이고, 토스 승인을 호출하지 않는다 (메모리 폴백 금지).
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import type { Server } from 'http'
import { bearer, testVerifier } from './helpers'

let server: Server
let url = ''
let tossCalls = 0

before(async () => {
  const auth = await import('../src/lib/auth')
  auth.setTokenVerifier(testVerifier)
  const { SubscriptionService } = await import('../src/services/subscriptionService')
  const { createUnavailableSubscriptionRepository } = await import('../src/repositories/subscriptionRepository')
  const gateway = {
    confirmPayment: async () => { tossCalls++; return { success: true } },
    cancelPayment: async () => { tossCalls++; return { success: true } },
    lookupPayment: async () => { tossCalls++; return { found: false as const } },
  }
  const subscriptionService = new SubscriptionService(createUnavailableSubscriptionRepository('test'), gateway)
  const { createApp } = await import('../src/app')
  server = createApp({ subscriptionService }).listen(0)
  await new Promise(r => server.once('listening', r))
  url = `http://127.0.0.1:${(server.address() as { port: number }).port}`
})

after(() => server?.close())

const post = (path: string, body: unknown) =>
  fetch(`${url}${path}`, { method: 'POST', headers: { 'Content-Type': 'application/json', ...bearer('alice') }, body: JSON.stringify(body) })

describe('payment API without subscription store', () => {
  it('confirm → 503 and Toss is never called', async () => {
    const r = await post('/api/payment/confirm', { paymentKey: 'pk', orderId: 'o1', planType: 'pro' })
    assert.equal(r.status, 503)
    assert.equal(((await r.json()) as any).success, false)
    assert.equal(tossCalls, 0)
  })
  it('cancel / change-plan / subscription → 503', async () => {
    assert.equal((await post('/api/payment/cancel', {})).status, 503)
    assert.equal((await post('/api/payment/change-plan', { newPlan: 'free' })).status, 503)
    assert.equal((await fetch(`${url}/api/payment/subscription`, { headers: bearer('alice') })).status, 503)
    assert.equal(tossCalls, 0)
  })
  it('/plans still works (no DB)', async () => {
    assert.equal((await fetch(`${url}/api/payment/plans`)).status, 200)
  })
})
