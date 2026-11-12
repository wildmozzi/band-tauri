import { Link } from 'react-router-dom'
import { PlanStatus } from '../payment/PlanStatus'
import { AdBanner } from './AdBanner'
import { useAuth } from '../auth/useAuth'
import { DesktopDownloadButton } from '../../shared/ui/DesktopDownloadButton'
import { SERVICE_NAME } from '../../shared/lib/brand'
import { MAX_PERFORMERS } from '../../native/engineApi'

type FeaturePillar = {
  title: string
  description: string
  details: string
  badge: string
}

// 사실만 적는다 (P3-12 ③): 측정되지 않은 지연 수치·리전·경쟁사명은 쓰지 않는다
const FEATURE_PILLARS: FeaturePillar[] = [
  {
    title: '데스크톱 앱 합주',
    description: `저지연 네이티브 오디오 엔진으로 최대 ${MAX_PERFORMERS}명이 함께 연주합니다.`,
    details: 'Windows 데스크톱 앱 · WASAPI 독점 모드 저지연 출력 · Opus 코덱 (ASIO 지원 예정)',
    badge: 'Live Rooms',
  },
  {
    title: '웹 관람',
    description: '설치 없이 브라우저에서 합주를 듣고 채팅으로 참여합니다.',
    details: '연주 참여는 데스크톱 앱에서',
    badge: 'Watch',
  },
  {
    title: '커뮤니티 피드',
    description: '합주 파트너를 찾고 프로젝트를 공유합니다.',
    details: '악기별 게시판 · 태그 · 좋아요 · 댓글',
    badge: 'Community',
  },
  {
    title: '녹음 · 믹스 랩',
    description: '브라우저에서 아이디어를 녹음하고 레벨/팬을 조정해 믹스를 스케치합니다.',
    details: '웹 녹음 · Mix Lab',
    badge: 'Creator',
  },
]

export function Home() {
  const { user } = useAuth()

  return (
    <div className="home-page">
      <header className="hero">
        <p className="eyebrow">{SERVICE_NAME}</p>
        <h1>온라인 합주실</h1>
        <p className="lead">
          연주는 저지연 오디오 엔진을 갖춘 데스크톱 앱에서 최대 {MAX_PERFORMERS}명이 함께, 관람과 채팅은 웹에서 설치 없이.
          커뮤니티와 녹음·믹스 도구도 함께 씁니다.
        </p>
        <div className="hero-actions">
          <Link to="/rooms" className="primary-cta">
            합주실 열기
          </Link>
          <DesktopDownloadButton className="ghost-cta" />
        </div>
        <div className="hero-stats">
          <div>
            <strong>최대 {MAX_PERFORMERS}명</strong>
            <span>동시 연주 (데스크톱 앱)</span>
          </div>
          <div>
            <strong>웹 관람</strong>
            <span>설치 없이 듣기 · 채팅</span>
          </div>
          <div>
            <strong>Windows</strong>
            <span>WASAPI 저지연(독점 모드) · ASIO 예정</span>
          </div>
        </div>
      </header>

      <main>
        <AdBanner position="top" />
        
        <section className="panel feature-grid" aria-label="핵심 기능">
          {FEATURE_PILLARS.map((feature) => (
            <article key={feature.title} className="feature-card">
              <span className="pill">{feature.badge}</span>
              <h3>{feature.title}</h3>
              <p>{feature.description}</p>
              <small>{feature.details}</small>
            </article>
          ))}
        </section>

        <section id="how-it-works" className="panel roadmap-section">
          <div className="section-head">
            <div>
              <p className="eyebrow">How it works</p>
              <h2>연주는 데스크톱, 관람은 웹</h2>
            </div>
          </div>
          <div className="roadmap-grid">
            <article className="roadmap-card">
              <h3>1. 합주실 찾기</h3>
              <p>공개 합주실을 둘러보거나 방을 만들어 초대 링크를 공유합니다.</p>
            </article>
            <article className="roadmap-card">
              <h3>2. 데스크톱 앱으로 연주</h3>
              <p>데스크톱 앱에서 오디오 장치를 고르고 연주에 참여합니다. 연주자는 방마다 최대 {MAX_PERFORMERS}명입니다.</p>
            </article>
            <article className="roadmap-card">
              <h3>3. 웹에서 관람</h3>
              <p>앱이 없어도 웹에서 합주를 듣고 채팅할 수 있습니다.</p>
            </article>
          </div>
        </section>

        {/* 사용자 플랜 상태 */}
        {user && (
          <section className="panel user-plan-section">
            <div className="section-head">
              <h2>내 플랜 상태</h2>
            </div>
            <PlanStatus />
          </section>
        )}

        <AdBanner position="bottom" />
      </main>
    </div>
  )
}
