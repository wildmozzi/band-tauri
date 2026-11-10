// 결제 API는 모두 Bearer 토큰 필요. userId는 서버가 토큰으로 결정한다(본문의 userId는 무시됨)
import { authHeaders } from '../lib/authHeaders'
import type { OrderStatus, OrderStatusResult } from './paymentFlow'

// 자동 IP 감지: 'auto'면 현재 접속한 호스트 사용
function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') {
    const protocol = window.location.protocol
    const host = window.location.hostname
    return `${protocol}//${host}:8080`
  }
  return envUrl
}
const API_BASE_URL = getApiUrl()

/** orderId는 createOrder로 서버에서 받은 것. amount는 토스 successUrl의 금액(서버가 주문 금액과 대조) */
export interface PaymentConfirmRequest {
  paymentKey: string
  orderId: string
  amount: number
  planType: 'standard' | 'pro'
}

/** 서버 발급 결제 주문 */
export interface PaymentOrder {
  orderId: string
  planType: 'standard' | 'pro'
  amount: number
  expiresAt: string
}

export interface CreateOrderResponse {
  success: boolean
  order?: PaymentOrder
  error?: string
}

export interface PaymentResponse {
  success: boolean
  /** HTTP 상태 (응답을 못 받았으면 0). 503이면 결과 미확정 → 주문 상태 조회 */
  httpStatus?: number
  subscription?: {
    id: string
    planType: string
    status: string
    startDate: string
    endDate: string
  }
  error?: string
}

export interface UserSubscription {
  success: boolean
  user?: {
    id: string
    planType: 'free' | 'standard' | 'pro'
    subscription?: {
      id: string
      status: string
      startDate: string
      endDate: string
      amount: number
    }
  }
  error?: string
}

/** 응답 본문 JSON (본문이 JSON이 아니면 빈 객체 — 프록시 오류 페이지 등) */
async function jsonOrEmpty(response: Response): Promise<Record<string, unknown>> {
  try {
    const data = await response.json()
    return data && typeof data === 'object' ? data as Record<string, unknown> : {}
  } catch {
    return {}
  }
}

class PaymentApi {
  // 결제 주문 발급: orderId·금액은 서버가 정한다 (토스 결제창 호출 전에 먼저)
  async createOrder(planType: 'standard' | 'pro'): Promise<CreateOrderResponse> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/orders`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(await authHeaders()),
        },
        body: JSON.stringify({ planType }),
      })
      const data = await jsonOrEmpty(response)
      if (!response.ok || !data.order) {
        return { success: false, error: typeof data.error === 'string' ? data.error : '주문 생성에 실패했습니다.' }
      }
      return { success: true, order: data.order as PaymentOrder }
    } catch (error) {
      console.error('Order creation failed:', error)
      return { success: false, error: '주문 생성 요청에 실패했습니다. 네트워크를 확인해 주세요.' }
    }
  }

  // 주문 상태 조회 ('결제 확인 중' 화면 폴링)
  async getOrderStatus(orderId: string): Promise<OrderStatusResult> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/orders/${encodeURIComponent(orderId)}`, {
        headers: await authHeaders(),
      })
      const data = await jsonOrEmpty(response)
      const order = data.order as { status?: OrderStatus } | undefined
      return {
        httpStatus: response.status,
        status: response.ok ? order?.status : undefined,
        error: typeof data.error === 'string' ? data.error : undefined,
      }
    } catch {
      return { httpStatus: 0 }
    }
  }

  // 결제 승인. httpStatus로 결과 미확정(503 등)을 구분한다
  async confirmPayment(request: PaymentConfirmRequest): Promise<PaymentResponse> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/confirm`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(await authHeaders()),
        },
        body: JSON.stringify(request),
      })

      const data = await jsonOrEmpty(response)
      return {
        ...(data as Omit<PaymentResponse, 'httpStatus'>),
        success: response.ok && data.success === true,
        httpStatus: response.status,
      }
    } catch (error) {
      // 응답 유실: 서버에서는 승인됐을 수 있다 → 호출자가 주문 상태로 확인
      console.error('Payment confirmation failed:', error)
      return {
        success: false,
        httpStatus: 0,
        error: '결제 승인 요청에 실패했습니다.'
      }
    }
  }

  // 결제 취소
  async cancelPayment(paymentKey: string, userId: string, cancelReason?: string): Promise<PaymentResponse> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/cancel`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(await authHeaders()),
        },
        body: JSON.stringify({
          paymentKey,
          userId,
          cancelReason
        }),
      })

      const data = await response.json()
      return data
    } catch (error) {
      console.error('Payment cancellation failed:', error)
      return {
        success: false,
        error: '결제 취소 요청에 실패했습니다.'
      }
    }
  }

  // 사용자 구독 정보 조회
  async getUserSubscription(userId: string): Promise<UserSubscription> {
    try {
      // 본인 구독만 조회 가능 (userId는 하위 호환용, 서버는 토큰의 사용자로 판정)
      void userId
      const response = await fetch(`${API_BASE_URL}/api/payment/subscription`, { headers: await authHeaders() })
      const data = await response.json()
      return data
    } catch {
      // 서버 미실행 시 조용히 실패 (localStorage fallback 사용)
      return {
        success: false,
        error: '구독 정보 조회에 실패했습니다.'
      }
    }
  }

  // 플랜 변경
  async changePlan(userId: string, newPlan: 'free' | 'standard' | 'pro'): Promise<PaymentResponse> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/change-plan`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(await authHeaders()),
        },
        body: JSON.stringify({
          userId,
          newPlan
        }),
      })

      const data = await response.json()
      return data
    } catch (error) {
      console.error('Plan change failed:', error)
      return {
        success: false,
        error: '플랜 변경 요청에 실패했습니다.'
      }
    }
  }

  // 플랜 정보 조회
  async getPlans() {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/plans`)
      const data = await response.json()
      return data
    } catch (error) {
      console.error('Plans inquiry failed:', error)
      return {
        success: false,
        error: '플랜 정보 조회에 실패했습니다.'
      }
    }
  }
}

export const paymentApi = new PaymentApi()