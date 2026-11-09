import { rateLimit } from 'express-rate-limit'

const common = {
  standardHeaders: 'draft-7' as const,
  legacyHeaders: false,
  message: { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' },
}

/** /api 전체: IP당 분당 120회 */
export const apiLimiter = rateLimit({ ...common, windowMs: 60_000, limit: 120 })

/** 업로드/음원 분리: IP당 분당 10회 (디스크·CPU 소모 방지, M6) */
export const uploadLimiter = rateLimit({ ...common, windowMs: 60_000, limit: 10 })

/** 결제: IP당 분당 30회 */
export const paymentLimiter = rateLimit({ ...common, windowMs: 60_000, limit: 30 })
