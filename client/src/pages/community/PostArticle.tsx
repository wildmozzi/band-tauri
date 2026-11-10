import type { Post } from '../../services/communityApi'
import { POST_INSTRUMENTS, formatRelativeTime } from './postLogic'

const DETAIL_INSTRUMENTS = [{ id: 'all', name: '전체', icon: '🎵' }, ...POST_INSTRUMENTS]

interface PostArticleProps {
  post: Post
  isAuthor: boolean
  onEdit: () => void
  onDelete: () => void
  onLike: () => void
}

/** 게시물 헤더(제목·태그·작성자·수정/삭제) + 본문·이미지 + 좋아요. */
export function PostArticle({ post, isAuthor, onEdit, onDelete, onLike }: PostArticleProps) {
  return (
    <>
      {/* 헤더: 제목과 태그 */}
      <div
        style={{
          padding: '24px 28px',
          borderBottom: '1px solid rgba(255, 255, 255, 0.08)',
          background: 'rgba(0, 0, 0, 0.2)',
        }}
      >
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: '16px' }}>
          <h1 style={{ fontSize: '28px', fontWeight: '700', margin: 0, flex: 1, lineHeight: '1.3' }}>
            {post.title || post.content}
          </h1>
          {post.tags.length > 0 && (
            <div style={{ display: 'flex', gap: '6px', flexShrink: 0 }}>
              {post.tags.map((tag, idx) => (
                <span
                  key={idx}
                  style={{
                    display: 'inline-block',
                    background: 'rgba(141, 123, 255, 0.15)',
                    color: '#a89fff',
                    padding: '4px 10px',
                    borderRadius: '6px',
                    fontSize: '13px',
                    fontWeight: '500',
                    border: '1px solid rgba(141, 123, 255, 0.3)',
                  }}
                >
                  #{tag}
                </span>
              ))}
            </div>
          )}
        </div>

        {/* 작성자 정보 및 수정/삭제 버튼 */}
        <div style={{ marginTop: '12px', fontSize: '14px', color: 'rgba(255, 255, 255, 0.6)', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <div>
            <span style={{ fontSize: '18px', marginRight: '8px' }}>
              {DETAIL_INSTRUMENTS.find(i => i.id === post.instrument)?.icon}
            </span>
            <span style={{ fontWeight: '600', color: 'rgba(255, 255, 255, 0.85)' }}>{post.author_name}</span>
            <span style={{ margin: '0 6px' }}>·</span>
            <span>{formatRelativeTime(post.created_at)}</span>
          </div>
          {isAuthor && (
            <div style={{ display: 'flex', gap: '8px' }}>
              <button
                onClick={onEdit}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: '1px solid rgba(141, 123, 255, 0.3)',
                  background: 'rgba(141, 123, 255, 0.1)',
                  color: '#a89fff',
                  cursor: 'pointer',
                  fontSize: '13px',
                  fontWeight: '500',
                }}
              >
                수정
              </button>
              <button
                onClick={onDelete}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: '1px solid rgba(255, 100, 100, 0.3)',
                  background: 'rgba(255, 100, 100, 0.1)',
                  color: '#ff6464',
                  cursor: 'pointer',
                  fontSize: '13px',
                  fontWeight: '500',
                }}
              >
                삭제
              </button>
            </div>
          )}
        </div>
      </div>

      {/* 본문 */}
      <div style={{ padding: '28px', lineHeight: '1.7', fontSize: '16px' }}>
        <p style={{ margin: 0, whiteSpace: 'pre-wrap' }}>{post.title ? post.content : ''}</p>

        {/* 이미지 갤러리 */}
        {post.images && post.images.length > 0 && (
          <div style={{ marginTop: '20px', display: 'flex', flexDirection: 'column', gap: '12px' }}>
            {post.images.map((imageUrl, idx) => (
              <img
                key={idx}
                src={imageUrl}
                alt={`첨부 이미지 ${idx + 1}`}
                style={{
                  maxWidth: '100%',
                  borderRadius: '8px',
                  border: '1px solid rgba(255, 255, 255, 0.1)',
                }}
              />
            ))}
          </div>
        )}
      </div>

      {/* 좋아요 */}
      <div
        style={{
          padding: '20px 28px',
          borderTop: '1px solid rgba(255, 255, 255, 0.08)',
          borderBottom: '1px solid rgba(255, 255, 255, 0.08)',
          display: 'flex',
          justifyContent: 'center',
        }}
      >
        <button
          onClick={onLike}
          data-testid="post-like-button"
          style={{
            background: 'transparent',
            border: 'none',
            cursor: 'pointer',
            display: 'flex',
            alignItems: 'center',
            gap: '8px',
            padding: '8px 16px',
            borderRadius: '20px',
            transition: 'all 0.2s',
          }}
          onMouseOver={(e) => {
            e.currentTarget.style.background = 'rgba(255, 255, 255, 0.05)'
            e.currentTarget.style.transform = 'scale(1.05)'
          }}
          onMouseOut={(e) => {
            e.currentTarget.style.background = 'transparent'
            e.currentTarget.style.transform = 'scale(1)'
          }}
        >
          <span style={{ fontSize: '24px' }}>
            {post.liked_by_user ? '❤️' : '🤍'}
          </span>
          <span style={{
            fontSize: '16px',
            fontWeight: '600',
            color: post.liked_by_user ? '#ff7ab8' : 'rgba(255, 255, 255, 0.7)'
          }}>
            {post.likes_count}
          </span>
        </button>
      </div>
    </>
  )
}
