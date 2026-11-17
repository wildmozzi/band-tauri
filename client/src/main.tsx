import './shared/ui/tokens.css'
import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './app/App.tsx'
import { preloadNativeRuntime } from './native/runtimeLoader'
import './app/index.css'

function render() {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  )
}

// 데스크톱 앱은 엔진 런타임(연주 세션·관람 방송, 지연 묶음)을 받은 뒤 그린다 — 합주실이 처음부터 진짜 세션을 가져
// 연주 시작 경로·타이밍이 그대로다. 웹은 받지 않고 바로 그린다(웹 관람 첫 묶음에서 빠진다). 실패해도 그린다(연주 시작만 오류)
void preloadNativeRuntime().finally(render)
