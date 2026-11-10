import { randomUUID } from 'crypto'
import { ConflictError } from '../../src/repositories/errors'
import type {
  PaymentIntent,
  PaymentRecord,
  RecordPaymentInput,
  SubscriptionRecord,
  SubscriptionRepository,
} from '../../src/repositories/subscriptionRepository'

/**
 * 테스트 전용 메모리 저장소 — 007 마이그레이션의 DB 함수 의미(pending 전환, 멱등, 유니크 키, 한 트랜잭션)를 흉내 낸다.
 * SQL 자체는 test/migration-007.test.ts가 실제 Postgres(PGlite)로 검증한다.
 * 운영 코드에서는 쓰지 않는다 (결제 정합성 때문에 메모리 폴백 금지).
 */
export class MemorySubscriptionRepository implements SubscriptionRepository {
  subscriptions: SubscriptionRecord[] = []
  payments: PaymentRecord[] = []

  async findPaymentsByKeyOrOrder(paymentKey: string, orderId: string) {
    return this.payments.filter(p => p.paymentKey === paymentKey || p.orderId === orderId)
  }

  async createPendingPayment(intent: PaymentIntent, createdAt = new Date()) {
    if (this.payments.some(p => p.paymentKey === intent.paymentKey || p.orderId === intent.orderId)) {
      throw new ConflictError('duplicate key', '23505')
    }
    this.payments.push({ id: randomUUID(), ...intent, status: 'pending', subscriptionId: null, createdAt })
  }

  async deletePendingPayment(orderId: string) {
    this.payments = this.payments.filter(p => !(p.orderId === orderId && p.status === 'pending'))
  }

  async listPendingPayments(createdBefore: Date, limit: number) {
    return this.payments
      .filter(p => p.status === 'pending' && p.createdAt < createdBefore)
      .sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime())
      .slice(0, limit)
  }

  async recordConfirmedPayment(input: RecordPaymentInput) {
    const row = this.payments.find(p => p.orderId === input.orderId)
    if (row) {
      if (row.userId !== input.userId || row.paymentKey !== input.paymentKey || row.planType !== input.planType || row.amount !== input.amount) {
        throw new ConflictError('payment conflict', '23505')
      }
      if (row.status === 'confirmed') {
        const sub = this.subscriptions.find(s => s.id === row.subscriptionId)
        if (sub) return sub
      }
      if (row.status !== 'pending') throw new ConflictError(`payment already ${row.status}`, '23505')
      const sub = this.newSubscription(input)
      row.status = 'confirmed'
      row.subscriptionId = sub.id
      return sub
    }
    if (this.payments.some(p => p.paymentKey === input.paymentKey)) throw new ConflictError('duplicate payment_key', '23505')
    const sub = this.newSubscription(input)
    this.payments.push({
      id: randomUUID(),
      userId: input.userId,
      orderId: input.orderId,
      paymentKey: input.paymentKey,
      planType: input.planType,
      amount: input.amount,
      status: 'confirmed',
      subscriptionId: sub.id,
      createdAt: new Date(),
    })
    return sub
  }

  private newSubscription(input: RecordPaymentInput): SubscriptionRecord {
    const now = new Date()
    const sub: SubscriptionRecord = {
      id: randomUUID(),
      userId: input.userId,
      planType: input.planType,
      status: 'active',
      startDate: now,
      endDate: new Date(now.getTime() + input.periodDays * 86_400_000),
      paymentKey: input.paymentKey,
      amount: input.amount,
      createdAt: now,
      updatedAt: now,
    }
    this.subscriptions.push(sub)
    return sub
  }

  async findSubscriptionById(id: string) {
    return this.subscriptions.find(s => s.id === id) ?? null
  }

  async findActiveSubscription(userId: string, now: Date) {
    const active = this.subscriptions
      .filter(s => s.userId === userId && s.status === 'active' && s.endDate > now)
      .sort((a, b) => b.createdAt.getTime() - a.createdAt.getTime())
    return active[0] ?? null
  }

  async cancelSubscription(subscriptionId: string, opts: { paymentCancelled: boolean; reason: string | null }) {
    const sub = this.subscriptions.find(s => s.id === subscriptionId && s.status === 'active')
    if (!sub) return false
    sub.status = 'cancelled'
    if (opts.paymentCancelled) {
      for (const p of this.payments) if (p.subscriptionId === sub.id && p.status === 'confirmed') p.status = 'cancelled'
    }
    return true
  }

  async expireDueSubscriptions(now: Date) {
    let n = 0
    for (const s of this.subscriptions) {
      if (s.status === 'active' && s.endDate <= now) { s.status = 'expired'; n++ }
    }
    return n
  }
}
