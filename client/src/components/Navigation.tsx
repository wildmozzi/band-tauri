import { useLayoutEffect, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { AudioSettings } from './AudioSettings'
import { PlanStatus } from './PlanStatus'
import { SERVICE_NAME } from '../lib/brand'
import './Navigation.css'

export function Navigation() {
  const { user, signOut } = useAuth()
  const navigate = useNavigate()
  const [showAudioSettings, setShowAudioSettings] = useState(false)
  const navRef = useRef<HTMLElement | null>(null)

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
        <AudioSettings isModal onClose={() => setShowAudioSettings(false)} />
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
              {user.user_metadata?.profile_photo ? (
                <img
                  src={user.user_metadata.profile_photo}
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
