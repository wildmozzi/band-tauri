import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import { AudioSettings } from './AudioSettings'
import { PlanStatus } from './PlanStatus'
import './Navigation.css'

export function Navigation() {
  const { user, signOut } = useAuth()
  const navigate = useNavigate()
  const [showAudioSettings, setShowAudioSettings] = useState(false)

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

      <nav className="top-nav">
        <div className="brand">
        <Link to="/">
          <span>BandSpace</span>
          <small>Web Studio</small>
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
        >
          🎛️
        </button>
        {user ? (
          <div className="user-menu">
            <PlanStatus compact />
            <Link to="/profile" className="profile-icon-link">
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
