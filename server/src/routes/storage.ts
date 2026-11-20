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
import {
  UploadQuota,
  VerifiedKeys,
  pendingKeyOf,
  resolvePostImageUrl,
  verifyPostImageUpload,
} from '../services/postImageService'
import { postImageCompleteBody, postImageResolveBody, postImageUploadBody } from './schemas'

/**
 * 파일 저장소 API (P3-10)
 * - POST /api/storage/post-images/upload-url  (인증) 서명 PUT URL 발급 — 업로드 위치는 `pending/<key>` (사용자별 24h 상한)
 * - POST /api/storage/post-images/complete    (인증) 업로드 확인: 크기·MIME·앞부분 매직 바이트(Range) 검증 →
 *                                              통과하면 최종 경로 `<key>`로 옮기고 조회 URL을 준다. 실패 시 삭제. 사용자별 분당 상한
 * - POST /api/storage/post-images/resolve     (인증) 게시글 저장 전 이미지 URL 정규화: 검증 완료(최종 경로) 이미지만 허용,
 *                                              이전된 Supabase URL은 새 조회 URL로 매핑, 원본이 없으면 거부
 * - GET  /api/storage/post-images/:owner/:file  이미지 조회 → 서명 GET URL로 302 (공개 URL이 없는 저장소용)
 */
export function createStorageRouter(
  images: StorageAdapter | null,
  opts: {
    rateLimits?: boolean
    quota?: UploadQuota
    verified?: VerifiedKeys
    /** 예전(Supabase) 이미지 원본 확인용. 현재 드라이버가 supabase면 images와 같다 */
    legacyImages?: StorageAdapter | null
    /** /complete·/resolve 사용자별 분당 상한 (기본 30) */
    perUserPerMinute?: number
  } = {},
) {
  const router = Router()
  const quota = opts.quota ?? new UploadQuota()
  const verified = opts.verified ?? new VerifiedKeys()
  const passthrough = (_req: Request, _res: Response, next: NextFunction) => next()
  const limitMessage = { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' }
  const uploadUrlLimiter = opts.rateLimits === false
    ? passthrough
    : rateLimit({ windowMs: 60_000, limit: 30, standardHeaders: 'draft-7', legacyHeaders: false, message: limitMessage })
  // 사용자 단위 상한: requireAuth 뒤에 둔다 (IP를 바꿔도 같은 계정이면 막힘, 리뷰2 M4)
  const perUser = rateLimit({
    windowMs: 60_000,
    limit: opts.perUserPerMinute ?? 30,
    standardHeaders: 'draft-7',
    legacyHeaders: false,
    message: limitMessage,
    keyGenerator: (_req, res) => `u:${String(res.locals.userId)}`,
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
      // 미검증 업로드는 pending/ 에만 쓸 수 있다. 조회 URL은 /complete가 검증 후에만 준다
      const signed = await images.signedPutUrl(pendingKeyOf(key), { contentType, contentLength: size, expiresInSec: POST_IMAGE_UPLOAD_TTL_SEC })
      return res.json({ success: true, key, upload: signed, maxBytes: POST_IMAGE_MAX_BYTES })
    } catch (e) {
      console.error('[Storage] 이미지 업로드 URL 발급 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '업로드 URL을 만들지 못했습니다.' })
    }
  })

  router.post('/post-images/complete', uploadUrlLimiter, requireAuth, opts.rateLimits === false && !opts.perUserPerMinute ? passthrough : perUser, async (req, res) => {
    if (!images) return unavailable(res)
    const parsed = postImageCompleteBody.safeParse(req.body)
    if (!parsed.success) return res.status(400).json({ success: false, error: '잘못된 요청입니다.' })
    try {
      const r = await verifyPostImageUpload(images, res.locals.userId as string, parsed.data.key, verified)
      if (!r.ok) return res.status(r.status).json({ success: false, error: r.error })
      return res.json({ success: true, key: parsed.data.key, url: postImageViewUrl(images, parsed.data.key) })
    } catch (e) {
      console.error('[Storage] 이미지 업로드 확인 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '업로드를 확인하지 못했습니다.' })
    }
  })

  router.post('/post-images/resolve', uploadUrlLimiter, requireAuth, opts.rateLimits === false && !opts.perUserPerMinute ? passthrough : perUser, async (req, res) => {
    if (!images) return unavailable(res)
    const parsed = postImageResolveBody.safeParse(req.body)
    if (!parsed.success) return res.status(400).json({ success: false, error: '잘못된 요청입니다.' })
    try {
      const legacy = opts.legacyImages !== undefined ? opts.legacyImages : null
      const results = []
      for (const u of parsed.data.urls) {
        results.push(await resolvePostImageUrl(u, { images, legacy, viewUrl: k => postImageViewUrl(images, k) }))
      }
      return res.json({
        success: true,
        urls: results.filter(r => r.ok).map(r => (r as { url: string }).url),
        rejected: results.filter(r => !r.ok).map(r => ({ url: r.input, reason: (r as { reason: string }).reason })),
      })
    } catch (e) {
      console.error('[Storage] 이미지 URL 정규화 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '이미지를 확인하지 못했습니다.' })
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
