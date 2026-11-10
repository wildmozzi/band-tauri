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
        background: 'rgba(0, 0, 0, 0.8)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: 1000,
      }}
      onClick={onCancel}
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
            onClick={onCancel}
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
            onClick={onConfirm}
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
  )
}
