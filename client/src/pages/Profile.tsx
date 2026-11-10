import { useState, useRef, useEffect } from 'react'
import { useAuth } from '../contexts/useAuth'
import { usePremium } from '../contexts/usePremium'
import { useNavigate } from 'react-router-dom'
import { isSupabaseConfigured } from '../lib/supabaseClient'
import {
  useProfileNicknameQuery,
  useSaveProfileMutation,
  useUpdateUserMetadataMutation,
} from '../features/profile/queries'
import './Profile.css'

type ProfileTab = 'info' | 'follow' | 'plan' | 'theme' | 'support'

export function Profile() {
  const { user } = useAuth()
  const { userPlan } = usePremium()
  const navigate = useNavigate()
  const fileInputRef = useRef<HTMLInputElement>(null)

  const [activeTab, setActiveTab] = useState<ProfileTab>('info')
  const [isEditing, setIsEditing] = useState(false)
  const saveProfile = useSaveProfileMutation()
  const updateMetadata = useUpdateUserMetadataMutation()
  const isSaving = saveProfile.isPending
  const [isHoveringPhoto, setIsHoveringPhoto] = useState(false)

  // 프로필 정보 상태
  const [nickname, setNickname] = useState('')
  const [originalNickname, setOriginalNickname] = useState('')
  const [bio, setBio] = useState(user?.user_metadata?.bio || '')
  const [snsLinks, setSnsLinks] = useState({
    instagram: user?.user_metadata?.instagram || '',
    youtube: user?.user_metadata?.youtube || '',
    twitter: user?.user_metadata?.twitter || '',
  })
  const [profilePhoto, setProfilePhoto] = useState(user?.user_metadata?.profile_photo || '')

  // 닉네임: user_metadata 우선, 없으면 profiles 테이블에서 가져온다
  const metadataNickname: string | undefined = user?.user_metadata?.nickname || undefined
  const { data: profileNickname } = useProfileNicknameQuery(user?.id, !metadataNickname)
  const { mutate: syncMetadata } = updateMetadata

  useEffect(() => {
    if (!user?.id || !metadataNickname) return
    setNickname(metadataNickname)
    setOriginalNickname(metadataNickname)
  }, [user?.id, metadataNickname])

  useEffect(() => {
    if (metadataNickname || !profileNickname) return
    setNickname(profileNickname)
    setOriginalNickname(profileNickname)
    // user_metadata도 업데이트
    syncMetadata({ nickname: profileNickname })
  }, [metadataNickname, profileNickname, syncMetadata])

  if (!user) {
    navigate('/auth')
    return null
  }

  const handlePhotoClick = () => {
    fileInputRef.current?.click()
  }

  const handlePhotoChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return

    // 이미지를 base64로 변환
    const reader = new FileReader()
    reader.onload = async (event) => {
      const base64Image = event.target?.result as string
      setProfilePhoto(base64Image)

      // Supabase에 저장
      if (isSupabaseConfigured()) {
        try {
          await updateMetadata.mutateAsync({ profile_photo: base64Image })
        } catch (err) {
          console.error('Failed to update profile photo:', err)
          alert('프로필 사진 업데이트에 실패했습니다.')
        }
      }
    }
    reader.readAsDataURL(file)
  }

  const handleSave = async () => {
    if (!isSupabaseConfigured() || !user?.id) {
      alert('데이터베이스에 연결할 수 없습니다.')
      return
    }

    try {
      // auth user_metadata + profiles 테이블
      await saveProfile.mutateAsync({
        userId: user.id,
        form: {
          nickname,
          bio,
          instagram: snsLinks.instagram,
          youtube: snsLinks.youtube,
          twitter: snsLinks.twitter,
        },
      })

      setOriginalNickname(nickname)
      setIsEditing(false)
      alert('프로필이 업데이트되었습니다.')
    } catch (err) {
      console.error('Failed to update profile:', err)
      alert('프로필 업데이트에 실패했습니다.')
    }
  }

  const handleCancel = () => {
    setNickname(originalNickname)
    setBio(user?.user_metadata?.bio || '')
    setSnsLinks({
      instagram: user?.user_metadata?.instagram || '',
      youtube: user?.user_metadata?.youtube || '',
      twitter: user?.user_metadata?.twitter || '',
    })
    setIsEditing(false)
  }

  return (
    <div className="profile-page">
      <div className="profile-container">
        {/* 좌측 사이드바 */}
        <aside className="profile-sidebar">
          <h2>프로필 설정</h2>
          <nav className="profile-nav">
            <button
              className={`profile-nav-item ${activeTab === 'info' ? 'active' : ''}`}
              onClick={() => setActiveTab('info')}
            >
              <span className="nav-icon">👤</span>
              내 정보
            </button>
            <button
              className={`profile-nav-item ${activeTab === 'follow' ? 'active' : ''}`}
              onClick={() => setActiveTab('follow')}
            >
              <span className="nav-icon">👥</span>
              팔로우/팔로워
              <span className="coming-soon">준비중</span>
            </button>
            <button
              className={`profile-nav-item ${activeTab === 'plan' ? 'active' : ''}`}
              onClick={() => setActiveTab('plan')}
            >
              <span className="nav-icon">💎</span>
              요금제
            </button>
            <button
              className={`profile-nav-item ${activeTab === 'theme' ? 'active' : ''}`}
              onClick={() => setActiveTab('theme')}
            >
              <span className="nav-icon">🎨</span>
              테마 변경
              <span className="coming-soon">준비중</span>
            </button>
            <button
              className={`profile-nav-item ${activeTab === 'support' ? 'active' : ''}`}
              onClick={() => setActiveTab('support')}
            >
              <span className="nav-icon">💬</span>
              문의하기/고객센터
              <span className="coming-soon">준비중</span>
            </button>
          </nav>
        </aside>

        {/* 메인 콘텐츠 */}
        <main className="profile-main">
          {activeTab === 'info' && (
            <div className="profile-info-section">
              <div className="profile-header">
                <div
                  className="profile-photo-wrapper"
                  onMouseEnter={() => setIsHoveringPhoto(true)}
                  onMouseLeave={() => setIsHoveringPhoto(false)}
                  onClick={handlePhotoClick}
                >
                  <div className="profile-photo">
                    {profilePhoto ? (
                      <img src={profilePhoto} alt="Profile" />
                    ) : (
                      <div className="default-avatar">
                        <span>👤</span>
                      </div>
                    )}
                  </div>
                  {isHoveringPhoto && (
                    <div className="photo-edit-overlay">
                      <span className="edit-icon">✏️</span>
                    </div>
                  )}
                  <input
                    ref={fileInputRef}
                    type="file"
                    accept="image/*"
                    onChange={handlePhotoChange}
                    style={{ display: 'none' }}
                  />
                </div>

                <div className="profile-header-info">
                  <h1>{nickname || user.email}</h1>
                  <p className="profile-email">{user.email}</p>
                </div>
              </div>

              <div className="profile-details">
                <div className="detail-section">
                  <h3>기본 정보</h3>

                  <div className="detail-item">
                    <label>이메일 (아이디)</label>
                    <input type="text" value={user.email || ''} disabled />
                  </div>

                  <div className="detail-item">
                    <label>닉네임</label>
                    <input
                      type="text"
                      value={nickname}
                      onChange={(e) => setNickname(e.target.value)}
                      disabled={!isEditing}
                      placeholder="닉네임을 입력하세요"
                    />
                  </div>

                  <div className="detail-item">
                    <label>자기소개</label>
                    <textarea
                      value={bio}
                      onChange={(e) => setBio(e.target.value)}
                      disabled={!isEditing}
                      placeholder="자신을 소개해주세요"
                      rows={4}
                      maxLength={200}
                    />
                    {isEditing && (
                      <small className="char-count">{bio.length}/200</small>
                    )}
                  </div>
                </div>

                <div className="detail-section">
                  <h3>SNS 연동</h3>

                  <div className="detail-item">
                    <label>
                      <span className="sns-icon">📷</span> Instagram
                    </label>
                    <input
                      type="text"
                      value={snsLinks.instagram}
                      onChange={(e) => setSnsLinks(prev => ({ ...prev, instagram: e.target.value }))}
                      disabled={!isEditing}
                      placeholder="Instagram 사용자명"
                    />
                  </div>

                  <div className="detail-item">
                    <label>
                      <span className="sns-icon">▶️</span> YouTube
                    </label>
                    <input
                      type="text"
                      value={snsLinks.youtube}
                      onChange={(e) => setSnsLinks(prev => ({ ...prev, youtube: e.target.value }))}
                      disabled={!isEditing}
                      placeholder="YouTube 채널 URL"
                    />
                  </div>

                  <div className="detail-item">
                    <label>
                      <span className="sns-icon">🐦</span> Twitter
                    </label>
                    <input
                      type="text"
                      value={snsLinks.twitter}
                      onChange={(e) => setSnsLinks(prev => ({ ...prev, twitter: e.target.value }))}
                      disabled={!isEditing}
                      placeholder="Twitter 사용자명"
                    />
                  </div>
                </div>

                <div className="detail-actions">
                  {!isEditing ? (
                    <button className="btn-primary" onClick={() => setIsEditing(true)}>
                      수정하기
                    </button>
                  ) : (
                    <>
                      <button className="btn-secondary" onClick={handleCancel} disabled={isSaving}>
                        취소
                      </button>
                      <button className="btn-primary" onClick={handleSave} disabled={isSaving}>
                        {isSaving ? '저장 중...' : '저장'}
                      </button>
                    </>
                  )}
                </div>
              </div>
            </div>
          )}

          {activeTab === 'follow' && (
            <div className="coming-soon-section">
              <div className="coming-soon-icon">👥</div>
              <h2>팔로우/팔로워</h2>
              <p>팔로우 기능은 곧 출시될 예정입니다.</p>
            </div>
          )}

          {activeTab === 'plan' && (
            <div className="plan-section">
              <h2>요금제 관리</h2>

              <div className="current-plan-card">
                <div className="plan-status">
                  <span className="plan-label">현재 요금제</span>
                  <span className={`plan-badge ${userPlan}`}>
                    {userPlan === 'free' && '무료 플랜'}
                    {userPlan === 'standard' && 'Standard 플랜'}
                    {userPlan === 'pro' && 'Pro 플랜'}
                  </span>
                </div>

                <div className="plan-details">
                  {userPlan === 'free' && (
                    <>
                      <p className="plan-description">기본 기능을 무료로 이용 중입니다.</p>
                      <ul className="plan-features-list">
                        <li>합주실 생성 (최대 4명)</li>
                        <li>녹음 기능 & 로컬 저장</li>
                        <li>기본 오디오 품질</li>
                      </ul>
                    </>
                  )}
                  {userPlan === 'standard' && (
                    <>
                      <p className="plan-description">Standard 플랜을 이용 중입니다.</p>
                      <ul className="plan-features-list">
                        <li>합주실 생성 (최대 6명)</li>
                        <li>비공개 방 생성</li>
                        <li>클라우드 저장 (30일)</li>
                        <li>Mix Lab 기본 기능</li>
                      </ul>
                    </>
                  )}
                  {userPlan === 'pro' && (
                    <>
                      <p className="plan-description">Pro 플랜을 이용 중입니다.</p>
                      <ul className="plan-features-list">
                        <li>합주실 생성 (최대 8명)</li>
                        <li>클라우드 저장 무제한</li>
                        <li>자동 믹싱 & 고급 Mix Lab</li>
                        <li>팀/밴드 관리 기능</li>
                      </ul>
                    </>
                  )}
                </div>

                <div className="plan-actions">
                  <button
                    className="btn-primary"
                    onClick={() => navigate('/pricing')}
                  >
                    {userPlan === 'free' ? '요금제 업그레이드' : '요금제 변경'}
                  </button>
                </div>
              </div>

              {userPlan !== 'free' && (
                <div className="subscription-info">
                  <h3>구독 정보</h3>
                  <div className="info-row">
                    <span className="info-label">결제 주기</span>
                    <span className="info-value">월간 구독</span>
                  </div>
                  <div className="info-row">
                    <span className="info-label">다음 결제일</span>
                    <span className="info-value">-</span>
                  </div>
                  <div className="info-row">
                    <span className="info-label">결제 수단</span>
                    <span className="info-value">등록된 결제 수단 없음</span>
                  </div>
                  <button
                    className="btn-secondary"
                    onClick={() => navigate('/pricing')}
                  >
                    결제 수단 관리
                  </button>
                </div>
              )}
            </div>
          )}

          {activeTab === 'theme' && (
            <div className="coming-soon-section">
              <div className="coming-soon-icon">🎨</div>
              <h2>테마 변경</h2>
              <p>다양한 테마 기능이 준비 중입니다.</p>
            </div>
          )}

          {activeTab === 'support' && (
            <div className="coming-soon-section">
              <div className="coming-soon-icon">💬</div>
              <h2>문의하기/고객센터</h2>
              <p>고객 지원 기능이 준비 중입니다.</p>
            </div>
          )}
        </main>
      </div>
    </div>
  )
}
