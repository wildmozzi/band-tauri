import 'dotenv/config'

import { createServer as createHttpServer } from 'http'
import { createServer as createHttpsServer } from 'https'
import { readFileSync, existsSync } from 'fs'
import { createApp } from './app'
import { supabaseAdmin as supabase } from './lib/supabase'
import type { WebSocketServer } from 'ws'
import { initializeWebSocket, wsActivity } from './services/wsService'
import { scheduleCleanup } from './services/cleanupService'
import { getDefaultSubscriptionService, scheduleSubscriptionJobs } from './services/subscriptionService'
import { createDefaultPaymentOrderService, scheduleOrderJobs } from './services/paymentOrderService'
import { createRoomRepository } from './repositories/roomRepository'
import { requirePublicApiUrl } from './lib/securityConfig'
import { shouldRunMaintenance } from './lib/maintenanceMode'

// 운영에서 PUBLIC_API_URL이 없으면 여기서 실패한다 (게시글 이미지 URL이 상대 경로로 저장되는 것을 막음)
requirePublicApiUrl()

const PORT = Number(process.env.PORT || 8080)

// 운영 집계(GET /api/ops/activity, P1-17①)는 아래에서 WS 서버를 만든 뒤부터 값을 돌려준다
let wss: WebSocketServer | null = null
const app = createApp({ activity: () => (wss ? wsActivity(wss) : null) })

if (supabase) {
  console.log('[DB] Supabase client initialized - participant sync enabled')
  // 전역 유지보수는 그 DB의 주 서버(운영)만 한다 — 로컬 개발 서버도 운영 DB를 쓰기 때문 (lib/maintenanceMode.ts)
  const maintenance = shouldRunMaintenance()
  if (maintenance.run) {
    console.log(`[MAINT] 전역 유지보수 실행 (${maintenance.reason})`)
    createRoomRepository(supabase)
      .resetAllParticipantCounts()
      .then(
        () => console.log('[DB] All room participants reset to 0'),
        (err) => console.error('[DB] Failed to reset participants:', err instanceof Error ? err.message : err),
      )
    scheduleCleanup(supabase)
    scheduleSubscriptionJobs()
    scheduleOrderJobs(createDefaultPaymentOrderService(getDefaultSubscriptionService()))
  } else {
    console.log(`[MAINT] 전역 유지보수(참가자 수 초기화, 정리·구독·주문 주기 작업) 건너뜀: ${maintenance.reason}`)
  }
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
  wss = initializeWebSocket(server, supabase)
  server.listen(PORT, () => {
    console.log(`WSS signaling server listening on wss://localhost:${PORT}`)
  })
} else {
  const server = createHttpServer(app)
  wss = initializeWebSocket(server, supabase)
  server.listen(PORT, () => {
    console.log(`HTTP + WS server listening on port ${PORT}`)
    console.log(`- HTTP API: http://localhost:${PORT}/api`)
    console.log(`- WebSocket: ws://localhost:${PORT}`)
  })
}
