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
          padding: 'var(--space-6) 28px',
          borderBottom: '1px solid var(--white-a08)',
          background: 'var(--black-a20)',
        }}
      >
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 'var(--space-4)' }}>
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
                    background: 'var(--violet-500-a15)',
                    color: 'var(--violet-300)',
                    padding: 'var(--space-1) 10px',
                    borderRadius: 'var(--radius-sm)',
                    fontSize: '13px',
                    fontWeight: '500',
                    border: '1px solid var(--violet-500-a30)',
                  }}
                >
                  #{tag}
                </span>
              ))}
            </div>
          )}
        </div>

        {/* 작성자 정보 및 수정/삭제 버튼 */}
        <div style={{ marginTop: 'var(--space-3)', fontSize: '14px', color: 'var(--white-a60)', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <div>
            <span style={{ fontSize: '18px', marginRight: 'var(--space-2)' }}>
              {DETAIL_INSTRUMENTS.find(i => i.id === post.instrument)?.icon}
            </span>
            <span style={{ fontWeight: '600', color: 'var(--white-a85)' }}>{post.author_name}</span>
            <span style={{ margin: '0 6px' }}>·</span>
            <span>{formatRelativeTime(post.created_at)}</span>
          </div>
          {isAuthor && (
            <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
              <button
                onClick={onEdit}
                style={{
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--violet-500-a30)',
                  background: 'var(--violet-500-a10)',
                  color: 'var(--violet-300)',
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
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--coral-400-a30)',
                  background: 'var(--coral-400-a10)',
                  color: 'var(--coral-400)',
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
          <div style={{ marginTop: 'var(--space-5)', display: 'flex', flexDirection: 'column', gap: 'var(--space-3)' }}>
            {post.images.map((imageUrl, idx) => (
              <img
                key={idx}
                src={imageUrl}
                alt={`첨부 이미지 ${idx + 1}`}
                style={{
                  maxWidth: '100%',
                  borderRadius: 'var(--radius-md)',
                  border: '1px solid var(--white-a10)',
                }}
              />
            ))}
          </div>
        )}
      </div>

      {/* 좋아요 */}
      <div
        style={{
          padding: 'var(--space-5) 28px',
          borderTop: '1px solid var(--white-a08)',
          borderBottom: '1px solid var(--white-a08)',
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
            gap: 'var(--space-2)',
            padding: 'var(--space-2) var(--space-4)',
            borderRadius: 'var(--radius-pill)',
            transition: 'all 0.2s',
          }}
          onMouseOver={(e) => {
            e.currentTarget.style.background = 'var(--white-a05)'
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
            color: post.liked_by_user ? 'var(--pink-400)' : 'var(--white-a70)'
          }}>
            {post.likes_count}
          </span>
        </button>
      </div>
    </>
  )
}
