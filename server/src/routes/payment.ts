import { Router } from 'express'
import { paymentService } from '../services/paymentService'
import { subscriptionService } from '../services/subscriptionService'
import { PLAN_PRICES } from '../types/payment'

const router = Router()

// 결제 승인
router.post('/confirm', async (req, res) => {
  try {
    const { paymentKey, orderId, amount, userId, planType } = req.body

    // 입력 검증
    if (!paymentKey || !orderId || !amount || !userId || !planType) {
      return res.status(400).json({
        success: false,
        error: '필수 파라미터가 누락되었습니다.'
      })
    }

    // 플랜 및 금액 검증
    const validation = paymentService.validatePaymentRequest({
      userId,
      planType,
      paymentMethod: 'card', // 임시
      amount
    })

    if (!validation.valid) {
      return res.status(400).json({
        success: false,
        error: validation.error
      })
    }

    // 토스페이먼츠 결제 승인
    const paymentResult = await paymentService.confirmPayment(paymentKey, orderId, amount)

    if (!paymentResult.success) {
      return res.status(400).json(paymentResult)
    }

    // 구독 생성
    const subscription = subscriptionService.createSubscription(
      userId,
      planType,
      paymentKey,
      amount
    )

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

// 결제 취소
router.post('/cancel', async (req, res) => {
  try {
    const { paymentKey, cancelReason, userId } = req.body

    if (!paymentKey || !userId) {
      return res.status(400).json({
        success: false,
        error: '필수 파라미터가 누락되었습니다.'
      })
    }

    // 사용자의 구독 확인
    const user = subscriptionService.getUser(userId)
    if (!user || !user.subscriptionId) {
      return res.status(404).json({
        success: false,
        error: '활성 구독을 찾을 수 없습니다.'
      })
    }

    // 토스페이먼츠 결제 취소
    const cancelResult = await paymentService.cancelPayment(
      paymentKey, 
      cancelReason || '사용자 요청'
    )

    if (!cancelResult.success) {
      return res.status(400).json(cancelResult)
    }

    // 구독 취소
    const cancelled = subscriptionService.cancelSubscription(user.subscriptionId)

    if (!cancelled) {
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

// 사용자 구독 정보 조회
router.get('/subscription/:userId', (req, res) => {
  try {
    const { userId } = req.params

    const user = subscriptionService.getUser(userId)

    // 사용자가 없으면 기본 Free 플랜 반환
    if (!user) {
      return res.json({
        success: true,
        user: {
          id: userId,
          planType: 'free',
          subscription: null
        }
      })
    }

    const subscription = user.subscriptionId
      ? subscriptionService.getSubscription(user.subscriptionId)
      : null

    res.json({
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
  } catch (error) {
    console.error('Subscription inquiry error:', error)
    res.status(500).json({
      success: false,
      error: '구독 정보 조회 중 오류가 발생했습니다.'
    })
  }
})

// 플랜 변경
router.post('/change-plan', (req, res) => {
  try {
    const { userId, newPlan } = req.body

    if (!userId || !newPlan) {
      return res.status(400).json({
        success: false,
        error: '필수 파라미터가 누락되었습니다.'
      })
    }

    if (!['free', 'standard', 'pro'].includes(newPlan)) {
      return res.status(400).json({
        success: false,
        error: '유효하지 않은 플랜입니다.'
      })
    }

    const success = subscriptionService.changeUserPlan(userId, newPlan)

    if (!success) {
      return res.status(404).json({
        success: false,
        error: '사용자를 찾을 수 없습니다.'
      })
    }

    res.json({
      success: true,
      message: '플랜이 변경되었습니다.',
      newPlan
    })
  } catch (error) {
    console.error('Plan change error:', error)
    res.status(500).json({
      success: false,
      error: '플랜 변경 중 오류가 발생했습니다.'
    })
  }
})

// 플랜 정보 조회
router.get('/plans', (req, res) => {
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

export default router