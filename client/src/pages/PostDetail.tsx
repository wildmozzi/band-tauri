import { useState, useEffect, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import { communityApi, type Post, type Comment } from '../services/communityApi'

// Comment component for rendering nested comments
interface CommentItemProps {
  comment: Comment
  user: { id: string; email?: string } | null
  postId: string
  depth?: number
  onReply: (parentId: string, content: string) => Promise<void>
  onEdit: (commentId: string, content: string) => Promise<void>
  onDelete: (commentId: string) => Promise<void>
  formatTime: (iso: string) => string
}

function CommentItem({ comment, user, postId, depth = 0, onReply, onEdit, onDelete, formatTime }: CommentItemProps) {
  const [isReplying, setIsReplying] = useState(false)
  const [replyContent, setReplyContent] = useState('')
  const [isEditing, setIsEditing] = useState(false)
  const [editContent, setEditContent] = useState(comment.content)
  const [submitting, setSubmitting] = useState(false)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)

  const isAuthor = user && user.id === comment.author_id
  const maxDepth = 2 // 최대 대댓글 깊이

  const handleSubmitReply = async () => {
    if (!replyContent.trim()) return
    setSubmitting(true)
    try {
      await onReply(comment.id, replyContent.trim())
      setReplyContent('')
      setIsReplying(false)
    } finally {
      setSubmitting(false)
    }
  }

  const handleSubmitEdit = async () => {
    if (!editContent.trim()) return
    setSubmitting(true)
    try {
      await onEdit(comment.id, editContent.trim())
      setIsEditing(false)
    } finally {
      setSubmitting(false)
    }
  }

  const handleDelete = async () => {
    setSubmitting(true)
    try {
      await onDelete(comment.id)
    } finally {
      setSubmitting(false)
      setShowDeleteConfirm(false)
    }
  }

  return (
    <div style={{ marginLeft: depth > 0 ? '24px' : '0' }}>
      <div
        style={{
          padding: '16px',
          background: depth > 0 ? 'rgba(141, 123, 255, 0.05)' : 'rgba(0, 0, 0, 0.25)',
          borderRadius: '8px',
          border: `1px solid ${depth > 0 ? 'rgba(141, 123, 255, 0.15)' : 'rgba(255, 255, 255, 0.05)'}`,
          marginBottom: '8px',
        }}
      >
        {/* 댓글 헤더 */}
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: '10px' }}>
          <div style={{ fontSize: '14px' }}>
            <span style={{ fontWeight: '600', color: 'rgba(255, 255, 255, 0.9)' }}>
              {comment.author_name}
            </span>
            <span style={{ color: 'rgba(255, 255, 255, 0.4)', fontSize: '13px', marginLeft: '8px' }}>
              {formatTime(comment.created_at)}
              {comment.updated_at && comment.updated_at !== comment.created_at && (
                <span style={{ marginLeft: '4px' }}>(수정됨)</span>
              )}
            </span>
          </div>
          {isAuthor && !isEditing && (
            <div style={{ display: 'flex', gap: '8px' }}>
              <button
                onClick={() => setIsEditing(true)}
                style={{
                  padding: '4px 8px',
                  borderRadius: '4px',
                  border: 'none',
                  background: 'rgba(141, 123, 255, 0.2)',
                  color: '#a89fff',
                  cursor: 'pointer',
                  fontSize: '12px',
                }}
              >
                수정
              </button>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                style={{
                  padding: '4px 8px',
                  borderRadius: '4px',
                  border: 'none',
                  background: 'rgba(255, 100, 100, 0.2)',
                  color: '#ff6464',
                  cursor: 'pointer',
                  fontSize: '12px',
                }}
              >
                삭제
              </button>
            </div>
          )}
        </div>

        {/* 댓글 본문 또는 수정 폼 */}
        {isEditing ? (
          <div>
            <textarea
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              rows={3}
              style={{
                width: '100%',
                padding: '10px',
                borderRadius: '6px',
                border: '1px solid rgba(141, 123, 255, 0.3)',
                background: 'rgba(0, 0, 0, 0.3)',
                color: '#fff',
                fontSize: '14px',
                resize: 'vertical',
                marginBottom: '8px',
                fontFamily: 'inherit',
              }}
            />
            <div style={{ display: 'flex', gap: '8px' }}>
              <button
                onClick={handleSubmitEdit}
                disabled={!editContent.trim() || submitting}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: 'none',
                  background: editContent.trim() && !submitting ? '#8d7bff' : 'rgba(141, 123, 255, 0.3)',
                  color: '#fff',
                  cursor: editContent.trim() && !submitting ? 'pointer' : 'not-allowed',
                  fontSize: '13px',
                  fontWeight: '600',
                }}
              >
                {submitting ? '저장 중...' : '저장'}
              </button>
              <button
                onClick={() => {
                  setIsEditing(false)
                  setEditContent(comment.content)
                }}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: '1px solid rgba(255, 255, 255, 0.15)',
                  background: 'transparent',
                  color: 'rgba(255, 255, 255, 0.7)',
                  cursor: 'pointer',
                  fontSize: '13px',
                }}
              >
                취소
              </button>
            </div>
          </div>
        ) : (
          <p style={{ margin: 0, lineHeight: '1.6', fontSize: '15px', color: 'rgba(255, 255, 255, 0.85)' }}>
            {comment.content}
          </p>
        )}

        {/* 답글 버튼 */}
        {!isEditing && user && depth < maxDepth && (
          <button
            onClick={() => setIsReplying(!isReplying)}
            style={{
              marginTop: '10px',
              padding: '4px 10px',
              borderRadius: '4px',
              border: 'none',
              background: 'transparent',
              color: 'rgba(141, 123, 255, 0.8)',
              cursor: 'pointer',
              fontSize: '13px',
              fontWeight: '500',
            }}
          >
            {isReplying ? '취소' : '답글'}
          </button>
        )}

        {/* 답글 입력 폼 */}
        {isReplying && (
          <div style={{ marginTop: '12px' }}>
            <textarea
              value={replyContent}
              onChange={(e) => setReplyContent(e.target.value)}
              placeholder="답글을 입력하세요..."
              rows={3}
              style={{
                width: '100%',
                padding: '10px',
                borderRadius: '6px',
                border: '1px solid rgba(141, 123, 255, 0.3)',
                background: 'rgba(0, 0, 0, 0.3)',
                color: '#fff',
                fontSize: '14px',
                resize: 'vertical',
                marginBottom: '8px',
                fontFamily: 'inherit',
              }}
            />
            <button
              onClick={handleSubmitReply}
              disabled={!replyContent.trim() || submitting}
              style={{
                padding: '6px 12px',
                borderRadius: '6px',
                border: 'none',
                background: replyContent.trim() && !submitting ? '#8d7bff' : 'rgba(141, 123, 255, 0.3)',
                color: '#fff',
                cursor: replyContent.trim() && !submitting ? 'pointer' : 'not-allowed',
                fontSize: '13px',
                fontWeight: '600',
              }}
            >
              {submitting ? '작성 중...' : '답글 작성'}
            </button>
          </div>
        )}

        {/* 삭제 확인 */}
        {showDeleteConfirm && (
          <div style={{
            marginTop: '12px',
            padding: '12px',
            background: 'rgba(255, 100, 100, 0.1)',
            borderRadius: '6px',
            border: '1px solid rgba(255, 100, 100, 0.3)',
          }}>
            <p style={{ margin: '0 0 10px 0', fontSize: '14px', color: 'rgba(255, 255, 255, 0.8)' }}>
              정말 이 댓글을 삭제하시겠습니까?
            </p>
            <div style={{ display: 'flex', gap: '8px' }}>
              <button
                onClick={handleDelete}
                disabled={submitting}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: 'none',
                  background: submitting ? 'rgba(255, 100, 100, 0.5)' : '#ff6464',
                  color: '#fff',
                  cursor: submitting ? 'not-allowed' : 'pointer',
                  fontSize: '13px',
                  fontWeight: '600',
                }}
              >
                {submitting ? '삭제 중...' : '삭제'}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(false)}
                style={{
                  padding: '6px 12px',
                  borderRadius: '6px',
                  border: '1px solid rgba(255, 255, 255, 0.15)',
                  background: 'transparent',
                  color: 'rgba(255, 255, 255, 0.7)',
                  cursor: 'pointer',
                  fontSize: '13px',
                }}
              >
                취소
              </button>
            </div>
          </div>
        )}
      </div>

      {/* 대댓글 렌더링 */}
      {comment.replies && comment.replies.length > 0 && (
        <div style={{ marginTop: '4px' }}>
          {comment.replies.map((reply) => (
            <CommentItem
              key={reply.id}
              comment={reply}
              user={user}
              postId={postId}
              depth={depth + 1}
              onReply={onReply}
              onEdit={onEdit}
              onDelete={onDelete}
              formatTime={formatTime}
            />
          ))}
        </div>
      )}
    </div>
  )
}

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

export function PostDetail() {
  const { postId } = useParams<{ postId: string }>()
  const navigate = useNavigate()
  const { user } = useAuth()
  const [post, setPost] = useState<Post | null>(null)
  const [comments, setComments] = useState<Comment[]>([])
  const [loading, setLoading] = useState(true)
  const [newComment, setNewComment] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [trendingTags, setTrendingTags] = useState<{ tag: string; count: number }[]>([])
  const [deleting, setDeleting] = useState(false)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)

  // Fetch post data
  const fetchPost = useCallback(async () => {
    if (!postId) return

    try {
      setLoading(true)
      const data = await communityApi.getPost(postId, user?.id)
      if (!data) {
        navigate('/community')
        return
      }
      setPost(data)
    } catch (error) {
      console.error('Failed to fetch post:', error)
      navigate('/community')
    } finally {
      setLoading(false)
    }
  }, [postId, user?.id, navigate])

  // Fetch comments
  const fetchComments = useCallback(async () => {
    if (!postId) return

    try {
      const data = await communityApi.getComments(postId)
      setComments(data)
    } catch (error) {
      console.error('Failed to fetch comments:', error)
    }
  }, [postId])

  // Fetch trending tags
  const fetchTrendingTags = useCallback(async () => {
    try {
      const tags = await communityApi.getTrendingTags(5)
      setTrendingTags(tags)
    } catch (error) {
      console.error('Failed to fetch trending tags:', error)
    }
  }, [])

  useEffect(() => {
    fetchPost()
    fetchComments()
    fetchTrendingTags()
  }, [fetchPost, fetchComments, fetchTrendingTags])

  const handleLike = async () => {
    if (!post || !user) {
      if (!user) alert('좋아요를 누르려면 로그인이 필요합니다.')
      return
    }

    try {
      const { liked, likesCount } = await communityApi.toggleLike(post.id, user.id)
      setPost(prev => prev ? {
        ...prev,
        likes_count: likesCount,
        liked_by_user: liked
      } : null)
    } catch (error) {
      console.error('Failed to toggle like:', error)
    }
  }

  const handleAddComment = async () => {
    if (!newComment.trim() || !user || !post) return

    try {
      setSubmitting(true)
      const comment = await communityApi.addComment({
        post_id: post.id,
        content: newComment.trim()
      }, user)

      setComments(prev => [...prev, comment])
      setPost(prev => prev ? { ...prev, comments_count: prev.comments_count + 1 } : null)
      setNewComment('')
    } catch (error) {
      console.error('Failed to add comment:', error)
      alert('댓글 작성에 실패했습니다.')
    } finally {
      setSubmitting(false)
    }
  }

  // 대댓글 추가
  const handleReply = async (parentId: string, content: string) => {
    if (!user || !post) return

    try {
      const reply = await communityApi.addComment({
        post_id: post.id,
        content,
        parent_id: parentId
      }, user)

      // 댓글 목록에서 부모 댓글을 찾아 replies에 추가
      setComments(prev => {
        const addReplyToComment = (comments: Comment[]): Comment[] => {
          return comments.map(comment => {
            if (comment.id === parentId) {
              return {
                ...comment,
                replies: [...(comment.replies || []), reply]
              }
            }
            if (comment.replies && comment.replies.length > 0) {
              return {
                ...comment,
                replies: addReplyToComment(comment.replies)
              }
            }
            return comment
          })
        }
        return addReplyToComment(prev)
      })
      setPost(prev => prev ? { ...prev, comments_count: prev.comments_count + 1 } : null)
    } catch (error) {
      console.error('Failed to add reply:', error)
      alert('답글 작성에 실패했습니다.')
    }
  }

  // 댓글 수정
  const handleEditComment = async (commentId: string, content: string) => {
    try {
      const updatedComment = await communityApi.updateComment(commentId, content)

      // 댓글 목록에서 해당 댓글을 찾아 업데이트
      setComments(prev => {
        const updateCommentInList = (comments: Comment[]): Comment[] => {
          return comments.map(comment => {
            if (comment.id === commentId) {
              return { ...comment, content: updatedComment.content, updated_at: updatedComment.updated_at }
            }
            if (comment.replies && comment.replies.length > 0) {
              return {
                ...comment,
                replies: updateCommentInList(comment.replies)
              }
            }
            return comment
          })
        }
        return updateCommentInList(prev)
      })
    } catch (error) {
      console.error('Failed to update comment:', error)
      alert('댓글 수정에 실패했습니다.')
    }
  }

  // 댓글 삭제
  const handleDeleteComment = async (commentId: string) => {
    try {
      await communityApi.deleteComment(commentId)

      // 댓글 목록에서 해당 댓글 제거 (대댓글 포함)
      setComments(prev => {
        const removeCommentFromList = (comments: Comment[]): Comment[] => {
          return comments
            .filter(comment => comment.id !== commentId)
            .map(comment => ({
              ...comment,
              replies: comment.replies ? removeCommentFromList(comment.replies) : []
            }))
        }
        return removeCommentFromList(prev)
      })

      // 삭제된 댓글 수 계산 (대댓글 포함)
      const countDeletedComments = (comments: Comment[], targetId: string): number => {
        for (const comment of comments) {
          if (comment.id === targetId) {
            return 1 + (comment.replies?.length || 0)
          }
          if (comment.replies) {
            const count = countDeletedComments(comment.replies, targetId)
            if (count > 0) return count
          }
        }
        return 0
      }

      const deletedCount = countDeletedComments(comments, commentId)
      setPost(prev => prev ? { ...prev, comments_count: Math.max(0, prev.comments_count - deletedCount) } : null)
    } catch (error) {
      console.error('Failed to delete comment:', error)
      alert('댓글 삭제에 실패했습니다.')
    }
  }

  const handleDelete = async () => {
    if (!post) return

    try {
      setDeleting(true)
      await communityApi.deletePost(post.id)
      navigate('/community')
    } catch (error) {
      console.error('Failed to delete post:', error)
      alert('게시물 삭제에 실패했습니다.')
    } finally {
      setDeleting(false)
      setShowDeleteConfirm(false)
    }
  }

  const isAuthor = user && post && user.id === post.author_id

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

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 320px', gap: '24px' }}>
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
                    {INSTRUMENTS.find(i => i.id === post.instrument)?.icon}
                  </span>
                  <span style={{ fontWeight: '600', color: 'rgba(255, 255, 255, 0.85)' }}>{post.author_name}</span>
                  <span style={{ margin: '0 6px' }}>·</span>
                  <span>{formatRelativeTime(post.created_at)}</span>
                </div>
                {isAuthor && (
                  <div style={{ display: 'flex', gap: '8px' }}>
                    <button
                      onClick={() => navigate(`/community/edit/${post.id}`)}
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
                      onClick={() => setShowDeleteConfirm(true)}
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
                onClick={handleLike}
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

            {/* 댓글 입력 */}
            <div style={{ padding: '28px' }}>
              <h3 style={{ fontSize: '18px', fontWeight: '600', marginBottom: '16px' }}>
                댓글 {comments.length}개
              </h3>
              {user ? (
                <div>
                  <textarea
                    value={newComment}
                    placeholder="댓글을 입력하세요..."
                    onChange={(e) => setNewComment(e.target.value)}
                    rows={4}
                    style={{
                      width: '100%',
                      padding: '14px',
                      borderRadius: '8px',
                      border: '1px solid rgba(255, 255, 255, 0.15)',
                      background: 'rgba(0, 0, 0, 0.3)',
                      color: '#fff',
                      fontSize: '14px',
                      resize: 'vertical',
                      marginBottom: '12px',
                      fontFamily: 'inherit',
                    }}
                  />
                  <button
                    onClick={handleAddComment}
                    disabled={!newComment.trim() || submitting}
                    style={{
                      background: newComment.trim() && !submitting ? '#8d7bff' : 'rgba(141, 123, 255, 0.3)',
                      border: 'none',
                      color: '#fff',
                      padding: '10px 20px',
                      borderRadius: '6px',
                      cursor: newComment.trim() && !submitting ? 'pointer' : 'not-allowed',
                      fontSize: '14px',
                      fontWeight: '600',
                    }}
                  >
                    {submitting ? '작성 중...' : '댓글 작성'}
                  </button>
                </div>
              ) : (
                <div
                  style={{
                    padding: '16px',
                    background: 'rgba(0, 0, 0, 0.2)',
                    borderRadius: '8px',
                    textAlign: 'center',
                    color: 'rgba(255, 255, 255, 0.5)',
                  }}
                >
                  댓글을 작성하려면 로그인이 필요합니다.
                </div>
              )}
            </div>

            {/* 댓글 목록 */}
            {comments.length > 0 && (
              <div style={{ padding: '0 28px 28px' }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                  {comments.map((comment) => (
                    <CommentItem
                      key={comment.id}
                      comment={comment}
                      user={user}
                      postId={post.id}
                      onReply={handleReply}
                      onEdit={handleEditComment}
                      onDelete={handleDeleteComment}
                      formatTime={formatRelativeTime}
                    />
                  ))}
                </div>
              </div>
            )}
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
                <p style={{ color: 'rgba(255, 255, 255, 0.5)', fontSize: '14px' }}>태그가 없습니다.</p>
              ) : (
                trendingTags.map(({ tag, count }) => (
                  <span
                    key={tag}
                    onClick={() => navigate('/community')}
                    style={{
                      display: 'inline-block',
                      background: 'rgba(141, 123, 255, 0.15)',
                      color: '#a89fff',
                      padding: '6px 12px',
                      borderRadius: '6px',
                      fontSize: '13px',
                      fontWeight: '500',
                      border: '1px solid rgba(141, 123, 255, 0.3)',
                      cursor: 'pointer',
                      transition: 'all 0.2s',
                    }}
                    onMouseOver={(e) => {
                      e.currentTarget.style.background = 'rgba(141, 123, 255, 0.25)'
                    }}
                    onMouseOut={(e) => {
                      e.currentTarget.style.background = 'rgba(141, 123, 255, 0.15)'
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

      {/* 삭제 확인 모달 */}
      {showDeleteConfirm && (
        <div
          style={{
            position: 'fixed',
            top: 0,
            left: 0,
            right: 0,
            bottom: 0,
            background: 'rgba(0, 0, 0, 0.8)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            zIndex: 1000,
          }}
          onClick={() => setShowDeleteConfirm(false)}
        >
          <div
            style={{
              background: 'rgba(18, 22, 45, 0.95)',
              border: '1px solid rgba(255, 255, 255, 0.1)',
              borderRadius: '12px',
              padding: '24px',
              maxWidth: '400px',
              width: '90%',
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <h3 style={{ fontSize: '18px', fontWeight: '600', marginBottom: '12px' }}>
              게시물 삭제
            </h3>
            <p style={{ color: 'rgba(255, 255, 255, 0.7)', marginBottom: '20px', lineHeight: '1.5' }}>
              정말 이 게시물을 삭제하시겠습니까?<br />
              삭제된 게시물은 복구할 수 없습니다.
            </p>
            <div style={{ display: 'flex', gap: '12px', justifyContent: 'flex-end' }}>
              <button
                onClick={() => setShowDeleteConfirm(false)}
                style={{
                  padding: '10px 20px',
                  borderRadius: '8px',
                  border: '1px solid rgba(255, 255, 255, 0.15)',
                  background: 'transparent',
                  color: 'rgba(255, 255, 255, 0.7)',
                  cursor: 'pointer',
                  fontSize: '14px',
                  fontWeight: '600',
                }}
              >
                취소
              </button>
              <button
                onClick={handleDelete}
                disabled={deleting}
                style={{
                  padding: '10px 20px',
                  borderRadius: '8px',
                  border: 'none',
                  background: deleting ? 'rgba(255, 100, 100, 0.5)' : '#ff6464',
                  color: '#fff',
                  cursor: deleting ? 'not-allowed' : 'pointer',
                  fontSize: '14px',
                  fontWeight: '600',
                }}
              >
                {deleting ? '삭제 중...' : '삭제'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
