import './shared/ui/tokens.css'
import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './app/App.tsx'
import { preloadPerformerUi } from './features/room'
import { preloadSettingsPerformerUi } from './features/settings'
import { isNativeApp } from './native/engineApi'
import { preloadNativeRuntime } from './native/runtimeLoader'
import './app/index.css'

function render() {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  )
}

// 데스크톱 앱은 엔진 런타임(연주 세션·관람 방송)과 연주자 전용 UI(지연 묶음)를 받은 뒤 그린다 — 합주실이 처음부터 진짜 세션과
// 연주자 화면 조각을 가져 연주 시작 경로·타이밍이 그대로다. 웹은 받지 않고 바로 그린다(웹 관람 첫 묶음에서 빠진다).
// 실패해도 그린다(런타임 실패는 연주 시작 때 다시 시도, UI 조각은 그릴 때 다시 받는다)
const ready = isNativeApp()
  ? Promise.allSettled([preloadNativeRuntime(), preloadPerformerUi(), preloadSettingsPerformerUi()]).then((results) => {
      for (const r of results) if (r.status === 'rejected') console.error('[App] 데스크톱 묶음 미리 받기 실패:', r.reason)
    })
  : Promise.resolve()
void ready.finally(render)
