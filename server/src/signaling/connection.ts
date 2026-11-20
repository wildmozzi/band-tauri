import type { IncomingMessage } from 'http'
import type { Env } from './types'

export function clientIp(req: IncomingMessage, env: Env): string {
  // fly.io 프록시 뒤에서는 Fly-Client-IP가 실제 클라이언트 IP (fly 밖에서는 위조 가능하므로 사용 안 함)
  if (env.FLY_APP_NAME) {
    const h = req.headers['fly-client-ip']
    const v = Array.isArray(h) ? h[0] : h
    if (v) return v
  }
  return req.socket.remoteAddress || 'unknown'
}
