import { useState } from 'react'
import { Navigate, useNavigate, useSearchParams } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { tossPaymentsService } from '../services/tossPayments'
import { paymentApi } from '../services/paymentApi'
import { SERVICE_NAME } from '../lib/brand'

interface PlanInfo {
  name: string
  price: number
  period: string
  features: string[]
  color: string
}

const PLAN_INFO: Record<string, PlanInfo> = {
  standard: {
    name: 'Standard 플랜',
    price: 2900,
    period: '월',
    features: [
      '합주실 최대 6명',
      '비공개 방 생성',
      '클라우드 저장 30일',
      'Mix Lab 기본 기능',
      '광고 제거'
    ],
    color: 'var(--indigo-400)'
  },
  pro: {
    name: 'Pro 플랜',
    price: 6900,
    period: '월',
    features: [
      '합주실 최대 8명',
      '자동 믹싱 기능',
      '클라우드 저장 무제한',
      'Mix Lab 고급 기능',
      '팀 관리 기능',
      '세션 히스토리'
    ],
    color: 'var(--pink-400)'
  }
}

export function Payment() {
  const { user, loading } = useAuth()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()

  const planType = searchParams.get('plan') as 'standard' | 'pro'
  const [isProcessing, setIsProcessing] = useState(false)
  const [agreementChecked, setAgreementChecked] = useState(false)

  // 유효한 플랜인지 체크 (렌더 중 navigate() 대신 <Navigate> — P3-12 ⑥)
  if (!planType || !PLAN_INFO[planType]) {
    return <Navigate to="/pricing" replace />
  }

  // 로그인 체크: 세션 복원이 끝난 뒤에만 판단한다. 복원 전(user=null)에 /auth로 보내면
  // 로그인 상태로 결제 URL을 직접 열었을 때 결제 화면에 도달하지 못한다 (P3-12 ⑩)
  if (loading) {
    return (
      <div className="payment-page">
        <p className="payment-auth-checking" role="status">로그인 상태를 확인하는 중...</p>
      </div>
    )
  }
  if (!user) {
    return <Navigate to="/auth" replace state={{ from: `/payment?plan=${planType}` }} />
  }

  const plan = PLAN_INFO[planType]

  const handlePayment = async () => {
    if (!agreementChecked) {
      alert('이용약관에 동의해주세요.')
      return
    }

    if (!user || !user.email) {
      alert('로그인이 필요합니다.')
      return
    }

    setIsProcessing(true)

    try {
      // 주문은 서버가 발급한다 (orderId·금액 = 서버 가격표, 30분 유효)
      const created = await paymentApi.createOrder(planType)
      if (!created.success || !created.order) {
        throw new Error(created.error || '주문 생성에 실패했습니다. 다시 시도해주세요.')
      }
      const { orderId, amount } = created.order

      // 결제 요청 데이터 (토스가 successUrl에 paymentKey·orderId·amount를 붙여 돌려준다)
      const paymentRequest = {
        amount,
        orderId,
        orderName: `${SERVICE_NAME} ${planType} Plan`,
        customerName: user.email?.split('@')[0] || 'user', // 이메일의 @ 앞부분 사용
        customerEmail: user.email || '',
        successUrl: `${window.location.origin}/payment/success?planType=${planType}`,
        failUrl: `${window.location.origin}/payment/fail`
      }

      // 토스페이먼츠 결제 화면으로 이동 (결제 방법 선택 포함)
      await tossPaymentsService.requestPayment(paymentRequest)
    } catch (error) {
      console.error('Payment request failed:', error)
      const errorMessage = error instanceof Error ? error.message : '결제 요청 중 오류가 발생했습니다. 다시 시도해주세요.'
      alert(errorMessage)
    } finally {
      setIsProcessing(false)
    }
  }



  return (
    <div className="payment-page">
      <div className="payment-container">
        <div className="payment-header">
          <button
            className="back-button"
            onClick={() => navigate('/pricing')}
          >
            ← 뒤로가기
          </button>
          <h1>결제하기</h1>
        </div>

        <div className="payment-content">
          {/* 플랜 정보 */}
          <div className="plan-summary">
            <div className="plan-summary-header">
              <h2>선택한 플랜</h2>
            </div>
            <div className="plan-summary-content">
              <div className="plan-info">
                <h3 style={{ color: plan.color }}>{plan.name}</h3>
                <div className="plan-price">
                  <span className="price">₩{plan.price.toLocaleString()}</span>
                  <span className="period">/{plan.period}</span>
                </div>
              </div>
              <div className="plan-features">
                <h4>포함된 기능</h4>
                <ul>
                  {plan.features.map((feature, index) => (
                    <li key={index}>✅ {feature}</li>
                  ))}
                </ul>
              </div>
            </div>
          </div>



          {/* 결제 정보 */}
          <div className="payment-summary">
            <h3>결제 정보</h3>
            <div className="payment-details">
              <div className="payment-row">
                <span>플랜</span>
                <span>{plan.name}</span>
              </div>
              <div className="payment-row">
                <span>결제 금액</span>
                <span>₩{plan.price.toLocaleString()}</span>
              </div>
              <div className="payment-row">
                <span>결제 주기</span>
                <span>매월 자동결제</span>
              </div>
              <div className="payment-row total">
                <span>총 결제 금액</span>
                <span>₩{plan.price.toLocaleString()}</span>
              </div>
            </div>
          </div>

          {/* 약관 동의 */}
          <div className="agreement-section">
            <label className="agreement-checkbox">
              <input
                type="checkbox"
                checked={agreementChecked}
                onChange={(e) => setAgreementChecked(e.target.checked)}
              />
              <span className="checkmark"></span>
              <span className="agreement-text">
                <a href="#" onClick={(e) => e.preventDefault()}>이용약관</a> 및
                <a href="#" onClick={(e) => e.preventDefault()}> 개인정보처리방침</a>에 동의합니다.
              </span>
            </label>
          </div>

          {/* 결제 버튼 */}
          <button
            className="payment-submit-btn"
            onClick={handlePayment}
            disabled={!agreementChecked || isProcessing}
          >
            {isProcessing ? (
              <>
                <span className="loading-spinner"></span>
                결제 처리 중...
              </>
            ) : (
              `₩${plan.price.toLocaleString()} 결제하기`
            )}
          </button>

          {/* 안내사항 */}
          <div className="payment-notice">
            <h4>📋 결제 안내</h4>
            <ul>
              <li>결제 버튼을 클릭하면 토스페이먼츠 결제 화면으로 이동합니다.</li>
              <li>다양한 결제 방법(카드, 계좌이체, 간편결제 등)을 선택할 수 있습니다.</li>
              <li>매월 같은 날짜에 자동으로 결제됩니다.</li>
              <li>언제든지 플랜을 변경하거나 해지할 수 있습니다.</li>
              <li>7일 무조건 환불 보장 정책을 제공합니다.</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  )
}