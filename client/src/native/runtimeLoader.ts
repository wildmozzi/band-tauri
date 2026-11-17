/**
 * 엔진 런타임 지연 로드 (웹 관람 묶음 분리).
 *
 * 연주 세션 본체·관람 방송(nativeRuntime)은 데스크톱 앱에서만 쓴다. 웹 관람 첫 묶음에서 빼려고 동적 import하고,
 * 데스크톱 앱은 첫 그리기 전에 preloadNativeRuntime()을 기다려(main.tsx) 합주실이 처음부터 진짜 세션을 갖는다 —
 * 연주 시작 경로·타이밍은 그대로다. 웹(또는 불러오기 실패)은 엔진 없는 세션(idleSession)을 쓴다.
 */
import { isNativeApp } from './engineApi'
import { IdleMixBroadcaster, IdleMixSource, IdlePerformSession, type MixBroadcasterApi, type MixSourceApi, type PerformSessionApi } from './idleSession'
import type { SessionEnv, SignalOut } from './performSession'

type NativeRuntime = typeof import('./nativeRuntime')

let runtime: NativeRuntime | null = null
let loading: Promise<NativeRuntime | null> | null = null

/** 데스크톱 앱이면 엔진 런타임을 불러온다(한 번). 웹이면 아무것도 받지 않고 null */
export function preloadNativeRuntime(): Promise<NativeRuntime | null> {
  if (!isNativeApp()) return Promise.resolve(null)
  loading ??= import('./nativeRuntime').then(
    (m) => (runtime = m),
    (err) => {
      console.error('[Native] 엔진 런타임을 불러오지 못했습니다:', err)
      loading = null // 다음 preload가 다시 시도한다
      return null
    },
  )
  return loading
}

/** 지금 불러와 있는 엔진 런타임(데스크톱 앱에서 preload가 끝났을 때만) */
export function loadedNativeRuntime(): NativeRuntime | null {
  return runtime
}

/** 연주 세션: 데스크톱 앱이면 진짜 세션, 아니면 엔진 없는 세션 */
export function createPerformSession(env: SessionEnv): PerformSessionApi {
  return runtime ? runtime.createPerformSession(env) : new IdlePerformSession()
}

/** 관람 방송(연주 중인 방송 담당 → 관람자). 엔진 런타임이 없으면 열리지 않는 빈 방송 */
export function createMixBridge(
  send: (payload: SignalOut) => void,
  onViewerCountChange: (connected: number) => void,
): { mixSource: MixSourceApi; broadcaster: MixBroadcasterApi } {
  return runtime ? runtime.createMixBridge(send, onViewerCountChange) : { mixSource: new IdleMixSource(), broadcaster: new IdleMixBroadcaster() }
}

/** 테스트 전용: 불러온 런타임을 잊는다 */
export function resetNativeRuntimeForTest(): void {
  runtime = null
  loading = null
}
