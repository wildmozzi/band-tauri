/**
 * 미리 받아 둘 수 있는 지연 모듈의 컴포넌트 (웹 첫 묶음 축소).
 *
 * 데스크톱 앱에서만 그리는 화면 조각(연주자 전용 UI 등)을 지연 묶음으로 빼되, 데스크톱 앱은 첫 그리기 전에 preload()를 기다려
 * 그 조각이 처음부터 바로 그려지게 한다(Suspense 폴백·깜빡임 없음 — 받아 둔 모듈의 컴포넌트를 그대로 그린다).
 * 받아 두지 않은 곳(웹, 또는 데스크톱 preload 실패)에서 그리게 되면 그때 받아 그린다(폴백 null).
 *
 * 받기 실패는 그 조각에서 끝난다(#221 리뷰 M-1): 작은 에러 경계가 "불러오지 못했습니다 + 다시 시도"를 보여 주고 앱의 나머지는
 * 그대로 돈다(#216 DeferredPerformSession과 같은 방향). React.lazy는 한 번 거절된 결과를 영구히 기억하므로, 다시 시도는
 * lazy를 새로 만들어 preload()를 다시 부른다(preload도 실패를 기억하지 않는다).
 * 한 인스턴스는 처음 정한 그리기 방식(받아 둔 컴포넌트 / 지연)을 끝까지 쓴다 — 도중에 바꾸면 하위 트리가 리마운트돼
 * 녹음·측정 같은 지역 상태가 초기화된다(#221 리뷰 L-1).
 */
import { Component, lazy, Suspense, useState, type ComponentType, type ErrorInfo, type ReactElement, type ReactNode } from 'react'

type AnyComponent = ComponentType<object>

export const PRELOAD_FAILED_MESSAGE = '이 화면을 불러오지 못했습니다. 다시 시도하고, 계속되면 앱을 다시 시작해주세요.'

/** 지연 조각 하나의 받기 실패를 그 자리에서 잡는 경계 */
class LoadBoundary extends Component<{ name: string; onRetry: () => void; children: ReactNode }, { failed: boolean }> {
  state = { failed: false }

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true }
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error(`[preloadable] ${this.props.name} 불러오기 실패:`, error, info.componentStack)
  }

  render(): ReactNode {
    if (!this.state.failed) return this.props.children
    return (
      <div className="preload-error" role="alert" style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 8, fontSize: '0.85rem' }}>
        <span>{PRELOAD_FAILED_MESSAGE}</span>
        <button type="button" onClick={this.props.onRetry}>
          다시 시도
        </button>
      </div>
    )
  }
}

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

  /** 모듈의 name 컴포넌트를 그리는 컴포넌트. 받아 두었으면 바로, 아니면 지연 로드(실패하면 그 자리에 안내 + 다시 시도) */
  function component<K extends keyof M & string>(name: K): M[K] {
    const makeLazy = () => lazy(() => preload().then((m) => ({ default: m[name] as AnyComponent })))

    function Preloadable(props: object): ReactElement {
      // 처음 그릴 때 정한 방식을 끝까지 쓴다(L-1). 받아 두었으면 그 컴포넌트, 아니면 이 인스턴스의 lazy
      const [loaded] = useState(() => (mod?.[name] as AnyComponent | undefined) ?? null)
      const [attempt, setAttempt] = useState(() => (loaded ? null : { key: 0, Lazy: makeLazy() }))
      if (loaded) {
        const Loaded = loaded
        return <Loaded {...props} />
      }
      const { key, Lazy } = attempt!
      // 다시 시도: 거절을 기억한 lazy를 버리고 새로 만든다(경계도 key로 새로)
      const retry = () => setAttempt({ key: key + 1, Lazy: makeLazy() })
      return (
        <LoadBoundary key={key} name={name} onRetry={retry}>
          <Suspense fallback={null}>
            <Lazy {...props} />
          </Suspense>
        </LoadBoundary>
      )
    }
    Preloadable.displayName = `Preloadable(${name})`
    return Preloadable as unknown as M[K]
  }

  return { preload, component, isLoaded: () => mod !== null }
}
