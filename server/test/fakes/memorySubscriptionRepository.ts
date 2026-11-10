import { randomUUID } from 'crypto'
import { ConflictError } from '../../src/repositories/errors'
import type {
  PaymentRecord,
  SubscriptionRecord,
  SubscriptionRepository,
} from '../../src/repositories/subscriptionRepository'

/**
 * 테스트 전용 메모리 저장소 — 007 마이그레이션의 DB 함수 의미(멱등, 유니크 키, 한 트랜잭션)를 흉내 낸다.
 * 운영 코드에서는 쓰지 않는다 (결제 정합성 때문에 메모리 폴백 금지).
 */
export class MemorySubscriptionRepository implements SubscriptionRepository {
  subscriptions: SubscriptionRecord[] = []
  payments: PaymentRecord[] = []

  async findPaymentsByKeyOrOrder(paymentKey: string, orderId: string) {
    return this.payments.filter(p => p.paymentKey === paymentKey || p.orderId === orderId)
  }

  async recordConfirmedPayment(input: Parameters<SubscriptionRepository['recordConfirmedPayment']>[0]) {
    const dup = this.payments.find(p => p.paymentKey === input.paymentKey || p.orderId === input.orderId)
    if (dup) {
      if (dup.userId === input.userId && dup.paymentKey === input.paymentKey && dup.orderId === input.orderId && dup.subscriptionId) {
        return this.subscriptions.find(s => s.id === dup.subscriptionId)!
      }
      throw new ConflictError('payment already recorded', '23505')
    }
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
    this.payments.push({
      id: randomUUID(),
      userId: input.userId,
      orderId: input.orderId,
      paymentKey: input.paymentKey,
      planType: input.planType,
      amount: input.amount,
      status: 'confirmed',
      subscriptionId: sub.id,
    })
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
      for (const p of this.payments) if (p.subscriptionId === sub.id) p.status = 'cancelled'
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
