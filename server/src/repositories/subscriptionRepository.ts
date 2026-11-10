import type { SupabaseClient } from '@supabase/supabase-js'
import type { UserPlan } from '../types/payment'
import { StoreUnavailableError, toRepositoryError } from './errors'

/**
 * subscriptions / payments 테이블 접근 (docs/migrations/007_subscriptions.sql).
 * 쓰기는 service_role로만 가능하다(RLS). 결제+구독 기록과 취소는 DB 함수로 한 트랜잭션에서 처리한다.
 */

export type SubscriptionStatus = 'active' | 'cancelled' | 'expired'
export type PaidPlan = Exclude<UserPlan, 'free'>

export interface SubscriptionRecord {
  id: string
  userId: string
  planType: PaidPlan
  status: SubscriptionStatus
  startDate: Date
  endDate: Date
  paymentKey: string | null
  amount: number
  createdAt: Date
  updatedAt: Date
}

export interface PaymentRecord {
  id: string
  userId: string
  orderId: string
  paymentKey: string
  planType: PaidPlan
  amount: number
  status: PaymentStatus
  subscriptionId: string | null
  createdAt: Date
  updatedAt: Date
}

/** manual_review: 재조정이 오래(24h) 판정하지 못해 수동 확인으로 뺀 결제 */
export type PaymentStatus = 'pending' | 'manual_review' | 'confirmed' | 'cancelled'

/** 결제 의도 (토스 승인 요청 전 pending 행) */
export interface PaymentIntent {
  userId: string
  orderId: string
  paymentKey: string
  planType: PaidPlan
  amount: number
}

export interface RecordPaymentInput extends PaymentIntent {
  periodDays: number
}

export interface SubscriptionRepository {
  /** paymentKey 또는 orderId가 같은 결제 기록 (멱등성 확인용) */
  findPaymentsByKeyOrOrder(paymentKey: string, orderId: string): Promise<PaymentRecord[]>
  /** orderId의 결제 기록 (주문 상태 조회용, order_id 유니크) */
  findPaymentByOrderId(orderId: string): Promise<PaymentRecord | null>
  /** 토스 승인 요청 전 pending 행 기록. paymentKey/orderId가 이미 있으면 ConflictError */
  createPendingPayment(intent: PaymentIntent): Promise<void>
  /** pending 행 삭제 (승인 거절·미승인 확정·보상 취소 후). pending이 아니면 아무것도 안 함 */
  deletePendingPayment(orderId: string): Promise<void>
  /** createdBefore보다 먼저 생긴 pending 행 (재조정용, 마지막 확인이 오래된 순) */
  listPendingPayments(createdBefore: Date, limit: number): Promise<PaymentRecord[]>
  /** 판정 보류: updated_at을 갱신해 다음 회차 순서를 뒤로 미룬다 */
  touchPendingPayment(orderId: string, now: Date): Promise<void>
  /** pending → manual_review (오래 미해결) */
  markPaymentForManualReview(orderId: string, now: Date): Promise<void>
  /**
   * 승인된 결제와 구독을 한 트랜잭션으로 기록한다 (pending 행이 있으면 confirmed로 전환).
   * 같은 사용자의 같은 결제가 이미 있으면 기존 구독을 돌려준다(멱등). 다른 사용자의 결제면 ConflictError.
   */
  recordConfirmedPayment(input: RecordPaymentInput): Promise<SubscriptionRecord>
  findSubscriptionById(id: string): Promise<SubscriptionRecord | null>
  /** 현재 유효한(active, end_date > now) 가장 최근 구독 */
  findActiveSubscription(userId: string, now: Date): Promise<SubscriptionRecord | null>
  /**
   * 구독을 cancelled로 바꾼다. paymentCancelled면 연결된 결제도 cancelled로(같은 트랜잭션).
   * active가 아니었으면 false.
   */
  cancelSubscription(subscriptionId: string, opts: { paymentCancelled: boolean; reason: string | null }): Promise<boolean>
  /** 기간이 지난 active 구독을 expired로. 바뀐 개수 */
  expireDueSubscriptions(now: Date): Promise<number>
}

interface SubscriptionRow {
  id: string
  user_id: string
  plan_type: PaidPlan
  status: SubscriptionStatus
  start_date: string
  end_date: string
  payment_key: string | null
  amount: number
  created_at: string
  updated_at: string
}

interface PaymentRow {
  id: string
  user_id: string
  order_id: string
  payment_key: string
  plan_type: PaidPlan
  amount: number
  status: PaymentStatus
  subscription_id: string | null
  created_at: string
  updated_at: string
}

export function subscriptionFromRow(r: SubscriptionRow): SubscriptionRecord {
  return {
    id: r.id,
    userId: r.user_id,
    planType: r.plan_type,
    status: r.status,
    startDate: new Date(r.start_date),
    endDate: new Date(r.end_date),
    paymentKey: r.payment_key,
    amount: r.amount,
    createdAt: new Date(r.created_at),
    updatedAt: new Date(r.updated_at),
  }
}

function paymentFromRow(r: PaymentRow): PaymentRecord {
  return {
    id: r.id,
    userId: r.user_id,
    orderId: r.order_id,
    paymentKey: r.payment_key,
    planType: r.plan_type,
    amount: r.amount,
    status: r.status,
    subscriptionId: r.subscription_id,
    createdAt: new Date(r.created_at),
    updatedAt: new Date(r.updated_at),
  }
}

const MIGRATION_HINT = 'subscriptions/payments 테이블 또는 함수가 없습니다 — docs/migrations/007_subscriptions.sql 실행 필요'

/** 테이블 미존재는 운영자가 바로 알아볼 수 있게 로그를 남긴다 (조용한 폴백 금지) */
function fail(err: { message: string; code?: string }, context: string): never {
  const e = toRepositoryError(err, context)
  if (e instanceof StoreUnavailableError) {
    console.error(`[DB][PAYMENT] ${MIGRATION_HINT} (${e.message})`)
  }
  throw e
}

export function createSupabaseSubscriptionRepository(supabase: SupabaseClient): SubscriptionRepository {
  return {
    async findPaymentsByKeyOrOrder(paymentKey, orderId) {
      // .or() 필터 문자열에 사용자 입력을 넣지 않도록 두 번 조회한다
      const [byKey, byOrder] = await Promise.all([
        supabase.from('payments').select('*').eq('payment_key', paymentKey),
        supabase.from('payments').select('*').eq('order_id', orderId),
      ])
      if (byKey.error) fail(byKey.error, 'payments.findByKey')
      if (byOrder.error) fail(byOrder.error, 'payments.findByOrder')
      const seen = new Map<string, PaymentRecord>()
      for (const r of [...(byKey.data ?? []), ...(byOrder.data ?? [])] as PaymentRow[]) {
        seen.set(r.id, paymentFromRow(r))
      }
      return [...seen.values()]
    },

    async findPaymentByOrderId(orderId) {
      const { data, error } = await supabase.from('payments').select('*').eq('order_id', orderId).maybeSingle()
      if (error) fail(error, 'payments.findByOrderId')
      return data ? paymentFromRow(data as PaymentRow) : null
    },

    async createPendingPayment(intent) {
      const { error } = await supabase.from('payments').insert({
        user_id: intent.userId,
        order_id: intent.orderId,
        payment_key: intent.paymentKey,
        plan_type: intent.planType,
        amount: intent.amount,
        status: 'pending',
      })
      if (error) fail(error, 'payments.createPending')
    },

    async deletePendingPayment(orderId) {
      const { error } = await supabase.from('payments').delete().eq('order_id', orderId).eq('status', 'pending')
      if (error) fail(error, 'payments.deletePending')
    },

    async listPendingPayments(createdBefore, limit) {
      const { data, error } = await supabase
        .from('payments')
        .select('*')
        .eq('status', 'pending')
        .lt('created_at', createdBefore.toISOString())
        .order('updated_at', { ascending: true })
        .limit(limit)
      if (error) fail(error, 'payments.listPending')
      return ((data ?? []) as PaymentRow[]).map(paymentFromRow)
    },

    async touchPendingPayment(orderId, now) {
      const { error } = await supabase.from('payments').update({ updated_at: now.toISOString() })
        .eq('order_id', orderId).eq('status', 'pending')
      if (error) fail(error, 'payments.touchPending')
    },

    async markPaymentForManualReview(orderId, now) {
      const { error } = await supabase.from('payments').update({ status: 'manual_review', updated_at: now.toISOString() })
        .eq('order_id', orderId).eq('status', 'pending')
      if (error) fail(error, 'payments.markManualReview')
    },

    async recordConfirmedPayment(input) {
      const { data, error } = await supabase.rpc('record_confirmed_payment', {
        p_user_id: input.userId,
        p_order_id: input.orderId,
        p_payment_key: input.paymentKey,
        p_plan_type: input.planType,
        p_amount: input.amount,
        p_period_days: input.periodDays,
      })
      if (error) fail(error, 'record_confirmed_payment')
      const row = (Array.isArray(data) ? data[0] : data) as SubscriptionRow | null
      if (!row) throw new Error('record_confirmed_payment returned no row')
      return subscriptionFromRow(row)
    },

    async findSubscriptionById(id) {
      const { data, error } = await supabase.from('subscriptions').select('*').eq('id', id).maybeSingle()
      if (error) fail(error, 'subscriptions.findById')
      return data ? subscriptionFromRow(data as SubscriptionRow) : null
    },

    async findActiveSubscription(userId, now) {
      const { data, error } = await supabase
        .from('subscriptions')
        .select('*')
        .eq('user_id', userId)
        .eq('status', 'active')
        .gt('end_date', now.toISOString())
        .order('created_at', { ascending: false })
        .limit(1)
        .maybeSingle()
      if (error) fail(error, 'subscriptions.findActive')
      return data ? subscriptionFromRow(data as SubscriptionRow) : null
    },

    async cancelSubscription(subscriptionId, opts) {
      const { data, error } = await supabase.rpc('cancel_subscription', {
        p_subscription_id: subscriptionId,
        p_cancel_payment: opts.paymentCancelled,
        p_reason: opts.reason,
      })
      if (error) fail(error, 'cancel_subscription')
      return data === true
    },

    async expireDueSubscriptions(now) {
      const { data, error } = await supabase
        .from('subscriptions')
        .update({ status: 'expired', updated_at: now.toISOString() })
        .eq('status', 'active')
        .lte('end_date', now.toISOString())
        .select('id')
      if (error) fail(error, 'subscriptions.expireDue')
      return (data ?? []).length
    },
  }
}

/** 백엔드(Supabase) 미설정: 모든 호출이 StoreUnavailableError (메모리 폴백 금지) */
export function createUnavailableSubscriptionRepository(reason = 'Supabase가 설정되지 않았습니다'): SubscriptionRepository {
  const unavailable = async (): Promise<never> => {
    console.error(`[DB][PAYMENT] 결제 저장소를 사용할 수 없습니다: ${reason}`)
    throw new StoreUnavailableError(reason)
  }
  return {
    findPaymentsByKeyOrOrder: unavailable,
    findPaymentByOrderId: unavailable,
    createPendingPayment: unavailable,
    deletePendingPayment: unavailable,
    listPendingPayments: unavailable,
    touchPendingPayment: unavailable,
    markPaymentForManualReview: unavailable,
    recordConfirmedPayment: unavailable,
    findSubscriptionById: unavailable,
    findActiveSubscription: unavailable,
    cancelSubscription: unavailable,
    expireDueSubscriptions: unavailable,
  }
}
