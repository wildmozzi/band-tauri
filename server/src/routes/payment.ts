import { Router } from 'express'
import { paymentService } from '../services/paymentService'
import { subscriptionService } from '../services/subscriptionService'
import { PLAN_PRICES, UserPlan } from '../types/payment'
import { requireAuth } from '../lib/auth'

/**
 * 결제/구독 API (H2)
 * - /plans 를 제외한 모든 라우트는 Authorization: Bearer <Supabase access token> 필수
 * - userId는 서버가 토큰으로 결정한다. body/URL의 userId는 무시한다.
 * - 결제 금액은 서버의 PLAN_PRICES로만 결정한다(body의 amount는 무시).
 */
const router = Router()

const PAID_PLANS: UserPlan[] = ['standard', 'pro']

function isPaidPlan(v: unknown): v is UserPlan {
  return typeof v === 'string' && (PAID_PLANS as string[]).includes(v)
}

function isShortString(v: unknown, max = 200): v is string {
  return typeof v === 'string' && v.length > 0 && v.length <= max
}

// 플랜 정보 조회 (공개)
router.get('/plans', (_req, res) => {
  res.json({
    success: true,
    plans: {
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
  })
})

// 이하 전부 인증 필요
router.use(requireAuth)

// 결제 승인
router.post('/confirm', async (req, res) => {
  try {
    const userId = res.locals.userId as string
    const { paymentKey, orderId, planType } = req.body ?? {}

    if (!isShortString(paymentKey) || !isShortString(orderId) || !isPaidPlan(planType)) {
      return res.status(400).json({
        success: false,
        error: '필수 파라미터가 누락되었거나 올바르지 않습니다.'
      })
    }

    // 금액은 서버 가격표로만 결정 (토스가 실제 주문 금액과 대조하므로 불일치하면 승인 실패)
    const amount = PLAN_PRICES[planType]

    const paymentResult = await paymentService.confirmPayment(paymentKey, orderId, amount)

    if (!paymentResult.success) {
      return res.status(400).json(paymentResult)
    }

    const subscription = subscriptionService.createSubscription(userId, planType, paymentKey, amount)

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
    console.error('Payment confirmation error:', error)
    res.status(500).json({
      success: false,
      error: '결제 처리 중 오류가 발생했습니다.'
    })
  }
})

// 결제 취소: 본인의 활성 구독에 속한 paymentKey만 취소 가능
router.post('/cancel', async (req, res) => {
  try {
    const userId = res.locals.userId as string
    const { paymentKey, cancelReason } = req.body ?? {}

    const subscription = subscriptionService.getUserActiveSubscription(userId)
    if (!subscription || !subscription.paymentKey) {
      return res.status(404).json({
        success: false,
        error: '활성 구독을 찾을 수 없습니다.'
      })
    }

    if (paymentKey !== undefined && paymentKey !== subscription.paymentKey) {
      return res.status(403).json({
        success: false,
        error: '본인의 결제만 취소할 수 있습니다.'
      })
    }

    const reason = typeof cancelReason === 'string' && cancelReason.trim()
      ? cancelReason.slice(0, 200)
      : '사용자 요청'

    const cancelResult = await paymentService.cancelPayment(subscription.paymentKey, reason)

    if (!cancelResult.success) {
      return res.status(400).json(cancelResult)
    }

    if (!subscriptionService.cancelSubscription(subscription.id)) {
      return res.status(400).json({
        success: false,
        error: '구독 취소에 실패했습니다.'
      })
    }

    res.json({
      success: true,
      message: '결제가 취소되었습니다.'
    })
  } catch (error) {
    console.error('Payment cancellation error:', error)
    res.status(500).json({
      success: false,
      error: '결제 취소 중 오류가 발생했습니다.'
    })
  }
})

// 내 구독 정보 조회
function sendSubscription(userId: string, res: import('express').Response) {
  const user = subscriptionService.getUser(userId)

  if (!user) {
    return res.json({
      success: true,
      user: { id: userId, planType: 'free', subscription: null }
    })
  }

  const subscription = user.subscriptionId
    ? subscriptionService.getSubscription(user.subscriptionId)
    : null

  return res.json({
    success: true,
    user: {
      id: user.id,
      planType: user.planType,
      subscription: subscription ? {
        id: subscription.id,
        status: subscription.status,
        startDate: subscription.startDate,
        endDate: subscription.endDate,
        amount: subscription.amount
      } : null
    }
  })
}

router.get('/subscription', (_req, res) => {
  sendSubscription(res.locals.userId as string, res)
})

// 하위 호환: URL의 userId는 본인일 때만 허용
router.get('/subscription/:userId', (req, res) => {
  const userId = res.locals.userId as string
  if (req.params.userId !== userId) {
    return res.status(403).json({ success: false, error: '본인의 구독 정보만 조회할 수 있습니다.' })
  }
  sendSubscription(userId, res)
})

// 플랜 변경: 무료로의 변경(구독 해지)만 허용. 유료 플랜은 /confirm 결제를 통해서만.
router.post('/change-plan', (req, res) => {
  try {
    const userId = res.locals.userId as string
    const { newPlan } = req.body ?? {}

    if (!['free', 'standard', 'pro'].includes(newPlan)) {
      return res.status(400).json({
        success: false,
        error: '유효하지 않은 플랜입니다.'
      })
    }

    if (newPlan !== 'free') {
      return res.status(403).json({
        success: false,
        error: '유료 플랜은 결제를 통해서만 변경할 수 있습니다.'
      })
    }

    if (subscriptionService.getUser(userId)) {
      subscriptionService.changeUserPlan(userId, 'free')
    }

    res.json({
      success: true,
      message: '플랜이 변경되었습니다.',
      newPlan: 'free'
    })
  } catch (error) {
    console.error('Plan change error:', error)
    res.status(500).json({
      success: false,
      error: '플랜 변경 중 오류가 발생했습니다.'
    })
  }
})

export default router
