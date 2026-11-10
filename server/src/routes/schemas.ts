import { z } from 'zod'

/**
 * 라우트 입력 스키마 (zod). 알 수 없는 키(body의 userId, amount 등)는 버린다 —
 * 신원은 토큰으로, 금액은 서버 가격표(주문)로만 정한다. confirm의 amount는 대조용일 뿐 결제 금액으로 쓰지 않는다.
 */

const shortString = z.string().min(1).max(200)

/** 서버 발급 orderId 형식 (토스 규칙: 영문·숫자·-·_ 6~64자) */
const orderIdString = z.string().regex(/^[A-Za-z0-9_-]{6,64}$/)

/** confirm: orderId는 POST /orders로 받은 것, amount는 토스 successUrl의 금액 (주문 금액과 대조) */
export const confirmPaymentBody = z.object({
  paymentKey: shortString,
  orderId: orderIdString,
  planType: z.enum(['standard', 'pro']),
  amount: z.number().int().positive(),
})

export const createOrderBody = z.object({
  planType: z.enum(['standard', 'pro']),
})

export const orderIdParams = z.object({
  orderId: orderIdString,
})

export const cancelPaymentBody = z.object({
  paymentKey: z.string().max(200).optional(),
  // 잘못된 타입이면 기본 사유로 (기존 동작)
  cancelReason: z.string().optional().catch(undefined),
})

export const changePlanBody = z.object({
  newPlan: z.enum(['free', 'standard', 'pro']),
})

/** 업로드 파일명 (경로 검증은 resolveUploadPath가 추가로 한다) */
export const uploadFilenameParams = z.object({
  filename: z.string().min(1).max(255),
})

export const separationIdParams = z.object({
  id: z.string().uuid(),
})
