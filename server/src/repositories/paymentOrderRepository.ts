import type { SupabaseClient } from '@supabase/supabase-js'
import { StoreUnavailableError, toRepositoryError } from './errors'
import type { PaidPlan } from './subscriptionRepository'

/**
 * payment_orders 테이블 접근 (docs/migrations/009_payment_orders.sql).
 * 서버가 발급한 결제 주문: orderId ↔ 사용자·플랜·금액·만료. 쓰기는 service_role로만(RLS).
 */

export interface PaymentOrderRecord {
  orderId: string
  userId: string
  planType: PaidPlan
  amount: number
  expiresAt: Date
  /** confirm을 한 번이라도 시도한 시각 (없으면 null) */
  confirmAttemptedAt: Date | null
  createdAt: Date
}

export interface NewPaymentOrder {
  orderId: string
  userId: string
  planType: PaidPlan
  amount: number
  expiresAt: Date
  createdAt: Date
}

export interface PaymentOrderRepository {
  /** 주문 생성. orderId가 이미 있으면 ConflictError */
  createOrder(order: NewPaymentOrder): Promise<void>
  findOrder(orderId: string): Promise<PaymentOrderRecord | null>
  /** confirm 시도 시각 기록 (이미 있으면 유지) */
  markConfirmAttempted(orderId: string, at: Date): Promise<void>
}

interface PaymentOrderRow {
  order_id: string
  user_id: string
  plan_type: PaidPlan
  amount: number
  expires_at: string
  confirm_attempted_at: string | null
  created_at: string
}

function orderFromRow(r: PaymentOrderRow): PaymentOrderRecord {
  return {
    orderId: r.order_id,
    userId: r.user_id,
    planType: r.plan_type,
    amount: r.amount,
    expiresAt: new Date(r.expires_at),
    confirmAttemptedAt: r.confirm_attempted_at ? new Date(r.confirm_attempted_at) : null,
    createdAt: new Date(r.created_at),
  }
}

const MIGRATION_HINT = 'payment_orders 테이블이 없습니다 — docs/migrations/009_payment_orders.sql 실행 필요'

function fail(err: { message: string; code?: string }, context: string): never {
  const e = toRepositoryError(err, context)
  if (e instanceof StoreUnavailableError) console.error(`[DB][PAYMENT] ${MIGRATION_HINT} (${e.message})`)
  throw e
}

export function createSupabasePaymentOrderRepository(supabase: SupabaseClient): PaymentOrderRepository {
  return {
    async createOrder(order) {
      const { error } = await supabase.from('payment_orders').insert({
        order_id: order.orderId,
        user_id: order.userId,
        plan_type: order.planType,
        amount: order.amount,
        expires_at: order.expiresAt.toISOString(),
        created_at: order.createdAt.toISOString(),
      })
      if (error) fail(error, 'payment_orders.create')
    },

    async findOrder(orderId) {
      const { data, error } = await supabase.from('payment_orders').select('*').eq('order_id', orderId).maybeSingle()
      if (error) fail(error, 'payment_orders.find')
      return data ? orderFromRow(data as PaymentOrderRow) : null
    },

    async markConfirmAttempted(orderId, at) {
      const { error } = await supabase.from('payment_orders')
        .update({ confirm_attempted_at: at.toISOString() })
        .eq('order_id', orderId)
        .is('confirm_attempted_at', null)
      if (error) fail(error, 'payment_orders.markConfirmAttempted')
    },
  }
}

/** 백엔드(Supabase) 미설정: 모든 호출이 StoreUnavailableError */
export function createUnavailablePaymentOrderRepository(reason = 'Supabase가 설정되지 않았습니다'): PaymentOrderRepository {
  const unavailable = async (): Promise<never> => {
    console.error(`[DB][PAYMENT] 주문 저장소를 사용할 수 없습니다: ${reason}`)
    throw new StoreUnavailableError(reason)
  }
  return { createOrder: unavailable, findOrder: unavailable, markConfirmAttempted: unavailable }
}
