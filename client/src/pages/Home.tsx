import { Link } from 'react-router-dom'
import { PlanStatus } from '../components/PlanStatus'
import { AdBanner } from '../components/AdBanner'
import { useAuth } from '../contexts/AuthContext'

type FeaturePillar = {
  title: string
  description: string
  details: string
  badge: string
}

const FEATURE_PILLARS: FeaturePillar[] = [
  {
    title: '초저지연 합주',
    description: '다중 엣지 시그널링 + UDP 가속으로 20ms 이하 지연을 유지합니다.',
    details: '서울 · 도쿄 · LA 리전에 동시 배포된 네트워크 스택',
    badge: 'Live Rooms',
  },
  {
    title: '개인 레코딩 스튜디오',
    description: '브라우저에서 바로 아이디어를 캡처하고 스템을 정리하세요.',
    details: 'Web Audio 기반 멀티트랙, WAV/MP3 내보내기 로드맵',
    badge: 'Creator',
  },
  {
    title: '커뮤니티 피드',
    description: '합주 파트너를 찾고 프로젝트를 공유하며 루프를 교환합니다.',
    details: '멤버십 · 오픈 세션 · 비공개 팀 기능',
    badge: 'Community',
  },
  {
    title: '믹스 랩',
    description: '레벨/팬/FX를 조정해 실시간으로 믹스를 스케치하세요.',
    details: 'AI 기반 밸런스 제안과 매치드 리퍼런스 계획',
    badge: 'Mix Lab',
  },
]

export function Home() {
  const { user } = useAuth()

  return (
    <div className="home-page">
      <header className="hero">
        <p className="eyebrow">SYNCROOM INSPIRED · WEB DEMO</p>
        <h1>
          Yamaha Syncroom을 재해석한
          <br />
          초저지연 온라인 합주실
        </h1>
        <p className="lead">
          BandSpace는 웹을 시작으로 데스크톱 · 모바일까지 확장되는 통합 합주 플랫폼입니다. 개인 녹음부터 커뮤니티,
          믹싱 실험까지 하나의 타임라인에서 이어집니다.
        </p>
        <div className="hero-actions">
          <Link to="/rooms" className="primary-cta">
            합주실 열기
          </Link>
          <a href="#roadmap" className="ghost-cta">
            제품 로드맵
          </a>
        </div>
        <div className="hero-stats">
          <div>
            <strong>18 ms</strong>
            <span>평균 왕복 지연</span>
          </div>
          <div>
            <strong>3 지역</strong>
            <span>KR · JP · US 엣지</span>
          </div>
          <div>
            <strong>256 kbps</strong>
            <span>양방향 오디오</span>
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

        <section id="roadmap" className="panel roadmap-section">
          <div className="section-head">
            <div>
              <p className="eyebrow">Roadmap</p>
              <h2>웹 → 데스크톱 → 모바일까지 확장</h2>
            </div>
          </div>
          <div className="roadmap-grid">
            <article className="roadmap-card">
              <h3>Web Beta</h3>
              <p>합주실 · 개인 녹음 · 커뮤니티 피드를 하나의 앱으로 통합합니다.</p>
              <ul>
                <li>WebRTC 기반 저지연 오디오</li>
                <li>세션 프리셋 / 샘플 공유</li>
                <li>믹스 랩 & 루프 스토리지</li>
              </ul>
            </article>
            <article className="roadmap-card">
              <h3>Desktop Studio</h3>
              <p>Electron + 네이티브 오디오 드라이버 연동으로 96kHz까지 확장.</p>
              <ul>
                <li>ASIO / Core Audio Bridge</li>
                <li>오프라인 프로젝트 · 로컬 캐시</li>
                <li>멀티 모니터 믹스 콘솔</li>
              </ul>
            </article>
            <article className="roadmap-card">
              <h3>Mobile Companion</h3>
              <p>외부 인터페이스 없이도 아이디어 캡처 및 커뮤니티 업데이트 지원.</p>
              <ul>
                <li>iOS / Android 네이티브 모듈</li>
                <li>Low-power Monitoring</li>
                <li>세션 알림 · DM</li>
              </ul>
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
