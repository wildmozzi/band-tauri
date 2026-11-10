/**
 * P3-5: SubscriptionService 단위 테스트 (repository·결제대행 목)
 * - 금액은 서버 가격표, 멱등성, 승인 후 DB 실패 시 재시도→보상 취소, 저장소 불가 시 승인 안 함
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { SubscriptionService, type PaymentGateway } from '../src/services/subscriptionService'
import { ServiceError } from '../src/services/errors'
import { ConflictError, RepositoryError, StoreUnavailableError } from '../src/repositories/errors'
import { MemorySubscriptionRepository } from './fakes/memorySubscriptionRepository'

function gateway(opts: { confirmOk?: boolean; cancelOk?: boolean } = {}) {
  const calls = { confirm: [] as [string, string, number][], cancel: [] as [string, string][] }
  const g: PaymentGateway = {
    async confirmPayment(k, o, a) {
      calls.confirm.push([k, o, a])
      return opts.confirmOk === false ? { success: false, error: 'toss says no' } : { success: true, paymentKey: k, orderId: o }
    },
    async cancelPayment(k, r) {
      calls.cancel.push([k, r])
      return opts.cancelOk === false ? { success: false, error: 'cancel failed' } : { success: true, paymentKey: k }
    },
  }
  return { g, calls }
}

/** recordConfirmedPayment가 failTimes번 실패하는 저장소 */
function flakyRepo(failTimes: number, err: () => Error = () => new RepositoryError('timeout')) {
  const repo = new MemorySubscriptionRepository()
  let n = 0
  const orig = repo.recordConfirmedPayment.bind(repo)
  repo.recordConfirmedPayment = async (input) => {
    n++
    if (n <= failTimes) throw err()
    return orig(input)
  }
  return { repo, attempts: () => n }
}

const opts = { retryDelaysMs: [0, 0] }

async function rejectsWith(p: Promise<unknown>, status: number) {
  await assert.rejects(p, (e: unknown) => e instanceof ServiceError && e.status === status)
}

describe('SubscriptionService.confirmPayment', () => {
  it('charges the server price and records a 30-day active subscription', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g, calls } = gateway()
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.deepEqual(calls.confirm, [['pk1', 'o1', 6900]])
    assert.equal(sub.amount, 6900)
    assert.equal(sub.status, 'active')
    assert.equal(Math.round((sub.endDate.getTime() - sub.startDate.getTime()) / 86_400_000), 30)
    assert.equal(repo.payments.length, 1)
  })

  it('Toss failure → 400 with the gateway error, nothing recorded', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g } = gateway({ confirmOk: false })
    await assert.rejects(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'),
      (e: unknown) => e instanceof ServiceError && e.status === 400 && e.body.error === 'toss says no')
    assert.equal(repo.subscriptions.length, 0)
  })

  it('same request twice → same subscription, Toss called once (idempotent)', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g, calls } = gateway()
    const svc = new SubscriptionService(repo, g, opts)
    const a = await svc.confirmPayment('u1', 'pk1', 'o1', 'standard')
    const b = await svc.confirmPayment('u1', 'pk1', 'o1', 'standard')
    assert.equal(a.id, b.id)
    assert.equal(calls.confirm.length, 1)
  })

  it('paymentKey/orderId owned by someone else, or plan mismatch → 409 without Toss', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g, calls } = gateway()
    const svc = new SubscriptionService(repo, g, opts)
    await svc.confirmPayment('u1', 'pk1', 'o1', 'standard')
    await rejectsWith(svc.confirmPayment('u2', 'pk1', 'o1', 'standard'), 409)
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'pro'), 409)
    await rejectsWith(svc.confirmPayment('u1', 'pk2', 'o1', 'standard'), 409)
    assert.equal(calls.confirm.length, 1)
  })

  it('store unavailable → StoreUnavailableError before Toss is called', async () => {
    const repo = new MemorySubscriptionRepository()
    repo.findPaymentsByKeyOrOrder = async () => { throw new StoreUnavailableError('no table', '42P01') }
    const { g, calls } = gateway()
    await assert.rejects(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), StoreUnavailableError)
    assert.equal(calls.confirm.length, 0)
  })

  it('transient DB failure after approval is retried and succeeds', async () => {
    const { repo, attempts } = flakyRepo(2)
    const { g, calls } = gateway()
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.equal(attempts(), 3)
    assert.equal(sub.status, 'active')
    assert.equal(calls.cancel.length, 0)
  })

  it('DB failure after all retries → compensating Toss cancel, 500', async () => {
    const { repo, attempts } = flakyRepo(99)
    const { g, calls } = gateway()
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 500)
    assert.equal(attempts(), 3)
    assert.deepEqual(calls.cancel.map(c => c[0]), ['pk1'])
    assert.equal(repo.subscriptions.length, 0)
  })

  it('conflict at record time is not retried but still compensated', async () => {
    const { repo, attempts } = flakyRepo(99, () => new ConflictError('dup', '23505'))
    const { g, calls } = gateway()
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 500)
    assert.equal(attempts(), 1)
    assert.equal(calls.cancel.length, 1)
  })

  it('concurrent duplicate: conflict because the same payment was just recorded → returns it, no refund', async () => {
    const repo = new MemorySubscriptionRepository()
    const orig = repo.recordConfirmedPayment.bind(repo)
    repo.recordConfirmedPayment = async (input) => {
      await orig(input) // 다른 요청이 먼저 커밋
      throw new ConflictError('dup', '23505')
    }
    const { g, calls } = gateway()
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.equal(sub.paymentKey, 'pk1')
    assert.equal(calls.cancel.length, 0)
  })

  it('conflict on a paymentKey recorded for someone else → 409, never cancels that payment', async () => {
    const repo = new MemorySubscriptionRepository()
    const orig = repo.recordConfirmedPayment.bind(repo)
    repo.recordConfirmedPayment = async (input) => {
      await orig({ ...input, userId: 'other' })
      throw new ConflictError('dup', '23505')
    }
    const { g, calls } = gateway()
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 409)
    assert.equal(calls.cancel.length, 0)
  })

  it('store disappears after approval → compensated, 503', async () => {
    const { repo } = flakyRepo(99, () => new StoreUnavailableError('gone', 'PGRST202'))
    const { g, calls } = gateway()
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 503)
    assert.equal(calls.cancel.length, 1)
  })

  it('compensation itself fails → 500 (logged as CRITICAL for manual action)', async () => {
    const { repo } = flakyRepo(99)
    const { g, calls } = gateway({ cancelOk: false })
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 500)
    assert.equal(calls.cancel.length, 1)
  })
})

describe('SubscriptionService.cancelPayment', () => {
  async function withActive() {
    const repo = new MemorySubscriptionRepository()
    const gw = gateway()
    const svc = new SubscriptionService(repo, gw.g, opts)
    const sub = await svc.confirmPayment('u1', 'pk1', 'o1', 'pro')
    return { repo, svc, sub, calls: gw.calls }
  }

  it('cancels own active subscription and marks payment cancelled', async () => {
    const { repo, svc, calls } = await withActive()
    await svc.cancelPayment('u1', 'pk1', 'reason')
    assert.deepEqual(calls.cancel, [['pk1', 'reason']])
    assert.equal(repo.subscriptions[0].status, 'cancelled')
    assert.equal(repo.payments[0].status, 'cancelled')
    assert.equal((await svc.getUserSubscription('u1')).planType, 'free')
  })

  it('404 without active subscription, 403 on foreign paymentKey', async () => {
    const { svc, calls } = await withActive()
    await rejectsWith(svc.cancelPayment('u2', undefined, 'r'), 404)
    await rejectsWith(svc.cancelPayment('u1', 'pk_other', 'r'), 403)
    assert.equal(calls.cancel.length, 0)
  })

  it('DB failure after Toss cancel → retried, then 500 (reconcile log)', async () => {
    const { repo, svc, calls } = await withActive()
    let n = 0
    repo.cancelSubscription = async () => { n++; throw new RepositoryError('timeout') }
    await rejectsWith(svc.cancelPayment('u1', undefined, 'r'), 500)
    assert.equal(n, 3)
    assert.equal(calls.cancel.length, 1)
  })
})

describe('SubscriptionService queries', () => {
  it('free user view / downgrade / expiry', async () => {
    let now = new Date('2026-01-01T00:00:00Z')
    const repo = new MemorySubscriptionRepository()
    const svc = new SubscriptionService(repo, gateway().g, { ...opts, now: () => now })
    assert.deepEqual(await svc.getUserSubscription('u1'), { id: 'u1', planType: 'free', subscription: null })

    await svc.confirmPayment('u1', 'pk1', 'o1', 'standard')
    const view = await svc.getUserSubscription('u1')
    assert.equal(view.planType, 'standard')
    assert.equal(view.subscription?.amount, 2900)

    await svc.downgradeToFree('u1')
    assert.equal((await svc.getUserSubscription('u1')).planType, 'free')
    assert.equal(repo.payments[0].status, 'confirmed', 'downgrade does not refund (existing behavior)')

    await svc.confirmPayment('u1', 'pk2', 'o2', 'pro')
    now = new Date(Date.now() + 31 * 86_400_000)
    assert.equal((await svc.getUserSubscription('u1')).planType, 'free', 'expired by end_date')
    assert.equal(await svc.expireDueSubscriptions(), 1)
  })
})

