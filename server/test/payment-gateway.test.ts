/**
 * P3-5 (리뷰 H1): 토스 게이트웨이 — 결과 분류(확정 거절 vs 불명), 타임아웃, 멱등키, 조회
 */
import { describe, it, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import axios from 'axios'

process.env.TOSS_PAYMENTS_SECRET_KEY ||= 'test_sk_dummy'

const origPost = axios.post
const origGet = axios.get
afterEach(() => { axios.post = origPost; axios.get = origGet })

const httpErr = (status: number, code?: string) => Object.assign(new Error(`HTTP ${status}`), { response: { status, data: { code, message: code ?? 'err' } } })

describe('classifyTossError', () => {
  it('no response (timeout / socket reset) → unknown', async () => {
    const { classifyTossError } = await import('../src/services/paymentService')
    assert.equal(classifyTossError(Object.assign(new Error('timeout of 15000ms exceeded'), { code: 'ECONNABORTED' })).outcome, 'unknown')
  })
  it('5xx and uncertain codes → unknown; ordinary 4xx → rejected', async () => {
    const { classifyTossError } = await import('../src/services/paymentService')
    assert.equal(classifyTossError(httpErr(502)).outcome, 'unknown')
    assert.equal(classifyTossError(httpErr(400, 'ALREADY_PROCESSED_PAYMENT')).outcome, 'unknown')
    assert.equal(classifyTossError(httpErr(400, 'PROVIDER_ERROR')).outcome, 'unknown')
    const r = classifyTossError(httpErr(400, 'REJECT_CARD_COMPANY'))
    assert.equal(r.outcome, 'rejected')
    assert.equal(r.code, 'REJECT_CARD_COMPANY')
  })
})

describe('PaymentService (axios stubbed)', () => {
  it('confirm sends a timeout and an Idempotency-Key per order', async () => {
    const { paymentService, TOSS_TIMEOUT_MS } = await import('../src/services/paymentService')
    let seen: any
    axios.post = (async (_url: string, _body: unknown, config: any) => { seen = config; return { data: { paymentKey: 'pk', orderId: 'o1' } } }) as any
    const r = await paymentService.confirmPayment('pk', 'o1', 6900)
    assert.equal(r.outcome, 'approved')
    assert.equal(seen.timeout, TOSS_TIMEOUT_MS)
    assert.equal(seen.headers['Idempotency-Key'], 'confirm-o1')
  })
  it('confirm without a response → unknown (not a definitive failure)', async () => {
    const { paymentService } = await import('../src/services/paymentService')
    axios.post = (async () => { throw new Error('socket hang up') }) as any
    assert.equal((await paymentService.confirmPayment('pk', 'o1', 6900)).outcome, 'unknown')
  })
  it('lookup: maps fields, 404 → not found, other errors throw; paymentKey is URL-encoded', async () => {
    const { paymentService } = await import('../src/services/paymentService')
    let url = ''
    axios.get = (async (u: string, config: any) => {
      url = u
      assert.ok(config.timeout > 0)
      return { data: { paymentKey: 'a/b', orderId: 'o1', status: 'DONE', totalAmount: 6900, extra: 1 } }
    }) as any
    assert.deepEqual(await paymentService.lookupPayment('a/b'), { found: true, payment: { paymentKey: 'a/b', orderId: 'o1', status: 'DONE', totalAmount: 6900 } })
    assert.ok(url.endsWith('/payments/a%2Fb'))
    axios.get = (async () => { throw httpErr(404, 'NOT_FOUND_PAYMENT') }) as any
    assert.deepEqual(await paymentService.lookupPayment('x'), { found: false })
    axios.get = (async () => { throw new Error('timeout') }) as any
    await assert.rejects(paymentService.lookupPayment('x'))
  })
})
