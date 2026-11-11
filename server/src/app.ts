import express from 'express'
import cors from 'cors'
import helmet from 'helmet'
import musicRoutes from './routes/music'
import { createPaymentRouter } from './routes/payment'
import type { SubscriptionService } from './services/subscriptionService'
import { createRoomsRouter } from './routes/rooms'
import type { RoomService } from './services/roomService'
import type { PaymentOrderService } from './services/paymentOrderService'
import { resolveCorsOrigin } from './lib/securityConfig'
import { apiLimiter, paymentLimiter } from './lib/rateLimits'
import { UPLOAD_DIR } from './lib/uploads'
import { createStorageRouter } from './routes/storage'
import { getDefaultStorage } from './storage/config'
import { createSupabaseStorageAdapter } from './storage/supabaseAdapter'
import { supabaseAdmin } from './lib/supabase'
import type { StorageAdapter } from './storage/types'
import type { UploadQuota } from './services/postImageService'
import { createPostsRouter } from './routes/posts'
import { createPostService } from './services/postService'
import { createPostRepository, type PostRepository } from './repositories/postRepository'

export interface AppOptions {
  /** 결제/구독 서비스 (테스트에서 저장소·결제대행 목 주입). 기본: Supabase 저장소 */
  subscriptionService?: SubscriptionService
  /** 방 생성/비밀번호 서비스 (테스트에서 메모리 저장소 주입). 기본: Supabase */
  roomService?: RoomService
  /** 결제 주문 서비스 (테스트에서 주문 저장소 목 주입). 기본: Supabase 저장소 + subscriptionService */
  paymentOrderService?: PaymentOrderService
  /** 게시글 이미지 저장소 (테스트에서 가짜 주입). 기본: STORAGE_DRIVER 설정의 post-images */
  imageStorage?: StorageAdapter | null
  /** 이미지 업로드 사용자별 상한 (테스트 주입). 기본: 24h 100개·200MB */
  imageUploadQuota?: UploadQuota
  /** 예전 Supabase 이미지 원본 확인용 (테스트 주입). 기본: Supabase post-images */
  legacyImageStorage?: StorageAdapter | null
  /** /complete·/resolve 사용자별 분당 상한 (테스트 주입) */
  imagePerUserPerMinute?: number
  /** 게시글 쓰기 저장소 (테스트에서 메모리 주입). 기본: Supabase service_role */
  postRepository?: PostRepository | null
  /** 테스트에서만 false (한 IP에서 많은 요청을 보낸다). 기본: 켬 */
  rateLimits?: boolean
}

/**
 * Express 앱 생성 (listen 없음 — index.ts와 테스트에서 사용)
 * 계층: routes(HTTP 입출력·검증) → services(업무 로직) → repositories(Supabase 접근)
 */
export function createApp(options: AppOptions = {}) {
  const cors_ = resolveCorsOrigin()
  if (cors_.mode === 'dev-all') {
    console.warn('[SECURITY] CORS_ORIGINS not set (development) - allowing all origins')
  } else if (cors_.mode === 'tauri-only') {
    console.warn('[SECURITY] CORS_ORIGINS not set - only Tauri app origins are allowed (browser origins rejected)')
  }

  const app = express()
  app.disable('x-powered-by')
  app.set('trust proxy', 1) // fly.io 프록시 뒤에서 동작 (rate limit이 실제 클라이언트 IP를 쓰도록)
  app.use(helmet({
    // 업로드 파일(/uploads)을 다른 origin(웹/앱)에서 재생·표시할 수 있도록
    crossOriginResourcePolicy: { policy: 'cross-origin' },
  }))
  app.use(cors({
    origin: cors_.origin,
    methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
    allowedHeaders: ['Content-Type', 'Authorization'],
  }))
  app.use(express.json({ limit: '1mb' }))
  app.use(express.urlencoded({ extended: true, limit: '1mb' }))

  // 업로드 파일 정적 서빙: 브라우저가 문서로 렌더링하지 않도록 attachment + nosniff(helmet)
  app.use('/uploads', express.static(UPLOAD_DIR, {
    dotfiles: 'deny',
    index: false,
    setHeaders: (res) => {
      res.setHeader('Content-Disposition', 'attachment')
    },
  }))

  // API 라우트
  const limit = options.rateLimits === false
    ? (_req: express.Request, _res: express.Response, next: express.NextFunction) => next()
    : null
  app.use('/api', limit ?? apiLimiter)
  app.use('/api/music', musicRoutes)
  app.use('/api/payment', limit ?? paymentLimiter, createPaymentRouter(options.subscriptionService, options.paymentOrderService))
  app.use('/api/rooms', createRoomsRouter(options.roomService))
  const imageStorage = options.imageStorage !== undefined ? options.imageStorage : getDefaultStorage('post-images')
  app.use('/api/posts', createPostsRouter(
    createPostService(
      options.postRepository !== undefined ? options.postRepository : supabaseAdmin ? createPostRepository(supabaseAdmin) : null,
      imageStorage,
    ),
    { rateLimits: options.rateLimits },
  ))
  app.use('/api/storage', createStorageRouter(
    imageStorage,
    {
      rateLimits: options.rateLimits,
      quota: options.imageUploadQuota,
      legacyImages: options.legacyImageStorage !== undefined
        ? options.legacyImageStorage
        : supabaseAdmin ? createSupabaseStorageAdapter(supabaseAdmin, 'post-images') : null,
      perUserPerMinute: options.imagePerUserPerMinute,
    },
  ))

  // 헬스체크
  app.get('/health', (_req, res) => {
    res.json({ status: 'ok', timestamp: new Date().toISOString() })
  })

  return app
}
