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
}

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