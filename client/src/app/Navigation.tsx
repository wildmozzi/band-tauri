import { Suspense, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { LATENCY_MEASURE_SECTION_ID, OPEN_LATENCY_MEASURE_EVENT } from '../native/loopbackOffsetStore'
import { OPEN_AUDIO_SETTINGS_EVENT } from '../native/deviceFallback'
import { Link, useNavigate } from 'react-router-dom'
import { profilePhotoOf, useAuth } from '../features/auth'
import { AudioSettings } from '../features/settings'
import { PlanStatus } from '../features/payment'
import { SERVICE_NAME } from '../shared/lib/brand'
import './Navigation.css'

export function Navigation() {
  const { user, signOut } = useAuth()
  const navigate = useNavigate()
  const [showAudioSettings, setShowAudioSettings] = useState(false)
  const navRef = useRef<HTMLElement | null>(null)
  // 서버가 기록한 사진 URL만 (서버만 쓰는 app_metadata, P2-12)
  const profilePhoto = profilePhotoOf(user)

  // 내비게이션 높이를 CSS 변수로 알린다 — 합주실이 남은 화면 높이에 맞춰 채팅 입력창을 첫 화면에 둔다 (P3-12 ⑨)
  useLayoutEffect(() => {
    const el = navRef.current
    if (!el) return
    const apply = () => document.documentElement.style.setProperty('--app-nav-height', `${el.offsetHeight}px`)
    apply()
    if (typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(apply)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  // 메트로놈 패널의 "지연 측정 열기"(P2-3c): 합주실을 떠나지 않게 오디오 설정 모달을 열고 지연 측정 절로 스크롤한다
  useEffect(() => {
    const open = () => {
      setShowAudioSettings(true)
      // 모달은 지연 로딩이라 처음 열 때는 그 절이 몇 프레임 뒤에 생긴다 — 생길 때까지(최대 약 2초) 기다렸다가 스크롤
      let tries = 120
      const scroll = () => {
        const el = document.getElementById(LATENCY_MEASURE_SECTION_ID)
        if (el) el.scrollIntoView({ block: 'start' })
        else if (--tries > 0) requestAnimationFrame(scroll)
      }
      requestAnimationFrame(scroll)
    }
    window.addEventListener(OPEN_LATENCY_MEASURE_EVENT, open)
    // 합주실 "저장된 장치를 찾을 수 없어 기본 장치로 시작" 알림의 "오디오 설정"(P2-27): 모달만 연다
    const openSettings = () => setShowAudioSettings(true)
    window.addEventListener(OPEN_AUDIO_SETTINGS_EVENT, openSettings)
    return () => {
      window.removeEventListener(OPEN_LATENCY_MEASURE_EVENT, open)
      window.removeEventListener(OPEN_AUDIO_SETTINGS_EVENT, openSettings)
    }
  }, [])

  const handleSignOut = async () => {
    try {
      await signOut()
      navigate('/')
    } catch (error) {
      console.error('Sign out error:', error)
    }
  }

  return (
    <>
      {/* 오디오 설정 모달 - nav 바깥에 렌더링 */}
      {showAudioSettings && (
        <Suspense fallback={null}>
          <AudioSettings isModal onClose={() => setShowAudioSettings(false)} />
        </Suspense>
      )}

      <nav className="top-nav" ref={navRef}>
        <div className="brand">
        <Link to="/">
          <span>{SERVICE_NAME}</span>
          <small>Web</small>
        </Link>
      </div>
      <div className="nav-links">
        <Link to="/">홈</Link>
        <Link to="/rooms">합주실</Link>
        <Link to="/recording">녹음</Link>
        <Link to="/mix">Mix Lab</Link>
        <Link to="/community">커뮤니티</Link>
        <Link to="/karaoke">노래방(데모)</Link>
      </div>
      <div className="nav-actions">
        <button
          onClick={() => setShowAudioSettings(true)}
          className="nav-settings"
          title="오디오 설정"
          aria-label="오디오 설정"
        >
          🎛️
        </button>
        {user ? (
          <div className="user-menu">
            <PlanStatus compact />
            <Link to="/profile" className="profile-icon-link" aria-label="프로필">
              {profilePhoto ? (
                <img
                  src={profilePhoto}
                  alt="Profile"
                  className="nav-profile-photo"
                />
              ) : (
                <div className="nav-profile-default">
                  <span>👤</span>
                </div>
              )}
            </Link>
            <span className="user-email">{user.email}</span>
            <button onClick={handleSignOut} className="nav-cta">
              로그아웃
            </button>
          </div>
        ) : (
          <Link to="/auth" className="nav-cta">
            로그인
          </Link>
        )}
        </div>
      </nav>
    </>
  )
}
