import { Router, type Response } from 'express'
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

export function createPostsRouter(service: PostService, opts: { rateLimits?: boolean } = {}) {
  const router = Router()
  if (opts.rateLimits !== false) {
    // 글쓰기·수정: IP당 분당 20회 (이미지 확인이 저장소 HEAD를 부른다)
    router.use(rateLimit({
      windowMs: 60_000,
      limit: 20,
      standardHeaders: 'draft-7',
      legacyHeaders: false,
      message: { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' },
    }))
  }
  router.use(requireAuth)

  router.post('/', async (req, res) => {
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

  router.put('/:id', async (req, res) => {
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
