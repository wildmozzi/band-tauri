import { useEffect, useState } from 'react'

/** 한 번에 하나만 열리는 순수 토글: 같은 id면 닫고, 다른 id면 그것만 연다. */
export function toggleSingleExpanded(prev: Set<string>, id: string): Set<string> {
  const next = new Set(prev)
  if (next.has(id)) {
    next.delete(id)
  } else {
    next.clear()
    next.add(id)
  }
  return next
}

/** 카드의 "+N" 태그 팝업 열림 상태. 바깥 클릭 시 닫힌다. */
export function useExpandedTags() {
  const [expandedTags, setExpandedTags] = useState<Set<string>>(new Set())

  const toggleTagsExpanded = (e: React.MouseEvent, roomId: string) => {
    e.stopPropagation() // 카드 클릭 이벤트 방지
    setExpandedTags(prev => toggleSingleExpanded(prev, roomId))
  }

  useEffect(() => {
    const handleClickOutside = () => {
      if (expandedTags.size > 0) {
        setExpandedTags(new Set())
      }
    }
    document.addEventListener('click', handleClickOutside)
    return () => document.removeEventListener('click', handleClickOutside)
  }, [expandedTags.size])

  return { expandedTags, toggleTagsExpanded }
}
