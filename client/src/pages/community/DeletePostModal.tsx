interface DeletePostModalProps {
  deleting: boolean
  onCancel: () => void
  onConfirm: () => void
}

export function DeletePostModal({ deleting, onCancel, onConfirm }: DeletePostModalProps) {
  return (
    <div
      style={{
        position: 'fixed',
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        background: 'var(--black-a80)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: 1000,
      }}
      onClick={onCancel}
    >
      <div
        style={{
          background: 'var(--panel-strong-a95)',
          border: '1px solid var(--white-a10)',
          borderRadius: 'var(--radius-xl)',
          padding: 'var(--space-6)',
          maxWidth: '400px',
          width: '90%',
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <h3 style={{ fontSize: '18px', fontWeight: '600', marginBottom: 'var(--space-3)' }}>
          게시물 삭제
        </h3>
        <p style={{ color: 'var(--white-a70)', marginBottom: 'var(--space-5)', lineHeight: '1.5' }}>
          정말 이 게시물을 삭제하시겠습니까?<br />
          삭제된 게시물은 복구할 수 없습니다.
        </p>
        <div style={{ display: 'flex', gap: 'var(--space-3)', justifyContent: 'flex-end' }}>
          <button
            onClick={onCancel}
            style={{
              padding: '10px var(--space-5)',
              borderRadius: 'var(--radius-md)',
              border: '1px solid var(--white-a15)',
              background: 'transparent',
              color: 'var(--white-a70)',
              cursor: 'pointer',
              fontSize: '14px',
              fontWeight: '600',
            }}
          >
            취소
          </button>
          <button
            onClick={onConfirm}
            disabled={deleting}
            style={{
              padding: '10px var(--space-5)',
              borderRadius: 'var(--radius-md)',
              border: 'none',
              background: deleting ? 'var(--coral-400-a50)' : 'var(--coral-400)',
              color: 'var(--white)',
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
  )
}
