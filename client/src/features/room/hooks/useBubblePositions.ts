import { useCallback, useEffect, useRef, useState } from 'react'

export type BubblePositions = Record<string, { top: number; left: number }>

/** 연주자 행 요소의 오른쪽 가운데 + 8px (말풍선 화살표 위치) */
export function bubblePositionFor(rect: { top: number; height: number; right: number }) {
  return { top: rect.top + rect.height / 2, left: rect.right + 8 }
}

/**
 * 악기 변경 요청 말풍선 위치. 연주자 행(performerRefs)과 목록(performersListRef)에 ref를 걸면
 * 스크롤/리사이즈마다 fixed 좌표를 다시 계산한다.
 */
export function useBubblePositions(pending: { oderId: string }[]) {
  const performerRefs = useRef<Record<string, HTMLDivElement | null>>({})
  const performersListRef = useRef<HTMLDivElement | null>(null)
  const [bubblePositions, setBubblePositions] = useState<BubblePositions>({})

  // 말풍선 위치 업데이트 함수
  const updateBubblePositions = useCallback(() => {
    const newPositions: BubblePositions = {}
    pending.forEach(request => {
      const el = performerRefs.current[request.oderId]
      if (el) {
        newPositions[request.oderId] = bubblePositionFor(el.getBoundingClientRect())
      }
    })
    setBubblePositions(newPositions)
  }, [pending])

  // 스크롤/리사이즈 시 말풍선 위치 업데이트
  useEffect(() => {
    if (pending.length === 0) return

    updateBubblePositions()

    const performersList = performersListRef.current

    const handleScroll = () => {
      requestAnimationFrame(updateBubblePositions)
    }
    const handleResize = () => {
      requestAnimationFrame(updateBubblePositions)
    }

    // performers-list 스크롤 이벤트 리스너
    performersList?.addEventListener('scroll', handleScroll, { passive: true })
    window.addEventListener('resize', handleResize)
    window.addEventListener('scroll', handleScroll, { passive: true })

    return () => {
      performersList?.removeEventListener('scroll', handleScroll)
      window.removeEventListener('resize', handleResize)
      window.removeEventListener('scroll', handleScroll)
    }
  }, [pending, updateBubblePositions])

  return { performerRefs, performersListRef, bubblePositions }
}
