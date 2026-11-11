import { useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { CommentSection } from './community/CommentSection'
import { DeletePostModal } from './community/DeletePostModal'
import { PostArticle } from './community/PostArticle'
import { TrendingTagsCard } from './community/TrendingTagsCard'
import { usePostDetail } from './community/usePostDetail'

export function PostDetail() {
  const { postId } = useParams<{ postId: string }>()
  const navigate = useNavigate()
  const { user } = useAuth()
  const {
    post,
    comments,
    loading,
    trendingTags,
    deleting,
    handleLike,
    addComment,
    handleReply,
    handleEditComment,
    handleDeleteComment,
    deletePost,
  } = usePostDetail(postId, user)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)

  const handleDelete = async () => {
    await deletePost()
    setShowDeleteConfirm(false)
  }

  const isAuthor = Boolean(user && post && user.id === post.author_id)

  if (loading) {
    return (
      <div style={{ maxWidth: '1400px', margin: '0 auto', padding: 'var(--space-6)', textAlign: 'center' }}>
        <p style={{ color: 'var(--white-a50)' }}>로딩 중...</p>
      </div>
    )
  }

  if (!post) {
    return (
      <div style={{ maxWidth: '1400px', margin: '0 auto', padding: 'var(--space-6)', textAlign: 'center' }}>
        <p style={{ color: 'var(--white-a50)' }}>게시물을 찾을 수 없습니다.</p>
      </div>
    )
  }

  return (
    <div style={{ maxWidth: '1400px', margin: '0 auto', padding: 'var(--space-6)' }}>
      {/* 뒤로가기 버튼 */}
      <button
        onClick={() => navigate('/community')}
        style={{
          background: 'transparent',
          border: '1px solid var(--white-a15)',
          color: 'var(--white-a70)',
          padding: 'var(--space-2) var(--space-4)',
          borderRadius: 'var(--radius-sm)',
          cursor: 'pointer',
          marginBottom: 'var(--space-6)',
          fontSize: '14px',
        }}
      >
        ← 목록으로
      </button>

      <div className="community-feed-grid">
        {/* 메인 콘텐츠 */}
        <div>
          {/* 메인 카드 */}
          <div
            style={{
              background: 'var(--panel-strong-a80)',
              border: '1px solid var(--white-a10)',
              borderRadius: 'var(--radius-xl)',
              overflow: 'hidden',
            }}
          >
            <PostArticle
              post={post}
              isAuthor={isAuthor}
              onEdit={() => navigate(`/community/edit/${post.id}`)}
              onDelete={() => setShowDeleteConfirm(true)}
              onLike={handleLike}
            />
            <CommentSection
              comments={comments}
              user={user}
              onAdd={addComment}
              onReply={handleReply}
              onEdit={handleEditComment}
              onDelete={handleDeleteComment}
            />
          </div>
        </div>

        {/* 사이드바 */}
        <div>
          <TrendingTagsCard tags={trendingTags} onTagClick={() => navigate('/community')} />
        </div>
      </div>

      {/* 삭제 확인 모달 */}
      {showDeleteConfirm && (
        <DeletePostModal
          deleting={deleting}
          onCancel={() => setShowDeleteConfirm(false)}
          onConfirm={handleDelete}
        />
      )}
    </div>
  )
}
