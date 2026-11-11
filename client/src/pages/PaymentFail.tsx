import { useEffect } from 'react'
import { Link, useSearchParams } from 'react-router-dom'

export function PaymentFail() {
  const [searchParams] = useSearchParams()

  const code = searchParams.get('code')
  const message = searchParams.get('message')
  const orderId = searchParams.get('orderId')

  // 토스 failUrl이 붙이는 정보가 하나도 없으면 직접 진입이다 (P3-12d)
  const hasFailInfo = Boolean(code || message || orderId)

  useEffect(() => {
    // 결제 실패 기록 — 사용자 취소·카드 거절 같은 예상된 결과라 error가 아닌 warn (P3-12d)
    if (hasFailInfo) console.warn('Payment failed:', { code, message, orderId })
  }, [hasFailInfo, code, message, orderId])

  const getErrorMessage = () => {
    switch (code) {
      case 'PAY_PROCESS_CANCELED':
        return '사용자가 결제를 취소했습니다.'
      case 'PAY_PROCESS_ABORTED':
        return '결제 진행 중 오류가 발생했습니다.'
      case 'REJECT_CARD_COMPANY':
        return '카드사에서 결제를 거절했습니다.'
      case 'INVALID_CARD_COMPANY':
        return '유효하지 않은 카드입니다.'
      case 'NOT_SUPPORTED_INSTALLMENT':
        return '지원하지 않는 할부 개월수입니다.'
      case 'EXCEED_MAX_DAILY_PAYMENT_COUNT':
        return '일일 결제 한도를 초과했습니다.'
      case 'NOT_AVAILABLE_BANK':
        return '은행 서비스 시간이 아닙니다.'
      case 'EXCEED_MAX_ONE_DAY_WITHDRAW_AMOUNT':
        return '일일 출금 한도를 초과했습니다.'
      default:
        return message || '결제 처리 중 오류가 발생했습니다.'
    }
  }

  if (!hasFailInfo) {
    return (
      <div className="payment-fail-page">
        <div className="fail-container">
          <div className="fail-content">
            <h1>표시할 결제 실패 정보가 없습니다</h1>
            <p className="fail-message">
              이 화면은 결제가 실패하거나 취소됐을 때 자동으로 열립니다. 요금제를 다시 보려면 아래 버튼을 눌러 주세요.
            </p>
            <div className="action-buttons">
              <Link to="/pricing" className="primary-action">
                요금제 보기
              </Link>
              <Link to="/" className="secondary-action">
                홈으로 가기
              </Link>
            </div>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="payment-fail-page">
      <div className="fail-container">
        <div className="fail-icon">
          <div className="error-mark">❌</div>
        </div>
        
        <div className="fail-content">
          <h1>결제에 실패했습니다</h1>
          <p className="fail-message">
            {getErrorMessage()}
          </p>
          
          {orderId && (
            <div className="order-info">
              <p className="order-id">주문번호: {orderId}</p>
            </div>
          )}

          <div className="fail-details">
            <h3>💡 해결 방법</h3>
            <ul>
              <li>카드 정보를 다시 확인해주세요</li>
              <li>카드 한도나 잔액을 확인해주세요</li>
              <li>다른 결제 방법을 시도해보세요</li>
              <li>문제가 지속되면 고객센터로 문의해주세요</li>
            </ul>
          </div>

          <div className="action-buttons">
            <Link to="/pricing" className="primary-action">
              다시 시도하기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>

          <div className="support-info">
            <h4>📞 도움이 필요하신가요?</h4>
            <p>
              결제 관련 문의는 
              <a href="mailto:support@bandspace.com"> support@bandspace.com</a>으로 
              연락주세요.
            </p>
            <p className="support-hours">
              고객센터 운영시간: 평일 09:00 - 18:00
            </p>
          </div>
        </div>
      </div>
    </div>
  )
}