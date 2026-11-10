import { loadTossPayments, type TossPaymentsInstance } from '@tosspayments/payment-sdk'

const CLIENT_KEY: string = import.meta.env.VITE_TOSS_PAYMENTS_CLIENT_KEY || ''

export interface PaymentRequest {
  amount: number
  orderId: string
  orderName: string
  customerName?: string
  customerEmail?: string
  successUrl: string
  failUrl: string
}

class TossPaymentsService {
  private tossPayments: TossPaymentsInstance | null = null

  async initialize() {
    if (!this.tossPayments) {
      if (!CLIENT_KEY) throw new Error('VITE_TOSS_PAYMENTS_CLIENT_KEY가 설정되지 않았습니다')
      this.tossPayments = await loadTossPayments(CLIENT_KEY)
    }
    return this.tossPayments
  }

  async requestPayment(request: PaymentRequest) {
    const tossPayments = await this.initialize()
    
    // 결제 방법을 지정하지 않으면 토스페이먼츠에서 결제 방법 선택 화면을 제공
    return tossPayments.requestPayment({
      amount: request.amount,
      orderId: request.orderId,
      orderName: request.orderName,
      customerName: request.customerName,
      customerEmail: request.customerEmail,
      successUrl: request.successUrl,
      failUrl: request.failUrl,
    })
  }

  generateOrderId(userId: string, planType: string): string {
    // userId를 안전한 형태로 변환 (영문, 숫자만)
    const safeUserId = userId.replace(/[^a-zA-Z0-9]/g, '').substring(0, 8) || 'user'
    const timestamp = Date.now().toString().slice(-8) // 마지막 8자리만
    const random = Math.random().toString(36).substring(2, 6) // 4자리 랜덤
    
    // orderId 생성 (영문, 숫자, -, _ 만 사용, 6-64자)
    const orderId = `order-${safeUserId}-${planType}-${timestamp}-${random}`
    
    // 길이 검증 (6-64자)
    if (orderId.length < 6 || orderId.length > 64) {
      // 길이가 맞지 않으면 더 간단한 형태로 생성
      return `order-${timestamp}-${random}`
    }
    
    return orderId
  }
}

export const tossPaymentsService = new TossPaymentsService()