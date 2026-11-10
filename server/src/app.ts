import express from 'express'
import cors from 'cors'
import helmet from 'helmet'
import musicRoutes from './routes/music'
import { createPaymentRouter } from './routes/payment'
import type { SubscriptionService } from './services/subscriptionService'
import { createRoomsRouter } from './routes/rooms'
import type { RoomService } from './services/roomService'
import { resolveCorsOrigin } from './lib/securityConfig'
import { apiLimiter, paymentLimiter } from './lib/rateLimits'
import { UPLOAD_DIR } from './lib/uploads'

export interface AppOptions {
  /** 결제/구독 서비스 (테스트에서 저장소·결제대행 목 주입). 기본: Supabase 저장소 */
  subscriptionService?: SubscriptionService
  /** 방 생성/비밀번호 서비스 (테스트에서 메모리 저장소 주입). 기본: Supabase */
  roomService?: RoomService
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
  app.use('/api', apiLimiter)
  app.use('/api/music', musicRoutes)
  app.use('/api/payment', paymentLimiter, createPaymentRouter(options.subscriptionService))
  app.use('/api/rooms', createRoomsRouter(options.roomService))

  // 헬스체크
  app.get('/health', (_req, res) => {
    res.json({ status: 'ok', timestamp: new Date().toISOString() })
  })

  return app
}
