interface TrendingTagsCardProps {
  tags: { tag: string; count: number }[]
  onTagClick: () => void
}

export function TrendingTagsCard({ tags, onTagClick }: TrendingTagsCardProps) {
  return (
    <div
      style={{
        background: 'var(--panel-strong-a80)',
        border: '1px solid var(--white-a10)',
        borderRadius: 'var(--radius-xl)',
        padding: 'var(--space-5)',
        marginBottom: 'var(--space-4)',
      }}
    >
      <h3 style={{ fontSize: '16px', fontWeight: '600', marginBottom: 'var(--space-3)' }}>트렌딩 태그</h3>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--space-2)' }}>
        {tags.length === 0 ? (
          <p style={{ color: 'var(--white-a50)', fontSize: '14px' }}>태그가 없습니다.</p>
        ) : (
          tags.map(({ tag, count }) => (
            <span
              key={tag}
              onClick={onTagClick}
              style={{
                display: 'inline-block',
                background: 'var(--violet-500-a15)',
                color: 'var(--violet-300)',
                padding: '6px var(--space-3)',
                borderRadius: 'var(--radius-sm)',
                fontSize: '13px',
                fontWeight: '500',
                border: '1px solid var(--violet-500-a30)',
                cursor: 'pointer',
                transition: 'all 0.2s',
              }}
              onMouseOver={(e) => {
                e.currentTarget.style.background = 'var(--violet-500-a25)'
              }}
              onMouseOut={(e) => {
                e.currentTarget.style.background = 'var(--violet-500-a15)'
              }}
            >
              #{tag} <small style={{ opacity: 0.7 }}>{count}</small>
            </span>
          ))
        )}
      </div>
    </div>
  )
}
