import { ConflictError } from '../../src/repositories/errors'
import type { NewPaymentOrder, PaymentOrderRecord, PaymentOrderRepository } from '../../src/repositories/paymentOrderRepository'

/** 테스트 전용 메모리 주문 저장소 (009 마이그레이션 의미: order_id PK, confirm 시도 시각은 처음 값 유지) */
export class MemoryPaymentOrderRepository implements PaymentOrderRepository {
  orders: PaymentOrderRecord[] = []

  async createOrder(order: NewPaymentOrder) {
    if (this.orders.some(o => o.orderId === order.orderId)) throw new ConflictError('duplicate order_id', '23505')
    this.orders.push({ ...order, confirmAttemptedAt: null })
  }

  async findOrder(orderId: string) {
    const o = this.orders.find(o => o.orderId === orderId)
    return o ? { ...o } : null
  }

  async markConfirmAttempted(orderId: string, at: Date) {
    for (const o of this.orders) if (o.orderId === orderId && !o.confirmAttemptedAt) o.confirmAttemptedAt = at
  }

  async countOrdersCreatedSince(userId: string, since: Date) {
    return this.orders.filter(o => o.userId === userId && o.createdAt >= since).length
  }

  async countOpenOrders(userId: string, now: Date) {
    return this.orders.filter(o => o.userId === userId && !o.confirmAttemptedAt && o.expiresAt > now).length
  }

  async deleteStaleOrders(before: Date) {
    const n = this.orders.length
    this.orders = this.orders.filter(o => !(!o.confirmAttemptedAt && o.expiresAt < before))
    return n - this.orders.length
  }
}
