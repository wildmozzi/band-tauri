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

      <div className="plan-limits">
        <div className="limit-item">
          <span className="limit-label">합주실 최대 인원</span>
          <span className="limit-value">{planLimits.maxParticipants}명</span>
        </div>
        
        <div className="limit-item">
          <span className="limit-label">클라우드 저장</span>
          <span className="limit-value">
            {planLimits.hasCloudStorage 
              ? planLimits.cloudStorageDays 
                ? `${planLimits.cloudStorageDays}일`
                : '무제한'
              : '불가'
            }
          </span>
        </div>

        <div className="limit-item">
          <span className="limit-label">Mix Lab</span>
          <span className="limit-value">
            {planLimits.hasMixLab 
              ? planLimits.mixLabPresets 
                ? `프리셋 ${planLimits.mixLabPresets}개`
                : '무제한'
              : '불가'
            }
          </span>
        </div>

        {planLimits.showAds && (
          <div className="limit-item warning">
            <span className="limit-label">광고</span>
            <span className="limit-value">표시됨</span>
          </div>
        )}
      </div>
    </div>
  )
}