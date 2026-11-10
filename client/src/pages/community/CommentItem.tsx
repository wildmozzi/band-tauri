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
