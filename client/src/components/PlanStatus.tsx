import { Link } from 'react-router-dom'
import { usePremium } from '../contexts/usePremium'

interface PlanStatusProps {
  showUpgrade?: boolean
  compact?: boolean
}

export function PlanStatus({ showUpgrade = true, compact = false }: PlanStatusProps) {
  const { userPlan, planLimits } = usePremium()

  const planInfo = {
    free: {
      name: '무료 플랜',
      // 배지 글자는 어두운 색(index.css) — #6b7280은 4.22:1이라 AA 미달 → gray-400 (P3-12d)
      color: 'var(--gray-400)',
      badge: '체험용',
    },
    standard: {
      name: 'Standard',
      color: 'var(--indigo-400)',
      badge: '추천',
    },
    pro: {
      name: 'Pro',
      color: 'var(--pink-400)',
      badge: '프리미엄',
    },
  }

  const currentPlan = planInfo[userPlan]

  if (compact) {
    return (
      <Link to="/pricing" className="plan-status-compact">
        <span
          className="plan-badge-compact"
          style={{ backgroundColor: currentPlan.color }}
        >
          {currentPlan.name}
        </span>
      </Link>
    )
  }

  return (
    <div className="plan-status-card">
      <div className="plan-status-header">
        <div className="plan-info">
          <span 
            className="plan-badge"
            style={{ backgroundColor: currentPlan.color }}
          >
            {currentPlan.badge}
          </span>
          <h3>{currentPlan.name}</h3>
        </div>
        {showUpgrade && userPlan !== 'pro' && (
          <Link to="/pricing" className="upgrade-link">
            업그레이드
          </Link>
        )}
      </div>

      {/* 실제 규칙 (P3-12a): 인원·비공개 방은 요금제와 무관 */}
      <div className="plan-limits">
        <div className="limit-item">
          <span className="limit-label">동시 연주</span>
          <span className="limit-value">최대 {planLimits.maxPerformers}명</span>
        </div>

        <div className="limit-item">
          <span className="limit-label">합주실 정원 (관람 포함)</span>
          <span className="limit-value">최대 {planLimits.maxRoomCapacity}명</span>
        </div>

        <div className="limit-item">
          <span className="limit-label">비공개 방</span>
          <span className="limit-value">{planLimits.canCreatePrivateRooms ? '가능' : '불가'}</span>
        </div>
      </div>
    </div>
  )
}