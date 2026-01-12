import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'

interface PremiumModalProps {
  isOpen: boolean
  onClose: () => void
  feature: string
  requiredPlan: 'standard' | 'pro'
}

export function PremiumModal({ isOpen, onClose, feature, requiredPlan }: PremiumModalProps) {
  const { user } = useAuth()
  const navigate = useNavigate()
  const [selectedPlan, setSelectedPlan] = useState<'standard' | 'pro'>(requiredPlan)

  if (!isOpen) return null

  const planDetails = {
    standard: {
      name: 'Standard',
      price: '₩2,900',
      period: '/월',
      features: [
        '합주실 생성 (최대 6명)',
        '비공개 방 생성',
        '클라우드 저장 (30일)',
        '녹음 파일 다운로드',
        'Mix Lab 기본 기능',
        '프로필 꾸미기'
      ]
    },
    pro: {
      name: 'Pro',
      price: '₩6,900',
      period: '/월',
      features: [
        '합주실 생성 (최대 8명)',
        '자동 믹싱 기능',
        '클라우드 저장 무제한',
        'Mix Lab 고급 기능',
        '합주실 예약',
        '팀/밴드 관리',
        '세션 히스토리'
      ]
    }
  }

  const handleUpgrade = () => {
    if (!user) {
      navigate('/auth', { state: { from: `/payment?plan=${selectedPlan}` } })
    } else {
      navigate(`/payment?plan=${selectedPlan}`)
    }
    onClose()
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="premium-modal" onClick={(e) => e.stopPropagation()}>
        <button className="modal-close" onClick={onClose}>×</button>
        
        <div className="modal-header">
          <div className="premium-icon">✨</div>
          <h2>프리미엄 기능입니다</h2>
          <p className="feature-description">
            <strong>"{feature}"</strong> 기능을 사용하려면 플랜 업그레이드가 필요합니다.
          </p>
        </div>

        <div className="modal-content">
          <div className="plan-selector">
            <div className="plan-tabs">
              <button 
                className={`plan-tab ${selectedPlan === 'standard' ? 'active' : ''}`}
                onClick={() => setSelectedPlan('standard')}
              >
                Standard
              </button>
              <button 
                className={`plan-tab ${selectedPlan === 'pro' ? 'active' : ''}`}
                onClick={() => setSelectedPlan('pro')}
              >
                Pro
              </button>
            </div>

            <div className="selected-plan-details">
              <div className="plan-info">
                <h3>{planDetails[selectedPlan].name} 플랜</h3>
                <div className="plan-price">
                  <span className="price">{planDetails[selectedPlan].price}</span>
                  <span className="period">{planDetails[selectedPlan].period}</span>
                </div>
              </div>

              <div className="plan-features">
                <h4>포함된 기능:</h4>
                <ul>
                  {planDetails[selectedPlan].features.map((feature, index) => (
                    <li key={index}>✅ {feature}</li>
                  ))}
                </ul>
              </div>
            </div>
          </div>

          <div className="modal-actions">
            <button className="upgrade-button" onClick={handleUpgrade}>
              {selectedPlan === 'standard' ? 'Standard로 업그레이드' : 'Pro로 업그레이드'}
            </button>
            <Link to="/pricing" className="view-all-plans" onClick={onClose}>
              모든 플랜 보기
            </Link>
          </div>

          <div className="modal-benefits">
            <div className="benefit-item">
              <span className="benefit-icon">🔄</span>
              <div>
                <strong>언제든 변경 가능</strong>
                <p>플랜 업그레이드/다운그레이드 자유</p>
              </div>
            </div>
            <div className="benefit-item">
              <span className="benefit-icon">💾</span>
              <div>
                <strong>데이터 보장</strong>
                <p>기존 녹음 파일과 설정 유지</p>
              </div>
            </div>
            <div className="benefit-item">
              <span className="benefit-icon">🔒</span>
              <div>
                <strong>7일 환불 보장</strong>
                <p>만족하지 않으면 전액 환불</p>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}