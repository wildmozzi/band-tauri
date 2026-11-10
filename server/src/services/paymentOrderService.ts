import { randomBytes } from 'crypto'
import { PLAN_PRICES } from '../types/payment'
import { ConflictError } from '../repositories/errors'
import type { PaidPlan, SubscriptionRecord } from '../repositories/subscriptionRepository'
import {
  createSupabasePaymentOrderRepository,
  createUnavailablePaymentOrderRepository,
  type PaymentOrderRecord,
  type PaymentOrderRepository,
} from '../repositories/paymentOrderRepository'
import { supabaseAdmin } from '../lib/supabase'
import { ServiceError } from './errors'
import type { SubscriptionService } from './subscriptionService'

/** 주문 유효 시간: 이 안에 토스 결제를 마치고 confirm해야 한다 */
export const ORDER_TTL_MS = 30 * 60_000

/**
 * 주문 상태 (GET /orders/:orderId, 클라이언트 계약)
 * - created: 발급됨, 아직 결제 승인 시도 없음
 * - processing: 승인 결과 확인 중 (pending/manual_review 결제 행)
 * - paid: 승인·구독 기록 완료
 * - failed: 승인 시도가 있었지만 결제 기록이 없음 (거절·미승인 확정·보상 취소)
 * - cancelled: 결제 후 취소됨
 * - expired: 결제 없이 만료
 */
export type OrderStatus = 'created' | 'processing' | 'paid' | 'failed' | 'cancelled' | 'expired'

export interface OrderView {
  orderId: string
  planType: PaidPlan
  amount: number
  expiresAt: Date
}

export interface OrderStatusView extends OrderView {
  status: OrderStatus
  subscription?: {
    id: string
    planType: PaidPlan
    status: string
    startDate: Date
    endDate: Date
  }
}

export interface ConfirmInput {
  paymentKey: string
  orderId: string
  amount: number
  planType: PaidPlan
}

export interface PaymentOrderServiceOptions {
  now?: () => Date
  /** 테스트용 orderId 생성기 */
  newOrderId?: () => string
}

/** 토스 orderId 규칙(영문·숫자·-·_, 6~64자)에 맞는 추측 불가능한 ID */
export function generateOrderId(): string {
  return `ord_${randomBytes(16).toString('hex')}`
}

const NOT_FOUND = () => new ServiceError(404, { success: false, error: '주문을 찾을 수 없습니다. 결제를 처음부터 다시 진행해 주세요.' })

/**
 * 결제 주문 — orderId는 서버만 발급한다 (P3-5b).
 * - 발급: 인증 사용자 + 플랜 → orderId, 금액은 서버 가격표, 만료 30분
 * - confirm: 그 사용자의 주문이고 플랜·금액이 일치할 때만 SubscriptionService.confirmPayment로 진행.
 *   클라이언트가 만든 orderId는 주문이 없으므로 404.
 * - 만료는 "아직 결제 기록이 없는" 주문에만 적용한다 (503 뒤 같은 confirm 재시도·멱등 재요청은 허용)
 */
export class PaymentOrderService {
  private readonly now: () => Date
  private readonly newOrderId: () => string

  constructor(
    private readonly orders: PaymentOrderRepository,
    private readonly subscriptions: SubscriptionService,
    opts: PaymentOrderServiceOptions = {},
  ) {
    this.now = opts.now ?? (() => new Date())
    this.newOrderId = opts.newOrderId ?? generateOrderId
  }

  async createOrder(userId: string, planType: PaidPlan): Promise<OrderView> {
    const createdAt = this.now()
    const order = {
      orderId: this.newOrderId(),
      userId,
      planType,
      amount: PLAN_PRICES[planType],
      createdAt,
      expiresAt: new Date(createdAt.getTime() + ORDER_TTL_MS),
    }
    try {
      await this.orders.createOrder(order)
    } catch (err) {
      // 128비트 난수 충돌은 사실상 없지만, 나면 재시도 대신 오류로 (조용한 덮어쓰기 금지)
      if (err instanceof ConflictError) throw new ServiceError(500, { success: false, error: '주문 생성에 실패했습니다. 다시 시도해 주세요.' })
      throw err
    }
    return { orderId: order.orderId, planType, amount: order.amount, expiresAt: order.expiresAt }
  }

  /** 본인 주문만. 남의 주문·없는 주문은 구분하지 않고 404 */
  private async ownOrder(userId: string, orderId: string): Promise<PaymentOrderRecord> {
    const order = await this.orders.findOrder(orderId)
    if (!order || order.userId !== userId) throw NOT_FOUND()
    return order
  }

  async confirm(userId: string, input: ConfirmInput): Promise<SubscriptionRecord> {
    const order = await this.ownOrder(userId, input.orderId)
    if (order.planType !== input.planType || order.amount !== input.amount || order.amount !== PLAN_PRICES[order.planType]) {
      console.warn(`[PAYMENT] 주문 불일치 거부 (userId=${userId} orderId=${order.orderId} order=${order.planType}/${order.amount} req=${input.planType}/${input.amount})`)
      throw new ServiceError(400, { success: false, error: '주문 정보와 결제 정보가 일치하지 않습니다.' })
    }
    const payment = await this.subscriptions.findPaymentForOrder(order.orderId)
    if (!payment && order.expiresAt.getTime() <= this.now().getTime()) {
      throw new ServiceError(410, { success: false, error: '주문이 만료되었습니다. 결제를 처음부터 다시 진행해 주세요.' })
    }
    if (!order.confirmAttemptedAt) await this.orders.markConfirmAttempted(order.orderId, this.now())
    return this.subscriptions.confirmPayment(userId, input.paymentKey, order.orderId, order.planType)
  }

  async getOrderStatus(userId: string, orderId: string): Promise<OrderStatusView> {
    const order = await this.ownOrder(userId, orderId)
    const view: OrderView = { orderId: order.orderId, planType: order.planType, amount: order.amount, expiresAt: order.expiresAt }

    // pending이면 주기 작업을 기다리지 않고 토스 조회로 앞당겨 확정 시도
    const payment = await this.subscriptions.reconcileOrderPayment(order.orderId)
    if (payment && payment.userId === userId) {
      if (payment.status === 'confirmed' && payment.subscriptionId) {
        const sub = await this.subscriptions.findSubscription(payment.subscriptionId)
        return {
          ...view,
          status: 'paid',
          subscription: sub ? { id: sub.id, planType: sub.planType, status: sub.status, startDate: sub.startDate, endDate: sub.endDate } : undefined,
        }
      }
      if (payment.status === 'cancelled') return { ...view, status: 'cancelled' }
      return { ...view, status: 'processing' } // pending / manual_review
    }
    if (order.confirmAttemptedAt) return { ...view, status: 'failed' }
    if (order.expiresAt.getTime() <= this.now().getTime()) return { ...view, status: 'expired' }
    return { ...view, status: 'created' }
  }
}

/** 운영 기본 인스턴스: Supabase 미설정이면 주문 API가 503 */
export function createDefaultPaymentOrderService(subscriptions: SubscriptionService): PaymentOrderService {
  const repo = supabaseAdmin ? createSupabasePaymentOrderRepository(supabaseAdmin) : createUnavailablePaymentOrderRepository()
  return new PaymentOrderService(repo, subscriptions)
}
