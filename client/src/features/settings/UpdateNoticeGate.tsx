// 앱 루트 업데이트 배너의 입구 — 배너·업데이트 본체(native/updater, @tauri-apps/plugin-updater)는 데스크톱에서만 지연 청크로 받는다.
// 웹 관람자는 아무것도 받지 않는다(첫 화면 JS 예산, e2e/bundle-budget). 배너 CSS는 cascade 순서를 지키려 첫 묶음에 둔다.
import { lazy, Suspense } from 'react'
import { isUpdaterAvailable } from '../../native/updaterGate'
import './UpdateNotice.css'

const LazyUpdateNotice = lazy(() => import('./UpdateNotice').then(m => ({ default: m.UpdateNotice })))

/** 앱 루트에 한 번 마운트. 데스크톱이면 배너 묶음을 받아 시작 시 1회 확인한다(UpdateNotice.tsx). */
export function UpdateNotice() {
  if (!isUpdaterAvailable()) return null
  return (
    <Suspense fallback={null}>
      <LazyUpdateNotice />
    </Suspense>
  )
}
