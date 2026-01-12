interface AdBannerProps {
  position?: 'top' | 'bottom' | 'sidebar'
  compact?: boolean
}

export function AdBanner(_props: AdBannerProps) {
  // 광고 기능은 현재 비활성화
  return null

  /* 향후 광고 기능 활성화 시 사용
  const adContent = {
    top: {
      title: '🎵 더 나은 합주 경험을 원하시나요?',
      description: 'Standard 플랜으로 업그레이드하고 광고 없이 더 많은 기능을 이용하세요.',
      cta: 'Standard 시작하기'
    },
    bottom: {
      title: '✨ 프리미엄 기능을 체험해보세요',
      description: '클라우드 저장, Mix Lab, 비공개 방 등 다양한 기능이 기다리고 있습니다.',
      cta: '플랜 보기'
    },
    sidebar: {
      title: '🚀 업그레이드',
      description: '더 많은 기능과 광고 제거',
      cta: '업그레이드'
    }
  }

  const content = adContent[position]

  if (compact) {
    return (
      <div className="ad-banner compact">
        <div className="ad-content">
          <span className="ad-label">광고</span>
          <p>{content.description}</p>
          <Link to="/pricing" className="ad-cta-compact">
            {content.cta}
          </Link>
        </div>
      </div>
    )
  }

  return (
    <div className={`ad-banner ${position}`}>
      <div className="ad-content">
        <span className="ad-label">광고</span>
        <div className="ad-text">
          <h4>{content.title}</h4>
          <p>{content.description}</p>
        </div>
        <Link to="/pricing" className="ad-cta">
          {content.cta}
        </Link>
      </div>
    </div>
  )
  */
}