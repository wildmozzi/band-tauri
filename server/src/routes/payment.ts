import { Router, type Response } from 'express'
import { PLAN_PRICES } from '../types/payment'
import { requireAuth } from '../lib/auth'
import { StoreUnavailableError } from '../repositories/errors'
import { ServiceError } from '../services/errors'
import { getDefaultSubscriptionService, type SubscriptionService } from '../services/subscriptionService'
import { cancelPaymentBody, changePlanBody, confirmPaymentBody } from './schemas'

/**
 * 결제/구독 API (H2) — HTTP 입출력과 입력 검증만. 업무 로직은 SubscriptionService.
 * - /plans 를 제외한 모든 라우트는 Authorization: Bearer <Supabase access token> 필수
 * - userId는 서버가 토큰으로 결정한다. body/URL의 userId는 무시한다.
 * - 결제 금액은 서버의 PLAN_PRICES로만 결정한다(body의 amount는 무시).
 * - 결제 저장소(DB)를 쓸 수 없으면 503.
 */

const PLANS = {
  free: {
    name: '무료 플랜',
    price: PLAN_PRICES.free,
    features: [
      '합주실 최대 4명',
      '기본 녹음 기능',
      '로컬 저장만 가능'
    ]
  },
  standard: {
    name: 'Standard 플랜',
    price: PLAN_PRICES.standard,
    features: [
      '합주실 최대 6명',
      '비공개 방 생성',
      '클라우드 저장 30일',
      'Mix Lab 기본 기능'
    ]
  },
  pro: {
    name: 'Pro 플랜',
    price: PLAN_PRICES.pro,
    features: [
      '합주실 최대 8명',
      '자동 믹싱 기능',
      '클라우드 저장 무제한',
      'Mix Lab 고급 기능',
      '팀 관리 기능'
    ]
  }
}

/** service 오류를 HTTP 응답으로. 처리했으면 true */
function sendServiceError(res: Response, error: unknown): boolean {
  if (error instanceof ServiceError) {
    res.status(error.status).json(error.body)
    return true
  }
  if (error instanceof StoreUnavailableError) {
    res.status(503).json({ success: false, error: '결제 시스템을 일시적으로 사용할 수 없습니다. 잠시 후 다시 시도해 주세요.' })
    return true
  }
  return false
}

export function createPaymentRouter(service: SubscriptionService = getDefaultSubscriptionService()) {
  const router = Router()

  // 플랜 정보 조회 (공개)
  router.get('/plans', (_req, res) => {
    res.json({ success: true, plans: PLANS })
  })

  // 이하 전부 인증 필요
  router.use(requireAuth)

  // 결제 승인
  router.post('/confirm', async (req, res) => {
    const parsed = confirmPaymentBody.safeParse(req.body ?? {})
    if (!parsed.success) {
      return res.status(400).json({
        success: false,
        error: '필수 파라미터가 누락되었거나 올바르지 않습니다.'
      })
    }
    try {
      const { paymentKey, orderId, planType } = parsed.data
      const subscription = await service.confirmPayment(res.locals.userId as string, paymentKey, orderId, planType)
      res.json({
        success: true,
        subscription: {
          id: subscription.id,
          planType: subscription.planType,
          status: subscription.status,
          startDate: subscription.startDate,
          endDate: subscription.endDate
        }
      })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('Payment confirmation error:', error)
      res.status(500).json({
        success: false,
        error: '결제 처리 중 오류가 발생했습니다.'
      })
    }
  })

  // 결제 취소: 본인의 활성 구독에 속한 paymentKey만 취소 가능
  router.post('/cancel', async (req, res) => {
    const parsed = cancelPaymentBody.safeParse(req.body ?? {})
    if (!parsed.success) {
      return res.status(400).json({ success: false, error: '잘못된 요청입니다.' })
    }
    try {
      const { paymentKey, cancelReason } = parsed.data
      const reason = cancelReason && cancelReason.trim() ? cancelReason.slice(0, 200) : '사용자 요청'
      await service.cancelPayment(res.locals.userId as string, paymentKey, reason)
      res.json({
        success: true,
        message: '결제가 취소되었습니다.'
      })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('Payment cancellation error:', error)
      res.status(500).json({
        success: false,
        error: '결제 취소 중 오류가 발생했습니다.'
      })
    }
  })

  // 내 구독 정보 조회
  async function sendSubscription(userId: string, res: Response) {
    try {
      const user = await service.getUserSubscription(userId)
      res.json({ success: true, user })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('Subscription lookup error:', error)
      res.status(500).json({ success: false, error: '구독 정보 조회 중 오류가 발생했습니다.' })
    }
  }

  router.get('/subscription', async (_req, res) => {
    await sendSubscription(res.locals.userId as string, res)
  })

  // 하위 호환: URL의 userId는 본인일 때만 허용
  router.get('/subscription/:userId', async (req, res) => {
    const userId = res.locals.userId as string
    if (req.params.userId !== userId) {
      return res.status(403).json({ success: false, error: '본인의 구독 정보만 조회할 수 있습니다.' })
    }
    await sendSubscription(userId, res)
  })

  // 플랜 변경: 무료로의 변경(구독 해지)만 허용. 유료 플랜은 /confirm 결제를 통해서만.
  router.post('/change-plan', async (req, res) => {
    const parsed = changePlanBody.safeParse(req.body ?? {})
    if (!parsed.success) {
      return res.status(400).json({
        success: false,
        error: '유효하지 않은 플랜입니다.'
      })
    }
    if (parsed.data.newPlan !== 'free') {
      return res.status(403).json({
        success: false,
        error: '유료 플랜은 결제를 통해서만 변경할 수 있습니다.'
      })
    }
    try {
      await service.downgradeToFree(res.locals.userId as string)
      res.json({
        success: true,
        message: '플랜이 변경되었습니다.',
        newPlan: 'free'
      })
    } catch (error) {
      if (sendServiceError(res, error)) return
      console.error('Plan change error:', error)
      res.status(500).json({
        success: false,
        error: '플랜 변경 중 오류가 발생했습니다.'
      })
    }
  })

  return router
}
