import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'
import { Modal } from '../../shared/ui'
import { PLANNED_NOTICE, plannedFeaturesFor } from './planFeatures'

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
    },
    pro: {
      name: 'Pro',
      price: '₩6,900',
      period: '/월',
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
    // 머리글 아이콘(✨)은 CSS(.premium-modal > .modal-header::before)로 그린다 — 제목(대화상자 이름)에 섞이지 않게
    <Modal
      className="modal-overlay"
      title="프리미엄 기능입니다"
      description={
        <>
          <strong>"{feature}"</strong> 기능을 사용하려면 플랜 업그레이드가 필요합니다.
        </>
      }
      onClose={onClose}
      classNames={{
        backdrop: 'premium-modal-backdrop',
        content: 'premium-modal',
        body: 'modal-content',
        close: 'modal-close',
        description: 'feature-description',
      }}
    >
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
            {/* P3-12a: 유료 전용 기능은 아직 없다 — 준비 중 기능을 판매 중인 혜택처럼 보이지 않는다 */}
            <h4>포함된 기능:</h4>
            <ul>
              <li>✅ 무료 플랜의 모든 기능</li>
            </ul>
            <h4>준비 중:</h4>
            <p>{PLANNED_NOTICE}</p>
            <ul>
              {plannedFeaturesFor(selectedPlan).map(f => (
                <li key={f}>⏳ {f}</li>
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
      </div>
    </Modal>
  )
}