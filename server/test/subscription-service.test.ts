/**
 * P3-5: SubscriptionService 단위 테스트 (repository·결제대행 목)
 * - 금액은 서버 가격표, 멱등성, pending 행, 승인 결과 불명 → 조회, 재확인 후 보상, 재조정 작업
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { SubscriptionService, type PaymentGateway } from '../src/services/subscriptionService'
import { ServiceError } from '../src/services/errors'
import { ConflictError, RepositoryError, StoreUnavailableError } from '../src/repositories/errors'
import type { PaymentResponse, TossPaymentLookup } from '../src/types/payment'
import { MemorySubscriptionRepository } from './fakes/memorySubscriptionRepository'

interface GatewayOpts {
  /** 회차별 confirm 응답 (마지막 값 반복). 기본: 승인 */
  confirm?: (PaymentResponse | 'throw')[]
  cancelOk?: boolean
  /** 조회 결과. 'throw'면 조회 실패 */
  lookup?: TossPaymentLookup | 'throw' | ((paymentKey: string) => TossPaymentLookup)
}

function gateway(opts: GatewayOpts = {}) {
  const calls = { confirm: [] as [string, string, number][], cancel: [] as [string, string][], lookup: [] as string[] }
  const g: PaymentGateway = {
    async confirmPayment(k, o, a) {
      calls.confirm.push([k, o, a])
      const seq = opts.confirm ?? [{ success: true, outcome: 'approved' }]
      const r = seq[Math.min(calls.confirm.length - 1, seq.length - 1)]
      if (r === 'throw') throw new Error('socket hang up')
      return r.success ? { ...r, paymentKey: k, orderId: o } : r
    },
    async cancelPayment(k, r) {
      calls.cancel.push([k, r])
      return opts.cancelOk === false ? { success: false, error: 'cancel failed' } : { success: true, paymentKey: k }
    },
    async lookupPayment(k) {
      calls.lookup.push(k)
      const l = opts.lookup ?? { found: false }
      if (l === 'throw') throw new Error('lookup timeout')
      return typeof l === 'function' ? l(k) : l
    },
  }
  return { g, calls }
}

const done = (orderId: string, amount: number, status = 'DONE'): TossPaymentLookup =>
  ({ found: true, payment: { paymentKey: 'pk1', orderId, status, totalAmount: amount } })
const unknown = (code?: string): PaymentResponse => ({ success: false, outcome: 'unknown', code, error: code ?? 'timeout' })

/** recordConfirmedPayment가 failTimes번 실패하는 저장소. commitFirst면 1회차는 커밋 후 throw(응답 유실) */
function flakyRepo(failTimes: number, err: () => Error = () => new RepositoryError('timeout'), commitFirst = false) {
  const repo = new MemorySubscriptionRepository()
  let n = 0
  const orig = repo.recordConfirmedPayment.bind(repo)
  repo.recordConfirmedPayment = async (input) => {
    n++
    if (commitFirst && n === 1) { await orig(input); throw err() }
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
  it('charges the server price, writes a pending row first, then a 30-day active subscription', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g, calls } = gateway()
    const origConfirm = g.confirmPayment
    g.confirmPayment = async (k, o, a) => {
      assert.equal(repo.payments[0]?.status, 'pending', 'pending row must exist before calling Toss')
      return origConfirm(k, o, a)
    }
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.deepEqual(calls.confirm, [['pk1', 'o1', 6900]])
    assert.equal(sub.amount, 6900)
    assert.equal(sub.status, 'active')
    assert.equal(Math.round((sub.endDate.getTime() - sub.startDate.getTime()) / 86_400_000), 30)
    assert.equal(repo.payments.length, 1)
    assert.equal(repo.payments[0].status, 'confirmed')
  })

  it('Toss rejection → 400 with the gateway error, pending row removed', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g } = gateway({ confirm: [{ success: false, outcome: 'rejected', error: 'toss says no' }] })
    await assert.rejects(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'),
      (e: unknown) => e instanceof ServiceError && e.status === 400 && e.body.error === 'toss says no')
    assert.equal(repo.subscriptions.length, 0)
    assert.equal(repo.payments.length, 0)
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

  it('L2: replay after the subscription was cancelled → 409, not a success', async () => {
    const repo = new MemorySubscriptionRepository()
    const svc = new SubscriptionService(repo, gateway().g, opts)
    await svc.confirmPayment('u1', 'pk1', 'o1', 'standard')
    await svc.downgradeToFree('u1')
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'standard'), 409)
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
})

describe('H1: approval outcome unknown → verify by lookup', () => {
  it('response lost (no outcome) then retry gets ALREADY_PROCESSED + lookup DONE → 200, one subscription', async () => {
    const repo = new MemorySubscriptionRepository()
    // 1회차: 토스는 승인했지만 응답 유실 + 조회도 실패 → 503(재조정 대기). 2회차: ALREADY_PROCESSED + 조회 DONE
    let lookupWorks = false
    const { g, calls } = gateway({
      confirm: [unknown(), unknown('ALREADY_PROCESSED_PAYMENT')],
      lookup: () => { if (!lookupWorks) throw new Error('lookup down'); return done('o1', 6900) },
    })
    const svc = new SubscriptionService(repo, g, opts)
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'pro'), 503)
    assert.equal(repo.payments[0].status, 'pending', 'pending row is kept for reconcile')
    assert.equal(calls.cancel.length, 0)

    lookupWorks = true
    const sub = await svc.confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.equal(sub.status, 'active')
    assert.equal(repo.subscriptions.length, 1)
    assert.equal(repo.payments.length, 1)
    assert.equal(repo.payments[0].status, 'confirmed')
  })

  it('gateway throws (timeout) and lookup says DONE → recorded', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g } = gateway({ confirm: [unknown()], lookup: done('o1', 2900) })
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'standard')
    assert.equal(sub.amount, 2900)
  })

  it('lookup says not found / ABORTED → 400 and pending removed', async () => {
    for (const lookup of [{ found: false } as const, done('o1', 6900, 'ABORTED')]) {
      const repo = new MemorySubscriptionRepository()
      const { g } = gateway({ confirm: [unknown()], lookup })
      await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 400)
      assert.equal(repo.payments.length, 0)
    }
  })

  it('lookup DONE but for another order or amount → not treated as approved', async () => {
    for (const lookup of [done('other-order', 6900), done('o1', 100)]) {
      const repo = new MemorySubscriptionRepository()
      const { g } = gateway({ confirm: [unknown()], lookup })
      await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 400)
      assert.equal(repo.subscriptions.length, 0)
    }
  })

  it('lookup IN_PROGRESS → 503 (never 400), pending kept', async () => {
    const repo = new MemorySubscriptionRepository()
    const { g } = gateway({ confirm: [unknown('PROVIDER_ERROR')], lookup: done('o1', 6900, 'IN_PROGRESS') })
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 503)
    assert.equal(repo.payments[0].status, 'pending')
  })
})

describe('after approval: DB failure handling (M1)', () => {
  it('first write committed but its response was lost, later attempts fail → re-check finds it, no refund', async () => {
    const { repo } = flakyRepo(99, () => new RepositoryError('connection reset'), true)
    const { g, calls } = gateway()
    const sub = await new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.equal(sub.status, 'active')
    assert.equal(calls.cancel.length, 0)
  })

  it('persistent transient failure → 503, no refund, pending left for the reconciler which then records it', async () => {
    const { repo } = flakyRepo(3)
    const gw = gateway({ lookup: done('o1', 6900) })
    let now = new Date()
    const svc = new SubscriptionService(repo, gw.g, { ...opts, now: () => now })
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'pro'), 503)
    assert.equal(gw.calls.cancel.length, 0)
    assert.equal(repo.payments[0].status, 'pending')

    now = new Date(Date.now() + 5 * 60_000)
    const r = await svc.reconcilePendingPayments()
    assert.deepEqual(r, { recorded: 1, discarded: 0, unresolved: 0 })
    assert.equal((await svc.getUserSubscription('u1')).planType, 'pro')
  })

  it('DB unreachable for the re-check → no refund, 503', async () => {
    const { repo } = flakyRepo(99)
    const { g, calls } = gateway()
    const svc = new SubscriptionService(repo, g, opts)
    const origFind = repo.findPaymentsByKeyOrOrder.bind(repo)
    let finds = 0
    repo.findPaymentsByKeyOrOrder = async (k, o) => { if (++finds > 1) throw new RepositoryError('db down'); return origFind(k, o) }
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'pro'), 503)
    assert.equal(calls.cancel.length, 0)
  })

  it('deterministic conflict with no record of our paymentKey → compensating cancel, pending removed', async () => {
    const { repo, attempts } = flakyRepo(99, () => new ConflictError('dup', '23505'))
    const { g, calls } = gateway()
    const svc = new SubscriptionService(repo, g, opts)
    // pending 행이 사라진 상황 (재확인 결과 none)
    repo.createPendingPayment = async () => {}
    await rejectsWith(svc.confirmPayment('u1', 'pk1', 'o1', 'pro'), 500)
    assert.equal(attempts(), 1, 'conflicts are not retried')
    assert.deepEqual(calls.cancel.map(c => c[0]), ['pk1'])
  })

  it('compensation itself fails → 500 (CRITICAL)', async () => {
    const { repo } = flakyRepo(99, () => new ConflictError('dup', '23505'))
    repo.createPendingPayment = async () => {}
    const { g, calls } = gateway({ cancelOk: false })
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 500)
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
    repo.createPendingPayment = async () => {}
    const orig = repo.recordConfirmedPayment.bind(repo)
    repo.recordConfirmedPayment = async (input) => {
      await orig({ ...input, userId: 'other' })
      throw new ConflictError('dup', '23505')
    }
    const { g, calls } = gateway()
    await rejectsWith(new SubscriptionService(repo, g, opts).confirmPayment('u1', 'pk1', 'o1', 'pro'), 409)
    assert.equal(calls.cancel.length, 0)
  })
})

describe('reconcilePendingPayments (server restart recovery)', () => {
  async function seed(repo: MemorySubscriptionRepository, orderId: string, paymentKey: string, ageMs: number) {
    await repo.createPendingPayment({ userId: 'u1', orderId, paymentKey, planType: 'pro', amount: 6900 }, new Date(Date.now() - ageMs))
  }

  it('DONE → recorded, ABORTED/not found → discarded, lookup failure → unresolved, fresh rows skipped', async () => {
    const repo = new MemorySubscriptionRepository()
    await seed(repo, 'o-done', 'pk-done', 10 * 60_000)
    await seed(repo, 'o-aborted', 'pk-aborted', 10 * 60_000)
    await seed(repo, 'o-gone', 'pk-gone', 10 * 60_000)
    await seed(repo, 'o-err', 'pk-err', 10 * 60_000)
    await seed(repo, 'o-fresh', 'pk-fresh', 10_000)
    const { g, calls } = gateway({
      lookup: (k) => {
        if (k === 'pk-done') return { found: true, payment: { paymentKey: k, orderId: 'o-done', status: 'DONE', totalAmount: 6900 } }
        if (k === 'pk-aborted') return { found: true, payment: { paymentKey: k, orderId: 'o-aborted', status: 'ABORTED', totalAmount: 6900 } }
        if (k === 'pk-err') throw new Error('timeout')
        return { found: false }
      },
    })
    const svc = new SubscriptionService(repo, g, opts)
    assert.deepEqual(await svc.reconcilePendingPayments(), { recorded: 1, discarded: 2, unresolved: 1 })
    assert.ok(!calls.lookup.includes('pk-fresh'), 'rows younger than the min age are left to the in-flight request')
    assert.deepEqual(repo.payments.map(p => [p.orderId, p.status]).sort(), [
      ['o-done', 'confirmed'], ['o-err', 'pending'], ['o-fresh', 'pending'],
    ])
    assert.equal((await svc.getUserSubscription('u1')).planType, 'pro')
    // 다시 실행해도 같은 결제를 두 번 기록하지 않는다
    assert.deepEqual(await svc.reconcilePendingPayments(), { recorded: 0, discarded: 0, unresolved: 1 })
    assert.equal(repo.subscriptions.length, 1)
  })

  it('after reconcile, the client retry returns the recorded subscription', async () => {
    const repo = new MemorySubscriptionRepository()
    await seed(repo, 'o1', 'pk1', 10 * 60_000)
    const { g, calls } = gateway({ lookup: done('o1', 6900) })
    const svc = new SubscriptionService(repo, g, opts)
    await svc.reconcilePendingPayments()
    const sub = await svc.confirmPayment('u1', 'pk1', 'o1', 'pro')
    assert.equal(sub.status, 'active')
    assert.equal(calls.confirm.length, 0)
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

  it('L1: first cancel committed but response lost, retry returns false → treated as success', async () => {
    const { repo, svc } = await withActive()
    const orig = repo.cancelSubscription.bind(repo)
    let n = 0
    repo.cancelSubscription = async (id, o) => {
      n++
      const r = await orig(id, o)
      if (n === 1) throw new RepositoryError('connection reset')
      return r
    }
    await svc.cancelPayment('u1', undefined, 'r')
    assert.equal(n, 2)
    assert.equal(repo.subscriptions[0].status, 'cancelled')
  })
})

describe('SubscriptionService queries', () => {
  it('free user view / downgrade / expiry', async () => {
    let now = new Date('2026-01-01T00:00:00Z')
    const repo = new MemorySubscriptionRepository()
    const svc = new SubscriptionService(repo, gateway().g, { ...opts, now: () => now })
    assert.deepEqual(await svc.getUserSubscription('u1'), { id: 'u1', planType: 'free', subscription: null })

    now = new Date()
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
