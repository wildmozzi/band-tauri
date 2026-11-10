import axios from 'axios'
import { PaymentRequest, PaymentResponse, PLAN_PRICES, TossPaymentLookup } from '../types/payment'

// 토스페이먼츠 API 설정
// 시크릿 키는 환경변수로만 받는다 (하드코딩 금지)
const TOSS_PAYMENTS_SECRET_KEY = process.env.TOSS_PAYMENTS_SECRET_KEY || ''
if (!TOSS_PAYMENTS_SECRET_KEY) {
  console.warn('[PAYMENT] TOSS_PAYMENTS_SECRET_KEY가 설정되지 않았습니다 - 결제 승인/취소가 실패합니다')
}
const TOSS_PAYMENTS_BASE_URL = 'https://api.tosspayments.com/v1'
/** 토스 API 타임아웃. 넘으면 결과 불명(unknown)으로 보고 조회로 확인한다 */
export const TOSS_TIMEOUT_MS = 15_000

/**
 * 거절이 아니라 "결과를 알 수 없음"으로 봐야 하는 토스 에러 코드.
 * ALREADY_PROCESSED_PAYMENT: 이전 승인 요청이 실제로는 성공했을 수 있다(응답 유실 후 재시도).
 * IDEMPOTENT_REQUEST_PROCESSING(409): 같은 멱등키의 승인이 아직 처리 중 (동시 confirm) — 거절이 아니다.
 */
const UNCERTAIN_CODES = new Set([
  'ALREADY_PROCESSED_PAYMENT',
  'IDEMPOTENT_REQUEST_PROCESSING',
  'PROVIDER_ERROR',
  'FAILED_INTERNAL_SYSTEM_PROCESSING',
  'FAILED_PAYMENT_INTERNAL_SYSTEM_PROCESSING',
  'UNKNOWN_PAYMENT_ERROR',
])

/** axios 에러 → 승인 결과 분류. 응답 없음(타임아웃·연결 끊김)·5xx·불확실 코드는 unknown */
export function classifyTossError(error: any): PaymentResponse {
  const status: number | undefined = error?.response?.status
  const code: string | undefined = error?.response?.data?.code
  const message: string = error?.response?.data?.message || error?.message || '결제 승인에 실패했습니다.'
  const uncertain = !error?.response || (status !== undefined && status >= 500) || (code !== undefined && UNCERTAIN_CODES.has(code))
  return { success: false, outcome: uncertain ? 'unknown' : 'rejected', code, error: message }
}

const pathKey = (paymentKey: string) => encodeURIComponent(paymentKey)

export class PaymentService {
  private getAuthHeader() {
    if (!TOSS_PAYMENTS_SECRET_KEY) throw new Error('TOSS_PAYMENTS_SECRET_KEY is not configured')
    const credentials = Buffer.from(`${TOSS_PAYMENTS_SECRET_KEY}:`).toString('base64')
    return `Basic ${credentials}`
  }

  /**
   * 결제 승인 (토스페이먼츠). Idempotency-Key=orderId: 같은 주문 재요청은 토스가 첫 응답을 돌려준다.
   * outcome: approved | rejected(확정 거절) | unknown(결과 불명 → 조회로 확인해야 함)
   */
  async confirmPayment(paymentKey: string, orderId: string, amount: number): Promise<PaymentResponse> {
    try {
      const response = await axios.post(
        `${TOSS_PAYMENTS_BASE_URL}/payments/confirm`,
        { paymentKey, orderId, amount },
        {
          timeout: TOSS_TIMEOUT_MS,
          headers: {
            'Authorization': this.getAuthHeader(),
            'Content-Type': 'application/json',
            'Idempotency-Key': `confirm-${orderId}`,
          }
        }
      )

      return {
        success: true,
        outcome: 'approved',
        paymentKey: response.data.paymentKey,
        orderId: response.data.orderId
      }
    } catch (error: any) {
      const r = classifyTossError(error)
      console.error(`Payment confirmation failed (${r.outcome}):`, error.response?.data || error.message)
      return r
    }
  }

  /**
   * 결제 전액 취소. 이미 취소된 결제(ALREADY_CANCELED_PAYMENT)는 성공으로 본다 (응답 유실 후 재시도).
   * 멱등키는 쓰지 않는다: 토스가 실패 응답까지 캐시하면 일시 오류 뒤 재시도가 막히고,
   * 전액 취소는 두 번 보내도 두 번째가 ALREADY_CANCELED라 이중 환불이 없다.
   */
  async cancelPayment(paymentKey: string, cancelReason: string): Promise<PaymentResponse> {
    try {
      const response = await axios.post(
        `${TOSS_PAYMENTS_BASE_URL}/payments/${pathKey(paymentKey)}/cancel`,
        { cancelReason },
        {
          timeout: TOSS_TIMEOUT_MS,
          headers: {
            'Authorization': this.getAuthHeader(),
            'Content-Type': 'application/json',
          }
        }
      )

      return {
        success: true,
        paymentKey: response.data.paymentKey
      }
    } catch (error: any) {
      if (error.response?.data?.code === 'ALREADY_CANCELED_PAYMENT') {
        console.warn(`[PAYMENT] 이미 취소된 결제 — 성공으로 처리 (paymentKey=${paymentKey})`)
        return { success: true, paymentKey }
      }
      console.error('Payment cancellation failed:', error.response?.data || error.message)
      return {
        success: false,
        error: error.response?.data?.message || '결제 취소에 실패했습니다.'
      }
    }
  }

  /**
   * 결제 상태 조회. 결제가 없으면 { found: false }. 그 밖의 실패(타임아웃 등)는 throw — 호출자는 "모름"으로 다룬다.
   */
  async lookupPayment(paymentKey: string): Promise<TossPaymentLookup> {
    try {
      const response = await axios.get(`${TOSS_PAYMENTS_BASE_URL}/payments/${pathKey(paymentKey)}`, {
        timeout: TOSS_TIMEOUT_MS,
        headers: { 'Authorization': this.getAuthHeader() },
      })
      const d = response.data
      return { found: true, payment: { paymentKey: d.paymentKey, orderId: d.orderId, status: d.status, totalAmount: d.totalAmount } }
    } catch (error: any) {
      if (error.response?.status === 404 || error.response?.data?.code === 'NOT_FOUND_PAYMENT') return { found: false }
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
