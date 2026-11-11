import { Router, type NextFunction, type Request, type Response } from 'express'
import { rateLimit } from 'express-rate-limit'
import { requireAuth } from '../lib/auth'
import { StoreUnavailableError } from '../repositories/errors'
import { ServiceError } from '../services/errors'
import type { PostService } from '../services/postService'
import { createPostBody, postFieldIssues, postIdParams, updatePostBody } from './schemas'

/**
 * 게시글 쓰기 API (P3-10d) — 작성자는 토큰의 userId, 이미지는 검증 완료 업로드만(PostService).
 * - POST /api/posts      글 작성
 * - PUT  /api/posts/:id  글 수정 (작성자만)
 * 조회·삭제·좋아요·댓글은 기존대로 클라이언트가 Supabase(RLS)로 한다.
 */

/**
 * 사용자별 글쓰기 상한 (P3-10g, PR #25 L4). requireAuth 뒤에서 토큰 userId로 센다 — IP를 바꿔도 같은 계정이면 막힌다.
 * 메모리 저장(express-rate-limit 기본 MemoryStore)이라 서버 재시작 시 초기화되고, 서버를 여러 대로 늘리면
 * 인스턴스마다 따로 센다(실효 상한 = 값 × 인스턴스 수). 스케일 아웃 시 Redis 등 공유 저장소로 바꿔야 한다.
 */
export interface PostUserLimits {
  /** 글 작성: 사용자당 분당 (기본 5) */
  createPerMinute: number
  /** 글 작성: 사용자당 24시간 (기본 50) */
  createPerDay: number
  /** 글 수정: 사용자당 분당 (기본 30) */
  editPerMinute: number
}

export const DEFAULT_POST_USER_LIMITS: PostUserLimits = { createPerMinute: 5, createPerDay: 50, editPerMinute: 30 }

function sendServiceError(res: Response, error: unknown): boolean {
  if (error instanceof ServiceError) {
    res.status(error.status).json(error.body)
    return true
  }
  if (error instanceof StoreUnavailableError) {
    res.status(503).json({ success: false, error: '게시글 저장소를 사용할 수 없습니다. 잠시 후 다시 시도해 주세요.' })
    return true
  }
  return false
}

export function createPostsRouter(
  service: PostService,
  opts: {
    rateLimits?: boolean
    /** 사용자별 상한. 생략하면 rateLimits가 false가 아닐 때 DEFAULT_POST_USER_LIMITS, 테스트는 값을 넣어 켠다 */
    userLimits?: Partial<PostUserLimits>
  } = {},
) {
  const router = Router()
  const limitMessage = { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' }
  if (opts.rateLimits !== false) {
    // 글쓰기·수정: IP당 분당 20회 (이미지 확인이 저장소 HEAD를 부른다)
    router.use(rateLimit({
      windowMs: 60_000,
      limit: 20,
      standardHeaders: 'draft-7',
      legacyHeaders: false,
      message: limitMessage,
    }))
  }
  router.use(requireAuth)

  const passthrough = (_req: Request, _res: Response, next: NextFunction) => next()
  const userLimitsOn = opts.rateLimits !== false || opts.userLimits !== undefined
  const limits: PostUserLimits = { ...DEFAULT_POST_USER_LIMITS, ...opts.userLimits }
  const perUser = (name: string, windowMs: number, limit: number) => userLimitsOn
    ? rateLimit({
      windowMs,
      limit,
      standardHeaders: 'draft-7',
      legacyHeaders: false,
      message: { success: false, error: '글을 너무 자주 쓰고 있습니다. 잠시 후 다시 시도해주세요.' },
      keyGenerator: (_req, res) => `${name}:${String(res.locals.userId)}`,
    })
    : passthrough
  const createLimits = [perUser('pc-m', 60_000, limits.createPerMinute), perUser('pc-d', 24 * 60 * 60_000, limits.createPerDay)]
  const editLimit = perUser('pe-m', 60_000, limits.editPerMinute)

  router.post('/', ...createLimits, async (req, res) => {
    const parsed = createPostBody.safeParse(req.body ?? {})
    if (!parsed.success) {
      res.status(400).json({ success: false, error: '입력값이 올바르지 않습니다.', issues: postFieldIssues(parsed.error) })
      return
    }
    try {
      const post = await service.create(res.locals.userId as string, parsed.data)
      res.status(201).json({ success: true, post })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('[POSTS] create failed:', error instanceof Error ? error.message : error)
      res.status(500).json({ success: false, error: '게시글 작성에 실패했습니다.' })
    }
  })

  router.put('/:id', editLimit, async (req, res) => {
    const params = postIdParams.safeParse(req.params)
    const body = updatePostBody.safeParse(req.body ?? {})
    if (!params.success || !body.success) {
      res.status(400).json({ success: false, error: '입력값이 올바르지 않습니다.', issues: body.success ? [] : postFieldIssues(body.error) })
      return
    }
    try {
      const post = await service.update(res.locals.userId as string, params.data.id, body.data)
      res.json({ success: true, post })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('[POSTS] update failed:', error instanceof Error ? error.message : error)
      res.status(500).json({ success: false, error: '게시글 수정에 실패했습니다.' })
    }
  })

  return router
}
