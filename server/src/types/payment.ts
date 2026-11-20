export type UserPlan = 'free' | 'standard' | 'pro'

export interface PaymentRequest {
  userId: string
  planType: UserPlan
  paymentMethod: 'card' | 'bank' | 'kakao' | 'naver'
  amount: number
}

export interface PaymentResponse {
  success: boolean
  paymentKey?: string
  orderId?: string
  error?: string
  /** 승인 결과 분류 (confirm). 없으면 success로 판단 */
  outcome?: 'approved' | 'rejected' | 'unknown'
  /** 토스 에러 코드 */
  code?: string
}

/** 토스 결제 조회 결과 (필요한 필드만) */
export interface TossPaymentStatus {
  paymentKey: string
  orderId: string
  /** READY | IN_PROGRESS | WAITING_FOR_DEPOSIT | DONE | CANCELED | PARTIAL_CANCELED | ABORTED | EXPIRED */
  status: string
  totalAmount: number
}

export type TossPaymentLookup = { found: true; payment: TossPaymentStatus } | { found: false }

export interface Subscription {
  id: string
  userId: string
  planType: UserPlan
  status: 'active' | 'cancelled' | 'expired'
  startDate: Date
  endDate: Date
  paymentKey?: string
  amount: number
  createdAt: Date
  updatedAt: Date
}

export interface User {
  id: string
  email: string
  nickname: string
  planType: UserPlan
  subscriptionId?: string
  createdAt: Date
  updatedAt: Date
}

export const PLAN_PRICES: Record<UserPlan, number> = {
  free: 0,
  standard: 2900,
  pro: 6900
}