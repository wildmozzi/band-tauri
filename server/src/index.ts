import 'dotenv/config'

import express from 'express'
import cors from 'cors'
import { createServer as createHttpServer } from 'http'
import { createServer as createHttpsServer } from 'https'
import { readFileSync, existsSync } from 'fs'
import { createClient } from '@supabase/supabase-js'
import musicRoutes from './routes/music'
import paymentRoutes from './routes/payment'
import { initializeWebSocket } from './services/wsService'
import { scheduleCleanup } from './services/cleanupService'

const PORT = Number(process.env.PORT || 8080)

// Express 앱 설정
const app = express()
app.use(cors({
  origin: '*',
  methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
  allowedHeaders: ['Content-Type', 'Authorization'],
}))
app.use(express.json())
app.use(express.urlencoded({ extended: true }))
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
