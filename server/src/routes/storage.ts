import { Router, type NextFunction, type Request, type Response } from 'express'
import { rateLimit } from 'express-rate-limit'
import { requireAuth } from '../lib/auth'
import {
  POST_IMAGE_MAX_BYTES,
  POST_IMAGE_UPLOAD_TTL_SEC,
  POST_IMAGE_VIEW_TTL_SEC,
  isPostImageOwner,
  newPostImageKey,
  postImageKeyFromParams,
  postImageViewUrl,
} from '../lib/imageStorage'
import type { StorageAdapter } from '../storage/types'
import { UploadQuota, verifyPostImageUpload } from '../services/postImageService'
import { postImageCompleteBody, postImageUploadBody } from './schemas'

/**
 * 파일 저장소 API (P3-10)
 * - POST /api/storage/post-images/upload-url  (인증) 게시글 이미지 서명 PUT URL 발급 (사용자별 24h 상한)
 * - POST /api/storage/post-images/complete    (인증) 업로드 확인: 크기·MIME·내용 검증, 실패 시 삭제
 * - GET  /api/storage/post-images/:owner/:file  이미지 조회 → 서명 GET URL로 302 (공개 URL이 없는 저장소용)
 */
export function createStorageRouter(images: StorageAdapter | null, opts: { rateLimits?: boolean; quota?: UploadQuota } = {}) {
  const router = Router()
  const quota = opts.quota ?? new UploadQuota()
  const uploadUrlLimiter = opts.rateLimits === false
    ? (_req: Request, _res: Response, next: NextFunction) => next()
    : rateLimit({
        windowMs: 60_000,
        limit: 30,
        standardHeaders: 'draft-7',
        legacyHeaders: false,
        message: { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' },
      })

  const unavailable = (res: Response) =>
    res.status(503).json({ success: false, error: '파일 저장소가 설정되지 않았습니다.' })

  router.post('/post-images/upload-url', uploadUrlLimiter, requireAuth, async (req, res) => {
    if (!images) return unavailable(res)
    const parsed = postImageUploadBody.safeParse(req.body)
    if (!parsed.success) {
      return res.status(400).json({ success: false, error: '지원하지 않는 이미지 형식입니다. (jpg, png, webp, gif)' })
    }
    const { contentType, size } = parsed.data
    if (size > POST_IMAGE_MAX_BYTES) {
      return res.status(413).json({ success: false, error: `이미지는 ${POST_IMAGE_MAX_BYTES / 1024 / 1024}MB 이하만 올릴 수 있습니다.` })
    }
    const userId = res.locals.userId as string
    if (!isPostImageOwner(userId)) {
      return res.status(400).json({ success: false, error: '잘못된 사용자입니다.' })
    }

    if (!quota.tryConsume(userId, size)) {
      return res.status(429).json({ success: false, error: '오늘 올릴 수 있는 이미지 수·용량을 넘었습니다. 내일 다시 시도해주세요.' })
    }

    const key = newPostImageKey(userId, contentType)
    try {
      const signed = await images.signedPutUrl(key, { contentType, contentLength: size, expiresInSec: POST_IMAGE_UPLOAD_TTL_SEC })
      return res.json({
        success: true,
        key,
        upload: signed,
        url: postImageViewUrl(images, key),
        maxBytes: POST_IMAGE_MAX_BYTES,
      })
    } catch (e) {
      console.error('[Storage] 이미지 업로드 URL 발급 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '업로드 URL을 만들지 못했습니다.' })
    }
  })

  router.post('/post-images/complete', uploadUrlLimiter, requireAuth, async (req, res) => {
    if (!images) return unavailable(res)
    const parsed = postImageCompleteBody.safeParse(req.body)
    if (!parsed.success) return res.status(400).json({ success: false, error: '잘못된 요청입니다.' })
    try {
      const r = await verifyPostImageUpload(images, res.locals.userId as string, parsed.data.key)
      if (!r.ok) return res.status(r.status).json({ success: false, error: r.error })
      return res.json({ success: true, key: parsed.data.key, url: postImageViewUrl(images, parsed.data.key) })
    } catch (e) {
      console.error('[Storage] 이미지 업로드 확인 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '업로드를 확인하지 못했습니다.' })
    }
  })

  router.get('/post-images/:owner/:file', async (req, res) => {
    if (!images) return unavailable(res)
    const key = postImageKeyFromParams(req.params.owner, req.params.file)
    if (!key) return res.status(400).json({ success: false, error: '잘못된 이미지 경로입니다.' })
    try {
      const url = await images.signedGetUrl(key, POST_IMAGE_VIEW_TTL_SEC)
      // 서명 URL은 시간 창 안에서 같다 → 리다이렉트도 짧게 캐시 (남은 서명 유효기간보다 짧게)
      res.setHeader('Cache-Control', 'public, max-age=600')
      return res.redirect(302, url)
    } catch (e) {
      console.error('[Storage] 이미지 URL 서명 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '이미지를 불러오지 못했습니다.' })
    }
  })

  return router
}
