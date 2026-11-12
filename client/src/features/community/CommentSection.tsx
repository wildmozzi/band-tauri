import { useState } from 'react'
import type { Comment } from './communityApi'
import { CommentItem } from './CommentItem'
import { formatRelativeTime } from './postLogic'

interface CommentSectionProps {
  comments: Comment[]
  user: { id: string; email?: string } | null
  /** 최상위 댓글 작성. 성공 시 true */
  onAdd: (content: string) => Promise<boolean>
  onReply: (parentId: string, content: string) => Promise<void>
  onEdit: (commentId: string, content: string) => Promise<void>
  onDelete: (commentId: string) => Promise<void>
}

/** 댓글 입력 폼 + 댓글 목록. */
export function CommentSection({ comments, user, onAdd, onReply, onEdit, onDelete }: CommentSectionProps) {
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
        <h3 style={{ fontSize: '18px', fontWeight: '600', marginBottom: 'var(--space-4)' }}>
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
                borderRadius: 'var(--radius-md)',
                border: '1px solid var(--white-a15)',
                background: 'var(--black-a30)',
                color: 'var(--white)',
                fontSize: '14px',
                resize: 'vertical',
                marginBottom: 'var(--space-3)',
                fontFamily: 'inherit',
              }}
            />
            <button
              onClick={handleAddComment}
              disabled={!newComment.trim() || submitting}
              style={{
                background: newComment.trim() && !submitting ? 'var(--accent)' : 'var(--violet-500-a30)',
                border: 'none',
                color: 'var(--white)',
                padding: '10px var(--space-5)',
                borderRadius: 'var(--radius-sm)',
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
              padding: 'var(--space-4)',
              background: 'var(--black-a20)',
              borderRadius: 'var(--radius-md)',
              textAlign: 'center',
              color: 'var(--white-a50)',
            }}
          >
            댓글을 작성하려면 로그인이 필요합니다.
          </div>
        )}
      </div>

      {/* 댓글 목록 */}
      {comments.length > 0 && (
        <div style={{ padding: '0 28px 28px' }}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-3)' }}>
            {comments.map((comment) => (
              <CommentItem
                key={comment.id}
                comment={comment}
                user={user}
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
