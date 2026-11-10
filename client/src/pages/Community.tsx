import { useState, useMemo, useEffect, type ReactNode } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { usePostsQuery, useToggleLikeMutation, useTrendingTagsQuery } from '../features/community/queries'
import type { Post } from '../services/communityApi'
import './community/communityList.css'

// 사용 가능한 악기 목록
const INSTRUMENTS = [
  { id: 'all', name: '전체', icon: '🎵' },
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

function formatRelativeTime(iso: string) {
  const diff = Date.now() - new Date(iso).getTime()
  const minutes = Math.max(0, Math.round(diff / 60000))
  if (minutes < 1) return '방금 전'
  if (minutes < 60) return `${minutes}분 전`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}시간 전`
  const days = Math.round(hours / 24)
  return `${days}일 전`
}

/**
 * 글 상세 링크 (P3-12 ⑤). 카드 전체를 덮는 링크(::after)라 카드 어디를 눌러도 이동하고,
 * 실제 <a>라서 새 탭 열기·키보드 이동이 된다. 제목이 있으면 제목이, 없으면 본문 요약이 링크다
 * (hidden이면 링크 없이 글자만 — 카드당 링크 하나).
 */
function PostLink({ postId, hidden = false, children }: { postId: string; hidden?: boolean; children: ReactNode }) {
  if (hidden) return <>{children}</>
  return (
    <Link to={`/community/${postId}`} className="community-post-link">
      {children}
    </Link>
  )
}

export function Community() {
  const navigate = useNavigate()
  const { user } = useAuth()
  const [selectedInstrument, setSelectedInstrument] = useState('all')
  const [selectedTag, setSelectedTag] = useState<string | null>(null)

  // 글 목록·인기 태그 (features/community/queries)
  const postsQuery = usePostsQuery({ instrument: selectedInstrument, tag: selectedTag, userId: user?.id ?? null })
  const trendingQuery = useTrendingTagsQuery(5)
  const toggleLike = useToggleLikeMutation()
  const posts: Post[] = useMemo(() => postsQuery.data ?? [], [postsQuery.data])
  const loading = postsQuery.isPending
  const trendingTags = trendingQuery.data ?? []

  useEffect(() => {
    if (postsQuery.error) console.error('Failed to fetch posts:', postsQuery.error)
  }, [postsQuery.error])

  useEffect(() => {
    if (trendingQuery.error) console.error('Failed to fetch trending tags:', trendingQuery.error)
  }, [trendingQuery.error])

  // 인기 게시물 (좋아요 10개 이상, 좋아요 많은 순)
  const popularPosts = useMemo(() => {
    return [...posts]
      .filter(post => post.likes_count >= 10)
      .sort((a, b) => b.likes_count - a.likes_count)
      .slice(0, 3)
  }, [posts])

  const handleLike = async (postId: string, e?: React.MouseEvent) => {
    e?.stopPropagation()

    if (!user) {
      alert('좋아요를 누르려면 로그인이 필요합니다.')
      return
    }

    try {
      // 응답값으로 목록·상세 캐시를 갱신한다
      await toggleLike.mutateAsync({ postId, userId: user.id })
    } catch (error) {
      console.error('Failed to toggle like:', error)
    }
  }

  const handleTagClick = (tag: string) => {
    setSelectedTag(selectedTag === tag ? null : tag)
  }

  const cardStyle = {
    background: 'rgba(18, 22, 45, 0.8)',
    border: '1px solid rgba(255, 255, 255, 0.1)',
    borderRadius: '12px',
    padding: '20px 24px',
    marginBottom: '16px',
    cursor: 'pointer',
    transition: 'all 0.2s',
  }

  return (
    <div style={{ maxWidth: '1400px', margin: '0 auto', padding: '24px' }}>
      {/* 헤더 */}
      <div style={{ marginBottom: '32px' }}>
        <h1 style={{ fontSize: '32px', fontWeight: '700', marginBottom: '8px' }}>커뮤니티</h1>
        <p style={{ color: 'rgba(255, 255, 255, 0.6)', fontSize: '16px' }}>세션 파트너와 아이디어 공유</p>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 320px', gap: '24px' }}>
        <div>
          {/* 인기 게시물 섹션 */}
          <div style={{ marginBottom: '24px' }}>
            <h2 style={{ fontSize: '18px', fontWeight: '600', marginBottom: '12px' }}>🔥 인기 게시물</h2>
            {loading ? (
              <div style={{ padding: '20px', textAlign: 'center', color: 'rgba(255, 255, 255, 0.5)' }}>
                로딩 중...
              </div>
            ) : popularPosts.length === 0 ? (
              <div style={{ padding: '20px', textAlign: 'center', color: 'rgba(255, 255, 255, 0.5)' }}>
                아직 인기 게시물이 없습니다. (좋아요 10개 이상)
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
                {popularPosts.map((post) => (
                  <article
                    key={post.id}
                    style={cardStyle}
                    className="community-post-card"
                    onMouseOver={(e) => {
                      e.currentTarget.style.borderColor = 'rgba(141, 123, 255, 0.4)'
                      e.currentTarget.style.transform = 'translateY(-2px)'
                    }}
                    onMouseOut={(e) => {
                      e.currentTarget.style.borderColor = 'rgba(255, 255, 255, 0.1)'
                      e.currentTarget.style.transform = 'translateY(0)'
                    }}
                  >
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: '16px' }}>
                      <div style={{ flex: 1, display: 'flex', alignItems: 'center', gap: '12px' }}>
                        <span style={{ fontSize: '20px' }}>
                          {INSTRUMENTS.find(i => i.id === post.instrument)?.icon}
                        </span>
                        <div style={{ flex: 1 }}>
                          {post.title && (
                            <div style={{ fontSize: '16px', fontWeight: '600', marginBottom: '4px' }}>
                              <PostLink postId={post.id}>{post.title}</PostLink>
                            </div>
                          )}
                          <div style={{ fontSize: '14px', fontWeight: '400', marginBottom: '4px', color: 'rgba(255, 255, 255, 0.85)' }}>
                            <PostLink postId={post.id} hidden={!!post.title}>
                              {post.content.length > 60 ? `${post.content.slice(0, 60)}...` : post.content}
                            </PostLink>
                          </div>
                          <div style={{ fontSize: '13px', color: 'rgba(255, 255, 255, 0.5)' }}>
                            <span style={{ fontWeight: '600', color: 'rgba(255, 255, 255, 0.7)' }}>{post.author_name}</span>
                            {' · '}
                            {formatRelativeTime(post.created_at)}
                          </div>
                        </div>
                      </div>
                      <div
                        style={{ display: 'flex', alignItems: 'center', gap: '8px', flexShrink: 0 }}
                        className="community-post-actions"
                      >
                        {post.tags.length > 0 && post.tags.map((tag, idx) => (
                          <span
                            key={idx}
                            onClick={() => handleTagClick(tag)}
                            style={{
                              display: 'inline-block',
                              background: selectedTag === tag ? 'rgba(141, 123, 255, 0.3)' : 'rgba(141, 123, 255, 0.15)',
                              color: '#a89fff',
                              padding: '4px 10px',
                              borderRadius: '6px',
                              fontSize: '12px',
                              fontWeight: '500',
                              border: selectedTag === tag ? '1px solid rgba(141, 123, 255, 0.6)' : '1px solid rgba(141, 123, 255, 0.3)',
                              cursor: 'pointer',
                            }}
                          >
                            #{tag}
                          </span>
                        ))}
                        <span
                          style={{
                            padding: '4px 12px',
                            background: post.liked_by_user ? 'rgba(255, 122, 184, 0.25)' : 'rgba(255, 122, 184, 0.15)',
                            color: '#ff7ab8',
                            borderRadius: '6px',
                            fontSize: '13px',
                            fontWeight: '500',
                            cursor: 'pointer',
                          }}
                          onClick={(e) => handleLike(post.id, e)}
                        >
                          {post.liked_by_user ? '💖' : '❤️'} {post.likes_count}
                        </span>
                      </div>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>

          {/* 세션별 게시판 섹션 */}
          <div>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '16px' }}>
              <h2 style={{ fontSize: '20px', fontWeight: '600', margin: 0 }}>🎵 세션별 게시판</h2>
              <button
                onClick={() => navigate('/community/create')}
                style={{
                  background: 'linear-gradient(135deg, #8d7bff, #a89fff)',
                  border: 'none',
                  color: '#fff',
                  padding: '10px 20px',
                  borderRadius: '8px',
                  cursor: 'pointer',
                  fontSize: '14px',
                  fontWeight: '600',
                  transition: 'all 0.2s',
                }}
                onMouseOver={(e) => {
                  e.currentTarget.style.transform = 'translateY(-2px)'
                  e.currentTarget.style.boxShadow = '0 4px 12px rgba(141, 123, 255, 0.3)'
                }}
                onMouseOut={(e) => {
                  e.currentTarget.style.transform = 'translateY(0)'
                  e.currentTarget.style.boxShadow = 'none'
                }}
              >
                ✏️ 글 작성
              </button>
            </div>

            {/* 악기 탭 */}
            <div style={{ display: 'flex', gap: '8px', marginBottom: '20px', flexWrap: 'wrap' }}>
              {INSTRUMENTS.map((instrument) => (
                <button
                  key={instrument.id}
                  onClick={() => setSelectedInstrument(instrument.id)}
                  style={{
                    background: selectedInstrument === instrument.id
                      ? 'linear-gradient(135deg, #8d7bff, #a89fff)'
                      : 'rgba(18, 22, 45, 0.8)',
                    border: selectedInstrument === instrument.id
                      ? '1px solid rgba(141, 123, 255, 0.5)'
                      : '1px solid rgba(255, 255, 255, 0.1)',
                    color: '#fff',
                    padding: '8px 16px',
                    borderRadius: '8px',
                    cursor: 'pointer',
                    fontSize: '14px',
                    fontWeight: '500',
                    display: 'flex',
                    alignItems: 'center',
                    gap: '6px',
                  }}
                >
                  <span>{instrument.icon}</span>
                  <span>{instrument.name}</span>
                </button>
              ))}
            </div>

            {/* 활성 필터 표시 */}
            {selectedTag && (
              <div
                style={{
                  background: 'rgba(141, 123, 255, 0.1)',
                  border: '1px solid rgba(141, 123, 255, 0.3)',
                  borderRadius: '8px',
                  padding: '12px 16px',
                  marginBottom: '16px',
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                }}
              >
                <span style={{ fontSize: '14px' }}>
                  <span style={{ color: 'rgba(255, 255, 255, 0.6)' }}>필터:</span>{' '}
                  <span style={{ color: '#a89fff', fontWeight: '600' }}>#{selectedTag}</span>
                </span>
                <button
                  onClick={() => setSelectedTag(null)}
                  style={{
                    background: 'transparent',
                    border: 'none',
                    color: 'rgba(255, 255, 255, 0.7)',
                    cursor: 'pointer',
                    fontSize: '14px',
                    padding: '4px 8px',
                  }}
                >
                  ✕ 필터 해제
                </button>
              </div>
            )}

            {/* 필터링된 게시글 목록 */}
            <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
              {loading ? (
                <div style={{ padding: '40px', textAlign: 'center', color: 'rgba(255, 255, 255, 0.5)' }}>
                  로딩 중...
                </div>
              ) : posts.length === 0 ? (
                <div style={{ padding: '40px', textAlign: 'center', color: 'rgba(255, 255, 255, 0.5)' }}>
                  {selectedTag
                    ? `#${selectedTag} 태그가 포함된 게시물이 없습니다.`
                    : '아직 게시물이 없습니다. 첫 게시물을 작성해보세요!'}
                </div>
              ) : (
                posts.map((post) => (
                  <article
                    key={post.id}
                    style={cardStyle}
                    className="community-post-card"
                    onMouseOver={(e) => {
                      e.currentTarget.style.borderColor = 'rgba(141, 123, 255, 0.4)'
                      e.currentTarget.style.transform = 'translateY(-2px)'
                    }}
                    onMouseOut={(e) => {
                      e.currentTarget.style.borderColor = 'rgba(255, 255, 255, 0.1)'
                      e.currentTarget.style.transform = 'translateY(0)'
                    }}
                  >
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: '16px' }}>
                      <div style={{ flex: 1 }}>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '8px' }}>
                          <span style={{ fontSize: '18px' }}>
                            {INSTRUMENTS.find(i => i.id === post.instrument)?.icon}
                          </span>
                          <span style={{ fontWeight: '600', fontSize: '14px', color: 'rgba(255, 255, 255, 0.7)' }}>
                            {post.author_name}
                          </span>
                          <span style={{ fontSize: '13px', color: 'rgba(255, 255, 255, 0.4)' }}>·</span>
                          <span style={{ fontSize: '13px', color: 'rgba(255, 255, 255, 0.4)' }}>
                            {formatRelativeTime(post.created_at)}
                          </span>
                        </div>
                        {post.title && (
                          <div style={{ fontSize: '16px', fontWeight: '600', marginBottom: '6px' }}>
                            <PostLink postId={post.id}>{post.title}</PostLink>
                          </div>
                        )}
                        <p style={{ fontSize: '14px', margin: '0 0 8px 0', lineHeight: '1.5', color: 'rgba(255, 255, 255, 0.85)' }}>
                          <PostLink postId={post.id} hidden={!!post.title}>
                            {post.content.length > 100 ? `${post.content.slice(0, 100)}...` : post.content}
                          </PostLink>
                        </p>
                        <div
                          style={{ display: 'flex', alignItems: 'center', gap: '8px' }}
                          className="community-post-actions"
                        >
                          <span
                            style={{
                              padding: '4px 10px',
                              background: post.liked_by_user ? 'rgba(255, 122, 184, 0.25)' : 'rgba(255, 122, 184, 0.15)',
                              color: '#ff7ab8',
                              borderRadius: '6px',
                              fontSize: '12px',
                              fontWeight: '500',
                              cursor: 'pointer',
                            }}
                            onClick={(e) => handleLike(post.id, e)}
                            data-testid={`post-like-${post.id}`}
                          >
                            {post.liked_by_user ? '💖' : '❤️'} {post.likes_count}
                          </span>
                          {post.comments_count > 0 && (
                            <span style={{ fontSize: '12px', color: 'rgba(255, 255, 255, 0.5)' }}>
                              💬 {post.comments_count}
                            </span>
                          )}
                        </div>
                      </div>
                      {post.tags.length > 0 && (
                        <div
                          style={{ display: 'flex', gap: '6px', flexShrink: 0 }}
                          className="community-post-actions"
                        >
                          {post.tags.map((tag, idx) => (
                            <span
                              key={idx}
                              onClick={() => handleTagClick(tag)}
                              style={{
                                display: 'inline-block',
                                background: selectedTag === tag ? 'rgba(141, 123, 255, 0.3)' : 'rgba(141, 123, 255, 0.15)',
                                color: '#a89fff',
                                padding: '4px 10px',
                                borderRadius: '6px',
                                fontSize: '12px',
                                fontWeight: '500',
                                border: selectedTag === tag ? '1px solid rgba(141, 123, 255, 0.6)' : '1px solid rgba(141, 123, 255, 0.3)',
                                cursor: 'pointer',
                              }}
                            >
                              #{tag}
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  </article>
                ))
              )}
            </div>
          </div>
        </div>

        {/* 사이드바 */}
        <div>
          {/* 트렌딩 태그 */}
          <div
            style={{
              background: 'rgba(18, 22, 45, 0.8)',
              border: '1px solid rgba(255, 255, 255, 0.1)',
              borderRadius: '12px',
              padding: '20px',
              marginBottom: '16px',
            }}
          >
            <h3 style={{ fontSize: '16px', fontWeight: '600', marginBottom: '12px' }}>트렌딩 태그</h3>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: '8px' }}>
              {trendingTags.length === 0 ? (
                <p style={{ color: 'rgba(255, 255, 255, 0.5)', fontSize: '14px' }}>첫 게시물을 남겨주세요.</p>
              ) : (
                trendingTags.map(({ tag, count }) => (
                  <span
                    key={tag}
                    onClick={() => handleTagClick(tag)}
                    style={{
                      display: 'inline-block',
                      background: selectedTag === tag ? 'rgba(141, 123, 255, 0.3)' : 'rgba(141, 123, 255, 0.15)',
                      color: '#a89fff',
                      padding: '6px 12px',
                      borderRadius: '6px',
                      fontSize: '13px',
                      fontWeight: '500',
                      border: selectedTag === tag ? '1px solid rgba(141, 123, 255, 0.6)' : '1px solid rgba(141, 123, 255, 0.3)',
                      cursor: 'pointer',
                      transition: 'all 0.2s',
                    }}
                    onMouseOver={(e) => {
                      if (selectedTag !== tag) {
                        e.currentTarget.style.background = 'rgba(141, 123, 255, 0.25)'
                      }
                    }}
                    onMouseOut={(e) => {
                      if (selectedTag !== tag) {
                        e.currentTarget.style.background = 'rgba(141, 123, 255, 0.15)'
                      }
                    }}
                  >
                    #{tag} <small style={{ opacity: 0.7 }}>{count}</small>
                  </span>
                ))
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
