import { useState } from 'react'
import type { Comment } from '../../services/communityApi'
import { CommentItem } from './CommentItem'
import { formatRelativeTime } from './postLogic'

interface CommentSectionProps {
  postId: string
  comments: Comment[]
  user: { id: string; email?: string } | null
  /** 최상위 댓글 작성. 성공 시 true */
  onAdd: (content: string) => Promise<boolean>
  onReply: (parentId: string, content: string) => Promise<void>
  onEdit: (commentId: string, content: string) => Promise<void>
  onDelete: (commentId: string) => Promise<void>
}

/** 댓글 입력 폼 + 댓글 목록. */
export function CommentSection({ postId, comments, user, onAdd, onReply, onEdit, onDelete }: CommentSectionProps) {
  const [newComment, setNewComment] = useState('')
  const [submitting, setSubmitting] = useState(false)

  const handleAddComment = async () => {
    if (!newComment.trim() || !user) return

    setSubmitting(true)
    try {
      if (await onAdd(newComment.trim())) setNewComment('')
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <>
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
                postId={postId}
                onReply={onReply}
                onEdit={onEdit}
                onDelete={onDelete}
                formatTime={formatRelativeTime}
              />
            ))}
          </div>
        </div>
      )}
    </>
  )
}
