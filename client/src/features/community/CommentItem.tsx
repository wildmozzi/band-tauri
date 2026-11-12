import { useState } from 'react'
import type { Comment } from '../../services/communityApi'

// Comment component for rendering nested comments
interface CommentItemProps {
  comment: Comment
  user: { id: string; email?: string } | null
  depth?: number
  onReply: (parentId: string, content: string) => Promise<void>
  onEdit: (commentId: string, content: string) => Promise<void>
  onDelete: (commentId: string) => Promise<void>
  formatTime: (iso: string) => string
}

export function CommentItem({ comment, user, depth = 0, onReply, onEdit, onDelete, formatTime }: CommentItemProps) {
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
        data-testid="comment"
        style={{
          padding: 'var(--space-4)',
          background: depth > 0 ? 'var(--violet-500-a05)' : 'var(--black-a25)',
          borderRadius: 'var(--radius-md)',
          border: `1px solid ${depth > 0 ? 'var(--violet-500-a15)' : 'var(--white-a05)'}`,
          marginBottom: 'var(--space-2)',
        }}
      >
        {/* 댓글 헤더 */}
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: '10px' }}>
          <div style={{ fontSize: '14px' }}>
            <span style={{ fontWeight: '600', color: 'var(--white-a90)' }}>
              {comment.author_name}
            </span>
            <span style={{ color: 'var(--white-a40)', fontSize: '13px', marginLeft: 'var(--space-2)' }}>
              {formatTime(comment.created_at)}
              {comment.updated_at && comment.updated_at !== comment.created_at && (
                <span style={{ marginLeft: 'var(--space-1)' }}>(수정됨)</span>
              )}
            </span>
          </div>
          {isAuthor && !isEditing && (
            <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
              <button
                onClick={() => setIsEditing(true)}
                style={{
                  padding: 'var(--space-1) var(--space-2)',
                  borderRadius: 'var(--radius-xs)',
                  border: 'none',
                  background: 'var(--violet-500-a20)',
                  color: 'var(--violet-300)',
                  cursor: 'pointer',
                  fontSize: '12px',
                }}
              >
                수정
              </button>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                style={{
                  padding: 'var(--space-1) var(--space-2)',
                  borderRadius: 'var(--radius-xs)',
                  border: 'none',
                  background: 'var(--coral-400-a20)',
                  color: 'var(--coral-400)',
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
                borderRadius: 'var(--radius-sm)',
                border: '1px solid var(--violet-500-a30)',
                background: 'var(--black-a30)',
                color: 'var(--white)',
                fontSize: '14px',
                resize: 'vertical',
                marginBottom: 'var(--space-2)',
                fontFamily: 'inherit',
              }}
            />
            <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
              <button
                onClick={handleSubmitEdit}
                disabled={!editContent.trim() || submitting}
                style={{
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: 'none',
                  background: editContent.trim() && !submitting ? 'var(--accent)' : 'var(--violet-500-a30)',
                  color: 'var(--white)',
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
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--white-a15)',
                  background: 'transparent',
                  color: 'var(--white-a70)',
                  cursor: 'pointer',
                  fontSize: '13px',
                }}
              >
                취소
              </button>
            </div>
          </div>
        ) : (
          <p style={{ margin: 0, lineHeight: '1.6', fontSize: '15px', color: 'var(--white-a85)' }}>
            {comment.content}
          </p>
        )}

        {/* 답글 버튼 */}
        {!isEditing && user && depth < maxDepth && (
          <button
            onClick={() => setIsReplying(!isReplying)}
            style={{
              marginTop: '10px',
              padding: 'var(--space-1) 10px',
              borderRadius: 'var(--radius-xs)',
              border: 'none',
              background: 'transparent',
              color: 'var(--violet-500-a80)',
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
          <div style={{ marginTop: 'var(--space-3)' }}>
            <textarea
              value={replyContent}
              onChange={(e) => setReplyContent(e.target.value)}
              placeholder="답글을 입력하세요..."
              rows={3}
              style={{
                width: '100%',
                padding: '10px',
                borderRadius: 'var(--radius-sm)',
                border: '1px solid var(--violet-500-a30)',
                background: 'var(--black-a30)',
                color: 'var(--white)',
                fontSize: '14px',
                resize: 'vertical',
                marginBottom: 'var(--space-2)',
                fontFamily: 'inherit',
              }}
            />
            <button
              onClick={handleSubmitReply}
              disabled={!replyContent.trim() || submitting}
              style={{
                padding: '6px var(--space-3)',
                borderRadius: 'var(--radius-sm)',
                border: 'none',
                background: replyContent.trim() && !submitting ? 'var(--accent)' : 'var(--violet-500-a30)',
                color: 'var(--white)',
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
            marginTop: 'var(--space-3)',
            padding: 'var(--space-3)',
            background: 'var(--coral-400-a10)',
            borderRadius: 'var(--radius-sm)',
            border: '1px solid var(--coral-400-a30)',
          }}>
            <p style={{ margin: '0 0 10px 0', fontSize: '14px', color: 'var(--white-a80)' }}>
              정말 이 댓글을 삭제하시겠습니까?
            </p>
            <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
              <button
                onClick={handleDelete}
                disabled={submitting}
                style={{
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: 'none',
                  background: submitting ? 'var(--coral-400-a50)' : 'var(--coral-400)',
                  color: 'var(--white)',
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
                  padding: '6px var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--white-a15)',
                  background: 'transparent',
                  color: 'var(--white-a70)',
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
        <div style={{ marginTop: 'var(--space-1)' }}>
          {comment.replies.map((reply) => (
            <CommentItem
              key={reply.id}
              comment={reply}
              user={user}
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
