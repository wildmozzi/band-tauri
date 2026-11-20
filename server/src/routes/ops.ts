import { Router } from 'express'
import { createHash, timingSafeEqual } from 'crypto'
import type { ActivityCounts } from '../signaling/registry'

type Env = Record<string, string | undefined>

/** 운영자 토큰 최소 길이. 짧은 값은 추측 가능하므로 경로 자체를 끈다 */
export const OPS_TOKEN_MIN_LENGTH = 32

/** OPS_TOKEN이 쓸 만하면 그 값, 없거나 짧으면 null (경로 꺼짐) */
export function resolveOpsToken(env: Env = process.env): string | null {
  const t = (env.OPS_TOKEN ?? '').trim()
  return t.length >= OPS_TOKEN_MIN_LENGTH ? t : null
}

const digest = (s: string) => createHash('sha256').update(s, 'utf8').digest()

/** `Authorization: Bearer <token>`이 운영자 토큰과 같은가 (길이가 달라도 일정 시간 비교) */
export function isOpsAuthorized(header: string | undefined, token: string): boolean {
  const m = /^Bearer\s+(.+)$/i.exec(header ?? '')
  if (!m) return false
  return timingSafeEqual(digest(m[1].trim()), digest(token))
}

/**
 * 운영 확인 경로 (P1-17①: "합주 중 배포 금지" — 배포 직전 지금 연주 중인 인원 확인, docs/RELEASE.md "서버 배포").
 *
 * GET /api/ops/activity → { activeRooms, performers, viewers, resuming, connections, safeToDeploy, timestamp }
 * - 집계 숫자만 돌려준다(방 id·사용자 정보 없음).
 * - 숫자도 이용 현황(언제 몇 명이 쓰는지)이라 공개하지 않는다: 운영자 토큰(env OPS_TOKEN, 32자 이상) 필요.
 *   사용자 JWT·관리자 역할 체계가 없어서 별도 비밀값으로 보호한다. 값은 fly secrets에만 둔다.
 * - OPS_TOKEN이 없거나 짧으면 404(경로가 없는 것처럼). 토큰이 틀리면 401. WS 서버가 아직 없으면 503.
 * - safeToDeploy = 연주자 0명. 관람자만 있으면 true지만 배포하면 관람이 끊겼다가 다시 붙는다(스크립트가 경고).
 * - 한계: 서버 인스턴스 1대 가정(fly min_machines_running=1). 여러 대면 머신마다 따로 물어야 한다.
 */
export function createOpsRouter(getActivity: () => ActivityCounts | null, env: Env = process.env) {
  const router = Router()
  const token = resolveOpsToken(env)

  router.get('/activity', (req, res) => {
    res.setHeader('Cache-Control', 'no-store')
    if (!token) {
      res.status(404).json({ error: 'Not found' })
      return
    }
    if (!isOpsAuthorized(req.get('authorization'), token)) {
      res.status(401).json({ error: 'Unauthorized' })
      return
    }
    const counts = getActivity()
    if (!counts) {
      res.status(503).json({ error: 'Signaling server not ready' })
      return
    }
    res.json({ ...counts, safeToDeploy: counts.performers === 0, timestamp: new Date().toISOString() })
  })

  return router
}
