interface TrendingTagsCardProps {
  tags: { tag: string; count: number }[]
  onTagClick: () => void
}

export function TrendingTagsCard({ tags, onTagClick }: TrendingTagsCardProps) {
  return (
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
        {tags.length === 0 ? (
          <p style={{ color: 'rgba(255, 255, 255, 0.5)', fontSize: '14px' }}>태그가 없습니다.</p>
        ) : (
          tags.map(({ tag, count }) => (
            <span
              key={tag}
              onClick={onTagClick}
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
  )
}
