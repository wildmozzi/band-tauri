import { useState } from 'react'
import './ProfileCard.css'

interface ProfileCardProps {
  userId: string
  nickname: string
  email?: string
  profilePhoto?: string
  bio?: string
  position?: { x: number; y: number }
  onClose: () => void
}

export function ProfileCard({
  userId,
  nickname,
  email,
  profilePhoto,
  bio,
  position,
  onClose
}: ProfileCardProps) {
  const [isFollowing, setIsFollowing] = useState(false)

  const handleFollow = () => {
    // TODO: 팔로우 로직 구현
    setIsFollowing(!isFollowing)
    console.log(isFollowing ? 'Unfollowing' : 'Following', userId)
  }

  const handleReport = () => {
    // TODO: 신고 로직 구현
    const confirmed = window.confirm(`${nickname} 사용자를 신고하시겠습니까?`)
    if (confirmed) {
      console.log('Reporting user:', userId)
      alert('신고가 접수되었습니다.')
      onClose()
    }
  }

  return (
    <>
      <div className="profile-card-backdrop" onClick={onClose} />
      <div
        className="profile-card"
        style={position ? { left: position.x, top: position.y } : undefined}
      >
        <button className="profile-card-close" onClick={onClose}>
          ×
        </button>

        <div className="profile-card-header">
          <div className="profile-card-photo">
            {profilePhoto ? (
              <img src={profilePhoto} alt={nickname} />
            ) : (
              <div className="default-avatar-small">
                <span>👤</span>
              </div>
            )}
          </div>
          <div className="profile-card-info">
            <h3>{nickname}</h3>
            {email && <p className="profile-card-email">{email}</p>}
          </div>
        </div>

        {bio && (
          <div className="profile-card-bio">
            <p>{bio}</p>
          </div>
        )}

        <div className="profile-card-actions">
          <button
            className={`follow-btn ${isFollowing ? 'following' : ''}`}
            onClick={handleFollow}
          >
            {isFollowing ? '팔로잉' : '팔로우'}
          </button>
          <button className="report-btn" onClick={handleReport}>
            신고
          </button>
        </div>
      </div>
    </>
  )
}
