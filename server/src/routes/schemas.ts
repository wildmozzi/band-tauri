import { z } from 'zod'

/**
 * 라우트 입력 스키마 (zod). 알 수 없는 키(body의 userId, amount 등)는 버린다 —
 * 신원은 토큰으로, 금액은 서버 가격표로만 정한다.
 */

const shortString = z.string().min(1).max(200)

export const confirmPaymentBody = z.object({
  paymentKey: shortString,
  orderId: shortString,
  planType: z.enum(['standard', 'pro']),
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
