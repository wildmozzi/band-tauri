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

/** 방 생성 (P2-6). host_id는 받지 않는다 — 토큰의 userId가 방장 */
export const createRoomBody = z.object({
  title: z.string().trim().min(1).max(100),
  description: z.string().trim().max(500).nullish().transform(v => v || null),
  genre: z.string().trim().max(30).nullish().transform(v => v || null),
  maxParticipants: z.number().int().min(2).max(20).default(8),
  tags: z.array(z.string().trim().min(1).max(20)).max(20).default([]),
  freeJoin: z.boolean().default(true),
  isPrivate: z.boolean().default(false),
  instrumentSlots: z.array(z.object({
    instrument: z.string().min(1).max(30),
    count: z.number().int().min(0).max(10),
  })).max(20).default([]).transform(slots => slots.filter(s => s.count > 0)),
  /** 평문은 해시만 저장하고 버린다. 길이 검증은 service(validateRoomPassword) */
  password: z.string().max(256).nullish(),
})

export const roomIdParams = z.object({
  id: z.string().uuid(),
})

export const setRoomPasswordBody = z.object({
  /** null 또는 빈 문자열 → 비밀번호 해제 */
  password: z.string().max(256).nullable(),
})
