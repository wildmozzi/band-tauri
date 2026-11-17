/**
 * 미리 받아 둘 수 있는 지연 모듈의 컴포넌트 (웹 첫 묶음 축소).
 *
 * 데스크톱 앱에서만 그리는 화면 조각(연주자 전용 UI 등)을 지연 묶음으로 빼되, 데스크톱 앱은 첫 그리기 전에 preload()를 기다려
 * 그 조각이 처음부터 바로 그려지게 한다(Suspense 폴백·깜빡임 없음 — 받아 둔 모듈의 컴포넌트를 그대로 그린다).
 * 받아 두지 않은 곳(웹)에서 그리게 되면 그때 React.lazy로 받아 그린다(폴백 null).
 */
import { lazy, Suspense, type ComponentType, type ReactElement } from 'react'

type AnyComponent = ComponentType<object>

export function preloadableModule<M extends Record<string, unknown>>(load: () => Promise<M>) {
  let mod: M | null = null
  let loading: Promise<M> | null = null

  /** 모듈을 받는다(한 번). 실패하면 다음 호출이 다시 시도한다 */
  const preload = (): Promise<M> => {
    loading ??= load().then(
      (m) => (mod = m),
      (err) => {
        loading = null
        throw err
      },
    )
    return loading
  }

  /** 모듈의 name 컴포넌트를 그리는 컴포넌트. 받아 두었으면 바로, 아니면 지연 로드 */
  function component<K extends keyof M & string>(name: K): M[K] {
    const Lazy = lazy(() => preload().then((m) => ({ default: m[name] as AnyComponent })))
    function Preloadable(props: object): ReactElement {
      const Loaded = mod?.[name] as AnyComponent | undefined
      const Target = Loaded ?? Lazy
      const el = <Target {...props} />
      return Loaded ? el : <Suspense fallback={null}>{el}</Suspense>
    }
    Preloadable.displayName = `Preloadable(${name})`
    return Preloadable as unknown as M[K]
  }

  return { preload, component, isLoaded: () => mod !== null }
}
