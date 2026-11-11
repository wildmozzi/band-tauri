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
      <div style={{ maxWidth: '1400px', margin: '0 auto', padding: '24px', textAlign: 'center' }}>
        <p style={{ color: 'rgba(255, 255, 255, 0.5)' }}>로딩 중...</p>
      </div>
    )
  }

  if (!post) {
    return (
      <div style={{ maxWidth: '1400px', margin: '0 auto', padding: '24px', textAlign: 'center' }}>
        <p style={{ color: 'rgba(255, 255, 255, 0.5)' }}>게시물을 찾을 수 없습니다.</p>
      </div>
    )
  }

  return (
    <div style={{ maxWidth: '1400px', margin: '0 auto', padding: '24px' }}>
      {/* 뒤로가기 버튼 */}
      <button
        onClick={() => navigate('/community')}
        style={{
          background: 'transparent',
          border: '1px solid rgba(255, 255, 255, 0.15)',
          color: 'rgba(255, 255, 255, 0.7)',
          padding: '8px 16px',
          borderRadius: '6px',
          cursor: 'pointer',
          marginBottom: '24px',
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
              background: 'rgba(18, 22, 45, 0.8)',
              border: '1px solid rgba(255, 255, 255, 0.1)',
              borderRadius: '12px',
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
