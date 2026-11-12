import { useCallback, useRef, useSyncExternalStore } from 'react'
import type { RoomSessionStore, RoomSnapshot } from './roomSessionStore'

/** 얕은 비교: 같은 키에 같은 참조면 같다 (배열·객체 모두) */
export function shallowEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false
  const ka = Object.keys(a)
  const kb = Object.keys(b)
  if (ka.length !== kb.length) return false
  return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && Object.is((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]))
}

/**
 * 스토어에서 조각 하나를 구독한다. 선택 결과가 얕은 비교로 같으면 이전 결과(같은 참조)를 돌려줘서 다시 그리지 않는다.
 * 선택자는 스냅샷만 읽는 순수 함수여야 한다. 캐시 키가 (스냅샷, 선택자 참조)라서 선택자가 바뀌면 같은 스냅샷이라도
 * 다시 고른다. 렌더마다 새로 만드는 인라인 선택자도 맞게 동작하지만 렌더마다 다시 고르므로, 모듈 수준 함수나
 * useMemo/useCallback으로 고정한 함수를 권장한다.
 */
export function useRoomSelector<T>(store: RoomSessionStore, selector: (s: RoomSnapshot) => T): T {
  const selectorRef = useRef(selector)
  selectorRef.current = selector
  const cache = useRef<{ snap: RoomSnapshot; selector: (s: RoomSnapshot) => T; value: T } | null>(null)
  const getSelection = useCallback((): T => {
    const snap = store.getSnapshot()
    const sel = selectorRef.current
    const c = cache.current
    if (c && c.snap === snap && c.selector === sel) return c.value
    const next = sel(snap)
    const value = c && shallowEqual(c.value, next) ? c.value : next
    cache.current = { snap, selector: sel, value }
    return value
  }, [store])
  return useSyncExternalStore(store.subscribe, getSelection, getSelection)
}
