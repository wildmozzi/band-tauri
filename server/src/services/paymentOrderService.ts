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
/** 사용자당 분당 주문 발급 수 상한 */
export const MAX_ORDERS_PER_MINUTE = 5
/** 사용자당 동시 미결 주문(만료 전·confirm 시도 없음) 상한 */
export const MAX_OPEN_ORDERS = 10
/** 미시도 주문은 만료 후 이 기간이 지나면 지운다 */
export const ORDER_RETENTION_MS = 24 * 60 * 60_000

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
    // 사용자당 발급 제한 (리뷰 #8 M1): 분당 발급 수, 동시 미결 주문 수
    const [recent, open] = await Promise.all([
      this.orders.countOrdersCreatedSince(userId, new Date(createdAt.getTime() - 60_000)),
      this.orders.countOpenOrders(userId, createdAt),
    ])
    if (recent >= MAX_ORDERS_PER_MINUTE || open >= MAX_OPEN_ORDERS) {
      console.warn(`[PAYMENT] 주문 발급 제한 (userId=${userId} recent=${recent} open=${open})`)
      throw new ServiceError(429, { success: false, error: '결제 요청이 너무 많습니다. 잠시 후 다시 시도해 주세요.' })
    }
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
    const payment = await this.subscriptions.findPaymentForOrder(order.orderId)
    // 금액의 기준은 주문 금액이다. 가격표 대조는 아직 결제 기록이 없을 때만 (가격 변경 직후 503 재시도를 막지 않게, 리뷰 #8 L3)
    const priceChanged = !payment && order.amount !== PLAN_PRICES[order.planType]
    if (order.planType !== input.planType || order.amount !== input.amount || priceChanged) {
      console.warn(`[PAYMENT] 주문 불일치 거부 (userId=${userId} orderId=${order.orderId} order=${order.planType}/${order.amount} req=${input.planType}/${input.amount})`)
      throw new ServiceError(400, { success: false, error: '주문 정보와 결제 정보가 일치하지 않습니다.' })
    }
    if (!payment && order.expiresAt.getTime() <= this.now().getTime()) {
      throw new ServiceError(410, { success: false, error: '주문이 만료되었습니다. 결제를 처음부터 다시 진행해 주세요.' })
    }
    // confirm 시도 표시는 pending 결제 행이 기록된 뒤에 한다 (리뷰 #8 M2).
    // 그래서 "시도 표시 있음 + 결제 행 없음"은 결제 행이 있다가 지워진 경우(거절·미승인·보상 취소)뿐이다 → failed.
    // 표시 전에 DB 오류가 나면 상태는 created로 남아 클라이언트가 confirm을 다시 보낸다(멱등).
    return this.subscriptions.confirmPayment(userId, input.paymentKey, order.orderId, order.planType, {
      amount: order.amount,
      onPendingRecorded: order.confirmAttemptedAt ? undefined : () => this.orders.markConfirmAttempted(order.orderId, this.now()),
    })
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
    // 시도 표시는 pending 행 기록 뒤에만 남으므로, 표시가 있는데 행이 없으면 행이 지워진 것 = 실패 확정
    if (order.confirmAttemptedAt) return { ...view, status: 'failed' }
    if (order.expiresAt.getTime() <= this.now().getTime()) return { ...view, status: 'expired' }
    return { ...view, status: 'created' }
  }

  /** 만료 후 ORDER_RETENTION_MS가 지난 미시도 주문 삭제 (시도한 주문은 결제 상태 조회용으로 남김) */
  async cleanupStaleOrders(): Promise<number> {
    return this.orders.deleteStaleOrders(new Date(this.now().getTime() - ORDER_RETENTION_MS))
  }
}

const ORDER_CLEANUP_INTERVAL_MS = 60 * 60_000

/** 주기 작업 (index.ts): 미결 만료 주문 정리 1시간마다 */
export function scheduleOrderJobs(service: PaymentOrderService) {
  setInterval(() => {
    service.cleanupStaleOrders().then(
      (n) => { if (n > 0) console.log(`[PAYMENT] 만료 주문 ${n}건 정리`) },
      (err) => console.error('[PAYMENT] 만료 주문 정리 실패:', err instanceof Error ? err.message : String(err)),
    )
  }, ORDER_CLEANUP_INTERVAL_MS).unref()
}

/** 운영 기본 인스턴스: Supabase 미설정이면 주문 API가 503 */
export function createDefaultPaymentOrderService(subscriptions: SubscriptionService): PaymentOrderService {
  const repo = supabaseAdmin ? createSupabasePaymentOrderRepository(supabaseAdmin) : createUnavailablePaymentOrderRepository()
  return new PaymentOrderService(repo, subscriptions)
}
