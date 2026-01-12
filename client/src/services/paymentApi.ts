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

export interface PaymentConfirmRequest {
  paymentKey: string
  orderId: string
  amount: number
  userId: string
  planType: 'standard' | 'pro'
}

export interface PaymentResponse {
  success: boolean
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

class PaymentApi {
  // 결제 승인
  async confirmPayment(request: PaymentConfirmRequest): Promise<PaymentResponse> {
    try {
      const response = await fetch(`${API_BASE_URL}/api/payment/confirm`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(request),
      })

      const data = await response.json()
      return data
    } catch (error) {
      console.error('Payment confirmation failed:', error)
      return {
        success: false,
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
      const response = await fetch(`${API_BASE_URL}/api/payment/subscription/${userId}`)
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