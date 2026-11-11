import { z } from 'zod'
import { ROOM_CAPACITY_MAX, ROOM_CAPACITY_MIN } from '../lib/roomCapacity'

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

/** 방 생성 (P2-6). host_id는 받지 않는다 — 토큰의 userId가 방장 */
export const createRoomBody = z.object({
  title: z.string().trim().min(1).max(100),
  description: z.string().trim().max(500).nullish().transform(v => v || null),
  genre: z.string().trim().max(30).nullish().transform(v => v || null),
  maxParticipants: z.number().int().min(ROOM_CAPACITY_MIN).max(ROOM_CAPACITY_MAX).default(8),
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

/** 게시글 이미지 서명 업로드 URL 요청 (P3-10). 크기·MIME은 서버 상한으로 검증, key는 서버가 정한다 */
export const postImageUploadBody = z.object({
  contentType: z.enum(['image/jpeg', 'image/png', 'image/webp', 'image/gif']),
  size: z.number().int().positive(),
})

/** 게시글 이미지 업로드 완료 확인 (P3-10 리뷰 M2) */
export const postImageCompleteBody = z.object({
  key: z.string().min(1).max(200),
})

/** 게시글 저장 전 이미지 URL 정규화 (리뷰2 M1·M3). 게시글 이미지는 최대 5장이지만 여유를 둔다 */
export const postImageResolveBody = z.object({
  urls: z.array(z.string().min(1).max(2048)).max(20),
})

/** 게시글 (P3-10d). 서버가 작성자·이미지 참조를 정한다. 이미지 최대 5장(클라이언트 MAX_IMAGES) */
export const POST_INSTRUMENTS = ['vocal', 'guitar', 'bass', 'keyboard', 'drums', 'other'] as const
export const POST_CATEGORIES = ['session', 'collab', 'beta', 'feedback', 'general'] as const
export const POST_LIMITS = { title: 200, content: 20_000, tags: 20, tagLength: 50, images: 5 } as const
const postTitle = z.string().trim().min(1).max(POST_LIMITS.title)
const postContent = z.string().trim().min(1).max(POST_LIMITS.content)
const postTags = z.array(z.string().trim().min(1).max(POST_LIMITS.tagLength)).max(POST_LIMITS.tags)
const postImages = z.array(z.string().min(1).max(2048)).max(POST_LIMITS.images)

export const createPostBody = z.object({
  title: postTitle,
  content: postContent,
  tags: postTags.default([]),
  images: postImages.default([]),
  instrument: z.enum(POST_INSTRUMENTS).default('other'),
  category: z.enum(POST_CATEGORIES).default('general'),
})

/**
 * 수정 본문은 모양(타입)과 크기 상한만 본다 (PR #25 M2). 새 입력 규칙은 postService가 기존 글과 비교해 적용한다:
 * 예전 클라이언트가 직접 쓴 글(제목 200자 초과, 태그 20개 초과, 이미지 5장 초과, 목록 밖 악기 등)도
 * 바뀌지 않은 값은 그대로 두고 수정할 수 있게.
 */
export const updatePostBody = z.object({
  title: z.string().max(10_000).optional(),
  content: z.string().max(200_000).optional(),
  tags: z.array(z.string().max(1000)).max(200).optional(),
  images: z.array(z.string().min(1).max(4096)).max(100).optional(),
  instrument: z.string().max(100).optional(),
  category: z.string().max(100).optional(),
})

export interface FieldIssue {
  field: string
  message: string
}

const POST_FIELD_LABELS: Record<string, string> = {
  title: '제목', content: '내용', tags: '태그', images: '이미지', instrument: '악기', category: '카테고리',
}

/** 게시글 필드 이름 → 사용자 표시 이름 */
export function postFieldLabel(field: string): string {
  return POST_FIELD_LABELS[field] ?? field
}

/** zod 오류 → 필드별 이유 (게시글 400 응답의 issues, PR #25 M2) */
export function postFieldIssues(error: z.ZodError): FieldIssue[] {
  const out: FieldIssue[] = []
  const seen = new Set<string>()
  for (const issue of error.issues) {
    const field = typeof issue.path[0] === 'string' ? issue.path[0] : 'body'
    const label = postFieldLabel(field)
    const inArray = issue.path.length > 1
    let message: string
    if (issue.code === 'too_small') message = inArray ? `${label}에 빈 값이 있습니다.` : `${label}을(를) 입력하세요.`
    else if (issue.code === 'too_big') {
      const max = Number(issue.maximum)
      message = issue.type === 'array' ? `${label}은(는) 최대 ${max}개까지입니다.` : inArray ? `${label}은(는) 하나에 최대 ${max}자까지입니다.` : `${label}은(는) 최대 ${max}자까지입니다.`
    } else if (issue.code === 'invalid_enum_value') message = `${label}: 선택할 수 없는 값입니다.`
    else if (issue.code === 'invalid_type' && issue.received === 'undefined') message = `${label}을(를) 입력하세요.`
    else message = `${label} 형식이 올바르지 않습니다.`
    const k = `${field}|${message}`
    if (seen.has(k)) continue
    seen.add(k)
    out.push({ field, message })
  }
  return out
}

export const postIdParams = z.object({
  id: z.string().uuid(),
})
