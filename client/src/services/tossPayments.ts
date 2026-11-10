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
  // orderId는 클라이언트가 만들지 않는다: paymentApi.createOrder로 서버에서 발급받는다 (P3-5b)
}

export const tossPaymentsService = new TossPaymentsService()