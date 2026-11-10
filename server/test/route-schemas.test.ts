/**
 * P3-5: 라우트 입력 스키마 (zod)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { cancelPaymentBody, changePlanBody, confirmPaymentBody, separationIdParams, uploadFilenameParams } from '../src/routes/schemas'

describe('route schemas', () => {
  it('confirm: requires short strings and a paid plan; strips userId/amount', () => {
    const ok = confirmPaymentBody.safeParse({ paymentKey: 'pk', orderId: 'o', planType: 'pro', amount: 1, userId: 'victim' })
    assert.ok(ok.success)
    assert.deepEqual(ok.data, { paymentKey: 'pk', orderId: 'o', planType: 'pro' })
    for (const bad of [
      { paymentKey: '', orderId: 'o', planType: 'pro' },
      { paymentKey: 'x'.repeat(201), orderId: 'o', planType: 'pro' },
      { paymentKey: 123, orderId: 'o', planType: 'pro' },
      { paymentKey: 'pk', orderId: 'o', planType: 'free' },
      { paymentKey: 'pk', orderId: 'o' },
    ]) assert.equal(confirmPaymentBody.safeParse(bad).success, false, JSON.stringify(bad).slice(0, 60))
  })

  it('cancel: paymentKey optional, non-string cancelReason falls back to default', () => {
    const empty = cancelPaymentBody.parse({})
    assert.equal(empty.paymentKey, undefined)
    assert.equal(empty.cancelReason, undefined)
    assert.equal(cancelPaymentBody.parse({ cancelReason: 42 }).cancelReason, undefined)
    assert.equal(cancelPaymentBody.safeParse({ paymentKey: 1 }).success, false)
  })

  it('change-plan: enum only', () => {
    assert.ok(changePlanBody.safeParse({ newPlan: 'free' }).success)
    assert.equal(changePlanBody.safeParse({ newPlan: ['free'] }).success, false)
    assert.equal(changePlanBody.safeParse({ newPlan: 'enterprise' }).success, false)
  })

  it('params', () => {
    assert.ok(separationIdParams.safeParse({ id: '0b6f2f4e-8d0e-4d4c-9f57-2f3c1b0c9a11' }).success)
    assert.equal(separationIdParams.safeParse({ id: '1; drop' }).success, false)
    assert.ok(uploadFilenameParams.safeParse({ filename: 'a.mid' }).success)
    assert.equal(uploadFilenameParams.safeParse({ filename: 'x'.repeat(256) }).success, false)
  })
})
