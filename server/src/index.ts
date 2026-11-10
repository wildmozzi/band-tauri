import 'dotenv/config'

import { createServer as createHttpServer } from 'http'
import { createServer as createHttpsServer } from 'https'
import { readFileSync, existsSync } from 'fs'
import { createApp } from './app'
import { supabaseAdmin as supabase } from './lib/supabase'
import { initializeWebSocket } from './services/wsService'
import { scheduleCleanup } from './services/cleanupService'
import { scheduleSubscriptionExpiry } from './services/subscriptionService'
import { createRoomRepository } from './repositories/roomRepository'

const PORT = Number(process.env.PORT || 8080)

const app = createApp()

if (supabase) {
  console.log('[DB] Supabase client initialized - participant sync enabled')
  createRoomRepository(supabase)
    .resetAllParticipantCounts()
    .then(
      () => console.log('[DB] All room participants reset to 0'),
      (err) => console.error('[DB] Failed to reset participants:', err instanceof Error ? err.message : err),
    )
  scheduleCleanup(supabase)
  scheduleSubscriptionExpiry()
} else {
  console.log('[DB] Supabase not configured - participant sync disabled')
  console.log('[DB] Set SUPABASE_URL and SUPABASE_SERVICE_KEY to enable')
  if (process.env.NODE_ENV !== 'development') {
    console.error('[SECURITY] Supabase not configured outside development: all WS clients are anonymous and authenticated APIs return 401')
  }
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
