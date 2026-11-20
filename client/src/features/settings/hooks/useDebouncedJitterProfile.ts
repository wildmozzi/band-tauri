import { useCallback, useEffect, useRef } from 'react'
import { engineSetJitterProfile } from '../../../native/engineApi'

export const PROFILE_APPLY_DEBOUNCE_MS = 200

/**
 * 지터 프로필 슬라이더: 드래그 중에는 표시만 바꾸고, 엔진 적용(IPC)은 마지막 값으로 200ms 뒤 한 번만(PR #20 M3).
 * 언마운트하면 대기 중인 적용을 버린다. 반환한 함수로 적용을 예약한다.
 */
export function useDebouncedJitterProfile(apply: (p: number) => Promise<unknown> = engineSetJitterProfile): (p: number) => void {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current)
  }, [])
  return useCallback(
    (p: number) => {
      if (timer.current) clearTimeout(timer.current)
      timer.current = setTimeout(() => {
        timer.current = null
        void apply(p).catch(() => undefined)
      }, PROFILE_APPLY_DEBOUNCE_MS)
    },
    [apply],
  )
}
