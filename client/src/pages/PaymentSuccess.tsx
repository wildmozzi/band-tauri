import { useEffect, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import { usePremium } from '../contexts/PremiumContext'
import { paymentApi } from '../services/paymentApi'

const PLAN_INFO: Record<string, { name: string; price: number }> = {
  standard: { name: 'Standard 플랜', price: 2900 },
  pro: { name: 'Pro 플랜', price: 6900 }
}

export function PaymentSuccess() {
  const { user } = useAuth()
  const { setUserPlan } = usePremium()
  const [searchParams] = useSearchParams()
  const [isProcessing, setIsProcessing] = useState(true)
  const [paymentResult, setPaymentResult] = useState<{
    success: boolean
    planType?: string
    planName?: string
    price?: number
    error?: string
  }>({ success: false })

  // URL 파라미터에서 결제 정보 추출
  const paymentKey = searchParams.get('paymentKey')
  const orderId = searchParams.get('orderId')
  const amount = searchParams.get('amount')
  const planType = searchParams.get('planType')

  useEffect(() => {
    const processPayment = async () => {
      if (!paymentKey || !orderId || !amount || !planType || !user) {
        setPaymentResult({
          success: false,
          error: '결제 정보가 올바르지 않습니다.'
        })
        setIsProcessing(false)
        return
      }

      try {
        // 서버에 결제 승인 요청
        const result = await paymentApi.confirmPayment({
          paymentKey,
          orderId,
          amount: parseInt(amount),
          userId: user.id,
          planType: planType as 'standard' | 'pro'
        })

        if (result.success && result.subscription) {
          // 결제 성공 - 사용자 플랜 업데이트
          setUserPlan(planType as 'standard' | 'pro')
          
          const planInfo = PLAN_INFO[planType]
          setPaymentResult({
            success: true,
            planType,
            planName: planInfo.name,
            price: planInfo.price
          })
        } else {
          setPaymentResult({
            success: false,
            error: result.error || '결제 승인에 실패했습니다.'
          })
        }
      } catch (error) {
        console.error('Payment processing error:', error)
        setPaymentResult({
          success: false,
          error: '결제 처리 중 오류가 발생했습니다.'
        })
      } finally {
        setIsProcessing(false)
      }
    }

    processPayment()
  }, [paymentKey, orderId, amount, planType, user, setUserPlan])

  // 로딩 중
  if (isProcessing) {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <div className="loading-spinner"></div>
          <h2>결제를 처리하고 있습니다...</h2>
          <p>잠시만 기다려주세요.</p>
        </div>
      </div>
    )
  }

  // 결제 실패
  if (!paymentResult.success) {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <div className="error-icon">❌</div>
          <h1>결제 처리에 실패했습니다</h1>
          <p className="error-message">{paymentResult.error}</p>
          <div className="action-buttons">
            <Link to="/pricing" className="primary-action">
              다시 시도하기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="payment-success-page">
      <div className="success-container">
        <div className="success-icon">
          <div className="checkmark">✓</div>
        </div>
        
        <div className="success-content">
          <h1>결제가 완료되었습니다!</h1>
          <p className="success-message">
            <strong>{paymentResult.planName}</strong>을 성공적으로 구독하셨습니다.
          </p>
          
          <div className="payment-info">
            <div className="info-row">
              <span>구독 플랜</span>
              <span>{paymentResult.planName}</span>
            </div>
            <div className="info-row">
              <span>결제 금액</span>
              <span>₩{paymentResult.price?.toLocaleString()}/월</span>
            </div>
            <div className="info-row">
              <span>다음 결제일</span>
              <span>{new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toLocaleDateString()}</span>
            </div>
          </div>

          <div className="next-steps">
            <h3>🎉 이제 다음 기능들을 이용하실 수 있습니다</h3>
            <div className="features-grid">
              {paymentResult.planType === 'standard' && (
                <>
                  <div className="feature-item">
                    <span className="feature-icon">👥</span>
                    <div>
                      <strong>6명 합주실</strong>
                      <p>더 많은 친구들과 함께 연주하세요</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">🔒</span>
                    <div>
                      <strong>비공개 방</strong>
                      <p>팀원들만의 전용 합주 공간</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">☁️</span>
                    <div>
                      <strong>클라우드 저장</strong>
                      <p>녹음 파일을 안전하게 보관</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">🎛️</span>
                    <div>
                      <strong>Mix Lab</strong>
                      <p>전문적인 믹싱 도구 사용</p>
                    </div>
                  </div>
                </>
              )}
              
              {paymentResult.planType === 'pro' && (
                <>
                  <div className="feature-item">
                    <span className="feature-icon">👥</span>
                    <div>
                      <strong>8명 합주실</strong>
                      <p>대규모 밴드 세션 가능</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">🤖</span>
                    <div>
                      <strong>자동 믹싱</strong>
                      <p>AI가 도와주는 스마트 믹싱</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">📅</span>
                    <div>
                      <strong>합주실 예약</strong>
                      <p>정기 세션을 미리 계획</p>
                    </div>
                  </div>
                  <div className="feature-item">
                    <span className="feature-icon">👑</span>
                    <div>
                      <strong>팀 관리</strong>
                      <p>밴드 멤버와 프로젝트 관리</p>
                    </div>
                  </div>
                </>
              )}
            </div>
          </div>

          <div className="action-buttons">
            <Link to="/rooms" className="primary-action">
              합주실 만들기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>

          <div className="support-info">
            <h4>📞 도움이 필요하신가요?</h4>
            <p>
              결제나 서비스 이용에 문의사항이 있으시면 
              <a href="mailto:support@bandspace.com"> support@bandspace.com</a>으로 
              연락주세요.
            </p>
          </div>
        </div>
      </div>
    </div>
  )
}