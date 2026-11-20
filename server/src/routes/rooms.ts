import { Router, type Response } from 'express'
import { rateLimit } from 'express-rate-limit'
import { requireAuth } from '../lib/auth'
import { StoreUnavailableError } from '../repositories/errors'
import { ServiceError } from '../services/errors'
import { getDefaultRoomService, type RoomService } from '../services/roomService'
import { createRoomBody, roomIdParams, setRoomPasswordBody } from './schemas'

/**
 * 방 API (P2-6) — HTTP 입출력과 입력 검증만. 업무 로직은 RoomService.
 * - POST /api/rooms: 방 생성(공개/비공개, 선택적 비밀번호). 방장 = 토큰의 userId
 * - PUT /api/rooms/:id/password: 방장이 비밀번호 설정/해제
 * 방 조회/수정/삭제는 기존대로 클라이언트가 Supabase(RLS)로 한다. 비밀번호 검증은 WS join에서.
 */


function sendServiceError(res: Response, error: unknown): boolean {
  if (error instanceof ServiceError) {
    res.status(error.status).json(error.body)
    return true
  }
  if (error instanceof StoreUnavailableError) {
    res.status(503).json({ success: false, error: '방 저장소를 사용할 수 없습니다. 잠시 후 다시 시도해 주세요.' })
    return true
  }
  return false
}

export function createRoomsRouter(service: RoomService = getDefaultRoomService()) {
  const router = Router()
  // 방 생성·비밀번호 변경: IP당 분당 30회 (scrypt CPU 소모 방지)
  router.use(rateLimit({
    windowMs: 60_000,
    limit: 30,
    standardHeaders: 'draft-7',
    legacyHeaders: false,
    message: { success: false, error: '요청이 너무 많습니다. 잠시 후 다시 시도해주세요.' },
  }), requireAuth)

  router.post('/', async (req, res) => {
    const parsed = createRoomBody.safeParse(req.body ?? {})
    if (!parsed.success) {
      res.status(400).json({ success: false, error: '입력값이 올바르지 않습니다.' })
      return
    }
    try {
      const room = await service.create(res.locals.userId as string, parsed.data)
      res.status(201).json({ success: true, room })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('[ROOMS] create failed:', error instanceof Error ? error.message : error)
      res.status(500).json({ success: false, error: '방 생성에 실패했습니다.' })
    }
  })

  router.put('/:id/password', async (req, res) => {
    const params = roomIdParams.safeParse(req.params)
    const body = setRoomPasswordBody.safeParse(req.body ?? {})
    if (!params.success || !body.success) {
      res.status(400).json({ success: false, error: '입력값이 올바르지 않습니다.' })
      return
    }
    try {
      const result = await service.setPassword(res.locals.userId as string, params.data.id, body.data.password)
      res.json({ success: true, ...result })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('[ROOMS] set password failed:', error instanceof Error ? error.message : error)
      res.status(500).json({ success: false, error: '비밀번호 변경에 실패했습니다.' })
    }
  })

  return router
}
