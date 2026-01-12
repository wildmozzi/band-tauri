import { useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import { usePremium } from '../contexts/PremiumContext'

type PlanType = 'free' | 'standard' | 'pro'

interface PlanFeature {
  name: string
  free: boolean | string
  standard: boolean | string
  pro: boolean | string
}

const PLAN_FEATURES: PlanFeature[] = [
  { name: '합주실 참여자 수', free: '최대 4명', standard: '최대 6명', pro: '최대 8명' },
  { name: '비공개 방', free: false, standard: true, pro: true },
  { name: '클라우드 저장', free: false, standard: '30일', pro: '무제한' },
  { name: '파일 다운로드', free: '로컬만', standard: '7일 제한', pro: '무제한' },
  { name: 'Mix Lab', free: false, standard: '기본 (프리셋 2개)', pro: '고급 (무제한)' },
  { name: '자동 믹싱', free: false, standard: false, pro: true },
  { name: '합주실 예약', free: false, standard: false, pro: true },
  { name: '팀 관리', free: false, standard: false, pro: true },
  { name: '세션 히스토리', free: false, standard: false, pro: true },
  { name: '광고', free: '표시', standard: '없음', pro: '없음' },
]

export function Pricing() {
  const { user } = useAuth()
  const { userPlan, setUserPlan } = usePremium()
  const navigate = useNavigate()

  const handlePlanSelect = (plan: PlanType) => {
    if (!user) {
      // 로그인이 필요한 경우
      navigate('/auth', { state: { from: `/payment?plan=${plan}` } })
      return
    }

    // 무료 플랜은 바로 변경
    if (plan === 'free') {
      setUserPlan('free')
      alert('무료 플랜으로 변경되었습니다.')
      return
    }

    // 유료 플랜은 결제 페이지로 이동
    navigate(`/payment?plan=${plan}`)
  }

  const renderFeatureValue = (value: boolean | string) => {
    if (typeof value === 'boolean') {
      return value ? '✅' : '❌'
    }
    return value
  }

  return (
    <div className="pricing-page">
      <header className="pricing-hero">
        <p className="eyebrow">PRICING PLANS</p>
        <h1>합주의 새로운 경험을 시작하세요</h1>
        <p className="lead">
          무료 체험부터 전문 밴드용까지, 당신에게 맞는 플랜을 선택하세요.
        </p>
      </header>

      <section className="pricing-plans">

        {/* 무료 플랜 */}
        <div className={`plan-card free-plan ${userPlan === 'free' ? 'current-plan' : ''}`}>
          <div className="plan-header">
            <h3>무료 플랜</h3>
            <p className="plan-subtitle">체험용</p>
            <div className="plan-price">
              <span className="price">₩0</span>
              <span className="period">/월</span>
            </div>
          </div>
          <div className="plan-features">
            <div className="feature-highlight">
              <h4>✨ 주요 기능</h4>
              <ul>
                <li>합주실 생성 (최대 4명 참여)</li>
                <li>합주실 참여 무제한</li>
                <li>녹음 기능 & 로컬 저장</li>
                <li>기본 오디오 품질</li>
              </ul>
            </div>
            <div className="feature-limitations">
              <h4>⚠️ 제한사항</h4>
              <ul>
                <li>광고 표시</li>
                <li>클라우드 저장 불가</li>
                <li>파일 공유/편집 기능 없음</li>
              </ul>
            </div>
          </div>
          <button 
            className={`plan-button free-button ${userPlan === 'free' ? 'current-plan' : ''}`}
            onClick={() => handlePlanSelect('free')}
            disabled={userPlan === 'free'}
          >
            {userPlan === 'free' ? '현재 플랜' : '무료로 시작하기'}
          </button>
          <p className="plan-recommendation">
            💡 처음 온라인 합주를 시도하는 분께 추천
          </p>
        </div>

        {/* Standard 플랜 */}
        <div className={`plan-card standard-plan ${userPlan === 'standard' ? 'current-plan' : 'recommended'}`}>
          <div className="plan-badge">추천!</div>
          <div className="plan-header">
            <h3>Standard 플랜</h3>
            <p className="plan-subtitle">가성비 최적</p>
            <div className="plan-price">
              <span className="price">₩2,900</span>
              <span className="period">/월</span>
            </div>
          </div>
          <div className="plan-features">
            <div className="feature-highlight">
              <h4>✨ 주요 기능</h4>
              <ul>
                <li>합주실 생성 (최대 6명 참여)</li>
                <li>비공개 방 생성</li>
                <li>합주실 무제한 생성</li>
                <li>클라우드 저장 (최대 30일)</li>
                <li>녹음 파일 다운로드 (7일 제한)</li>
                <li>Mix Lab 기본 이퀄라이저</li>
                <li>프로필 꾸미기</li>
                <li>녹음 파일 링크 공유</li>
              </ul>
            </div>
          </div>
          <button 
            className={`plan-button standard-button ${userPlan === 'standard' ? 'current-plan' : ''}`}
            onClick={() => handlePlanSelect('standard')}
            disabled={userPlan === 'standard'}
          >
            {userPlan === 'standard' ? '현재 플랜' : 'Standard 시작하기'}
          </button>
          <p className="plan-recommendation">
            🎯 합주/녹음을 관리·다운로드·공유까지 확장
          </p>
        </div>

        {/* Pro 플랜 */}
        <div className={`plan-card pro-plan ${userPlan === 'pro' ? 'current-plan' : ''}`}>
          <div className="plan-header">
            <h3>Pro 플랜</h3>
            <p className="plan-subtitle">크루·밴드용</p>
            <div className="plan-price">
              <span className="price">₩6,900</span>
              <span className="period">/월</span>
            </div>
          </div>
          <div className="plan-features">
            <div className="feature-highlight">
              <h4>✨ 주요 기능</h4>
              <ul>
                <li>합주실 생성 (최대 8명 참여)</li>
                <li>합주 녹음 자동 믹싱</li>
                <li>클라우드 저장 무제한 + 자동 백업</li>
                <li>녹음 파일 다운로드 무제한</li>
                <li>Mix Lab 고급 (프리셋 저장 무제한)</li>
                <li>합주실 예약 기능</li>
                <li>팀/밴드 단위 관리</li>
                <li>세션 공유 권한 설정</li>
                <li>세션 히스토리 제공</li>
                <li>배지/인증 표시</li>
              </ul>
            </div>
          </div>
          <button 
            className={`plan-button pro-button ${userPlan === 'pro' ? 'current-plan' : ''}`}
            onClick={() => handlePlanSelect('pro')}
            disabled={userPlan === 'pro'}
          >
            {userPlan === 'pro' ? '현재 플랜' : 'Pro 시작하기'}
          </button>
          <p className="plan-recommendation">
            🚀 전문 밴드용 워크플로우 + 프로젝트 관리
          </p>
        </div>
      </section>

      {/* 비교표 */}
      <section className="comparison-table">
        <h2>플랜 비교표</h2>
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>기능</th>
                <th>무료</th>
                <th>Standard</th>
                <th>Pro</th>
              </tr>
            </thead>
            <tbody>
              {PLAN_FEATURES.map((feature, index) => (
                <tr key={index}>
                  <td className="feature-name">{feature.name}</td>
                  <td>{renderFeatureValue(feature.free)}</td>
                  <td>{renderFeatureValue(feature.standard)}</td>
                  <td>{renderFeatureValue(feature.pro)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      {/* FAQ */}
      <section className="faq-section">
        <h2>자주 묻는 질문</h2>
        <div className="faq-grid">
          <div className="faq-item">
            <h3>Q. 플랜을 중간에 변경할 수 있나요?</h3>
            <p>네, 언제든 업그레이드/다운그레이드 가능합니다. 기존 데이터는 유지됩니다.</p>
          </div>
          <div className="faq-item">
            <h3>Q. 무료 플랜에서 Pro로 바로 업그레이드 가능한가요?</h3>
            <p>네, 가능합니다. Standard를 거치지 않고 바로 Pro 플랜으로 변경할 수 있습니다.</p>
          </div>
          <div className="faq-item">
            <h3>Q. 녹음 파일이 삭제되면 복구할 수 있나요?</h3>
            <p>Pro 플랜의 경우 자동 백업으로 30일간 복구 가능합니다.</p>
          </div>
          <div className="faq-item">
            <h3>Q. 팀 계정으로 결제할 수 있나요?</h3>
            <p>Pro 플랜에서 팀 단위 결제 및 관리 기능을 제공합니다.</p>
          </div>
        </div>
      </section>

      {/* 시작하기 */}
      <section className="getting-started">
        <h2>🎵 시작하기</h2>
        <div className="steps-grid">
          <div className="step">
            <div className="step-number">1</div>
            <h3>무료로 체험</h3>
            <p>회원가입 후 바로 합주 시작<br />4명까지 함께 연주 가능</p>
          </div>
          <div className="step">
            <div className="step-number">2</div>
            <h3>필요에 맞는 플랜 선택</h3>
            <p>가끔 합주 → Standard<br />정기 밴드 활동 → Pro</p>
          </div>
          <div className="step">
            <div className="step-number">3</div>
            <h3>언제든 변경 가능</h3>
            <p>플랜 업그레이드/다운그레이드 자유<br />기존 데이터 유지 보장</p>
          </div>
        </div>
      </section>
    </div>
  )
}