import { Router, type NextFunction, type Request, type Response } from 'express'
import { rateLimit } from 'express-rate-limit'
import { requireAuth } from '../lib/auth'
import { AVATAR_MAX_BYTES } from '../lib/avatarStorage'
import { POST_IMAGE_UPLOAD_TTL_SEC, POST_IMAGE_VIEW_TTL_SEC, isPostImageOwner, newPostImageKey, postImageKeyFromParams } from '../lib/imageStorage'
import type { StorageAdapter } from '../storage/types'
import { UploadQuota, VerifiedKeys, pendingKeyOf } from '../services/postImageService'
import { completeAvatarUpload, removeAvatar, type ProfilePhotoMetadata } from '../services/profilePhotoService'
import { avatarUploadBody, postImageCompleteBody } from './schemas'

/** 프로필 사진 하루 발급 상한 (사용자별) */
export const AVATAR_QUOTA = { maxCount: 20, maxBytes: 20 * AVATAR_MAX_BYTES }

/**
 * 프로필 사진 API (P2-12) — /api/storage/avatars
 * - POST   /upload-url  (인증) 서명 PUT URL 발급 — `pending/<key>`, 2MB·jpg/png/webp, 사용자별 24h 상한
 * - POST   /complete    (인증) 업로드 검증 → 최종 경로 → user_metadata.profile_photo_url 기록(예전 base64 키 삭제) → 예전 사진 삭제
 * - DELETE /            (인증) 사진 지우기 (metadata 비움 + 사진 삭제)
 * - GET    /:owner/:file  조회 → 서명 GET URL로 302 (공개 URL이 없는 저장소용)
 * 신원은 JWT(requireAuth)로만 정한다 — 요청 본문의 userId·경로는 쓰지 않는다.
 */
export function createAvatarRouter(
  storage: StorageAdapter | null,
  meta: ProfilePhotoMetadata | null,
  opts: { rateLimits?: boolean; quota?: UploadQuota; verified?: VerifiedKeys; perUserPerMinute?: number } = {},
) {
  const router = Router()
  const quota = opts.quota ?? new UploadQuota(AVATAR_QUOTA)
  const verified = opts.verified ?? new VerifiedKeys()
  const passthrough = (_req: Request, _res: Response, next: NextFunction) => next()
  const limitMessage = { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' }
  const ipLimiter = opts.rateLimits === false
    ? passthrough
    : rateLimit({ windowMs: 60_000, limit: 30, standardHeaders: 'draft-7', legacyHeaders: false, message: limitMessage })
  const perUser = opts.rateLimits === false && !opts.perUserPerMinute
    ? passthrough
    : rateLimit({
      windowMs: 60_000,
      limit: opts.perUserPerMinute ?? 10,
      standardHeaders: 'draft-7',
      legacyHeaders: false,
      message: limitMessage,
      keyGenerator: (_req, res) => `avatar:${String(res.locals.userId)}`,
    })

  const unavailable = (res: Response) =>
    res.status(503).json({ success: false, error: '파일 저장소가 설정되지 않았습니다.' })

  router.post('/upload-url', ipLimiter, requireAuth, async (req, res) => {
    if (!storage || !meta) return unavailable(res)
    const parsed = avatarUploadBody.safeParse(req.body)
    if (!parsed.success) {
      return res.status(400).json({ success: false, error: '지원하지 않는 이미지 형식입니다. (jpg, png, webp)' })
    }
    const { contentType, size } = parsed.data
    if (size > AVATAR_MAX_BYTES) {
      return res.status(413).json({ success: false, error: `프로필 사진은 ${AVATAR_MAX_BYTES / 1024 / 1024}MB 이하만 올릴 수 있습니다.` })
    }
    const userId = res.locals.userId as string
    if (!isPostImageOwner(userId)) return res.status(400).json({ success: false, error: '잘못된 사용자입니다.' })
    if (!quota.tryConsume(userId, size)) {
      return res.status(429).json({ success: false, error: '오늘 프로필 사진을 바꿀 수 있는 횟수를 넘었습니다. 내일 다시 시도해주세요.' })
    }
    const key = newPostImageKey(userId, contentType)
    try {
      const upload = await storage.signedPutUrl(pendingKeyOf(key), { contentType, contentLength: size, expiresInSec: POST_IMAGE_UPLOAD_TTL_SEC })
      return res.json({ success: true, key, upload, maxBytes: AVATAR_MAX_BYTES })
    } catch (e) {
      console.error('[Avatar] 업로드 URL 발급 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '업로드 URL을 만들지 못했습니다.' })
    }
  })

  router.post('/complete', ipLimiter, requireAuth, perUser, async (req, res) => {
    if (!storage || !meta) return unavailable(res)
    const parsed = postImageCompleteBody.safeParse(req.body)
    if (!parsed.success) return res.status(400).json({ success: false, error: '잘못된 요청입니다.' })
    try {
      const r = await completeAvatarUpload(storage, meta, res.locals.userId as string, parsed.data.key, verified)
      if (!r.ok) return res.status(r.status).json({ success: false, error: r.error })
      return res.json({ success: true, url: r.url })
    } catch (e) {
      console.error('[Avatar] 업로드 확인 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '프로필 사진을 저장하지 못했습니다.' })
    }
  })

  router.delete('/', ipLimiter, requireAuth, perUser, async (_req, res) => {
    if (!storage || !meta) return unavailable(res)
    try {
      await removeAvatar(storage, meta, res.locals.userId as string)
      return res.json({ success: true })
    } catch (e) {
      console.error('[Avatar] 사진 삭제 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '프로필 사진을 지우지 못했습니다.' })
    }
  })

  router.get('/:owner/:file', async (req, res) => {
    if (!storage) return unavailable(res)
    const key = postImageKeyFromParams(req.params.owner, req.params.file)
    if (!key) return res.status(400).json({ success: false, error: '잘못된 이미지 경로입니다.' })
    try {
      const url = await storage.signedGetUrl(key, POST_IMAGE_VIEW_TTL_SEC)
      res.setHeader('Cache-Control', 'public, max-age=600')
      return res.redirect(302, url)
    } catch (e) {
      console.error('[Avatar] URL 서명 실패:', (e as Error).message)
      return res.status(500).json({ success: false, error: '이미지를 불러오지 못했습니다.' })
    }
  })

  return router
}
