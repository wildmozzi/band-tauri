import axios from 'axios'
import { PaymentRequest, PaymentResponse, PLAN_PRICES } from '../types/payment'

// 토스페이먼츠 API 설정
const TOSS_PAYMENTS_SECRET_KEY = process.env.TOSS_PAYMENTS_SECRET_KEY || 'test_sk_zXLkKEypNArWmo50nX3lmeaxYG5R'
const TOSS_PAYMENTS_BASE_URL = 'https://api.tosspayments.com/v1'

export class PaymentService {
  private getAuthHeader() {
    const credentials = Buffer.from(`${TOSS_PAYMENTS_SECRET_KEY}:`).toString('base64')
    return `Basic ${credentials}`
  }

  // 결제 승인 (토스페이먼츠)
  async confirmPayment(paymentKey: string, orderId: string, amount: number): Promise<PaymentResponse> {
    try {
      const response = await axios.post(
        `${TOSS_PAYMENTS_BASE_URL}/payments/confirm`,
        {
          paymentKey,
          orderId,
          amount
        },
        {
          headers: {
            'Authorization': this.getAuthHeader(),
            'Content-Type': 'application/json'
          }
        }
      )

      return {
        success: true,
        paymentKey: response.data.paymentKey,
        orderId: response.data.orderId
      }
    } catch (error: any) {
      console.error('Payment confirmation failed:', error.response?.data || error.message)
      return {
        success: false,
        error: error.response?.data?.message || '결제 승인에 실패했습니다.'
      }
    }
  }

  // 결제 취소
  async cancelPayment(paymentKey: string, cancelReason: string): Promise<PaymentResponse> {
    try {
      const response = await axios.post(
        `${TOSS_PAYMENTS_BASE_URL}/payments/${paymentKey}/cancel`,
        {
          cancelReason
        },
        {
          headers: {
            'Authorization': this.getAuthHeader(),
            'Content-Type': 'application/json'
          }
        }
      )

      return {
        success: true,
        paymentKey: response.data.paymentKey
      }
    } catch (error: any) {
      console.error('Payment cancellation failed:', error.response?.data || error.message)
      return {
        success: false,
        error: error.response?.data?.message || '결제 취소에 실패했습니다.'
      }
    }
  }

  // 결제 정보 조회
  async getPayment(paymentKey: string) {
    try {
      const response = await axios.get(
        `${TOSS_PAYMENTS_BASE_URL}/payments/${paymentKey}`,
        {
          headers: {
            'Authorization': this.getAuthHeader()
          }
        }
      )

      return response.data
    } catch (error: any) {
      console.error('Payment inquiry failed:', error.response?.data || error.message)
      throw new Error(error.response?.data?.message || '결제 정보 조회에 실패했습니다.')
    }
  }

  // 결제 요청 검증
  validatePaymentRequest(request: PaymentRequest): { valid: boolean; error?: string } {
    const { planType, amount } = request

    if (!['standard', 'pro'].includes(planType)) {
      return { valid: false, error: '유효하지 않은 플랜입니다.' }
    }

    const expectedAmount = PLAN_PRICES[planType]
    if (amount !== expectedAmount) {
      return { valid: false, error: '결제 금액이 올바르지 않습니다.' }
    }

    return { valid: true }
  }

  // 주문 ID 생성
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

export const paymentService = new PaymentService()