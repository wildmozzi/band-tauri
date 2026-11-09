import 'dotenv/config'

import express from 'express'
import cors, { type CorsOptions } from 'cors'
import helmet from 'helmet'
import { createServer as createHttpServer } from 'http'
import { createServer as createHttpsServer } from 'https'
import { readFileSync, existsSync } from 'fs'
import { createClient } from '@supabase/supabase-js'
import musicRoutes from './routes/music'
import paymentRoutes from './routes/payment'
import { initializeWebSocket } from './services/wsService'
import { scheduleCleanup } from './services/cleanupService'

const PORT = Number(process.env.PORT || 8080)

// CORS 허용 도메인: CORS_ORIGINS (쉼표 구분). 예) https://band.vercel.app,https://localhost:5173
// 미설정 시 모든 origin 허용(개발 호환) + 경고. 운영에서는 반드시 설정할 것.
// Tauri 앱 origin(tauri://localhost, http(s)://tauri.localhost)은 항상 허용.
const TAURI_ORIGINS = ['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost']
const corsOrigins = (process.env.CORS_ORIGINS || '')
  .split(',')
  .map(s => s.trim())
  .filter(Boolean)
if (corsOrigins.length === 0) {
  console.warn('[SECURITY] CORS_ORIGINS not set - allowing all origins')
}
const corsOrigin: CorsOptions['origin'] = corsOrigins.length === 0
  ? true
  : [...corsOrigins, ...TAURI_ORIGINS]

// Express 앱 설정
const app = express()
app.disable('x-powered-by')
app.set('trust proxy', 1) // fly.io 프록시 뒤에서 동작
app.use(helmet({
  // 업로드 파일(/uploads)을 다른 origin(웹/앱)에서 재생·표시할 수 있도록
  crossOriginResourcePolicy: { policy: 'cross-origin' },
}))
app.use(cors({
  origin: corsOrigin,
  methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
  allowedHeaders: ['Content-Type', 'Authorization'],
}))
app.use(express.json({ limit: '1mb' }))
app.use(express.urlencoded({ extended: true, limit: '1mb' }))
app.use('/uploads', express.static('uploads'))

// API 라우트
app.use('/api/music', musicRoutes)
app.use('/api/payment', paymentRoutes)

// 헬스체크
app.get('/health', (_req, res) => {
  res.json({ status: 'ok', timestamp: new Date().toISOString() })
})

// Supabase 초기화
const supabaseUrl = process.env.SUPABASE_URL || ''
const supabaseKey = process.env.SUPABASE_SERVICE_KEY || ''
const supabase = supabaseUrl && supabaseKey ? createClient(supabaseUrl, supabaseKey) : null

if (supabase) {
  console.log('[DB] Supabase client initialized - participant sync enabled')
  supabase
    .from('rooms')
    .update({ current_participants: 0 })
    .gt('current_participants', 0)
    .then(({ error }) => {
      if (error) console.error('[DB] Failed to reset participants:', error.message)
      else console.log('[DB] All room participants reset to 0')
    })
  scheduleCleanup(supabase)
} else {
  console.log('[DB] Supabase not configured - participant sync disabled')
  console.log('[DB] Set SUPABASE_URL and SUPABASE_SERVICE_KEY to enable')
}

// HTTP/HTTPS 서버 + WebSocket 초기화
const certPath = './certs/cert.pem'
const keyPath = './certs/key.pem'

if (existsSync(certPath) && existsSync(keyPath)) {
  const server = createHttpsServer({ cert: readFileSync(certPath), key: readFileSync(keyPath) }, app)
  initializeWebSocket(server, supabase)
  server.listen(PORT, () => {
    console.log(`WSS signaling server listening on wss://localhost:${PORT}`)
  })
} else {
  const server = createHttpServer(app)
  initializeWebSocket(server, supabase)
  server.listen(PORT, () => {
    console.log(`HTTP + WS server listening on port ${PORT}`)
    console.log(`- HTTP API: http://localhost:${PORT}/api`)
    console.log(`- WebSocket: ws://localhost:${PORT}`)
  })
}
