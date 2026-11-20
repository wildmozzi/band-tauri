import { Suspense, useEffect } from 'react'
import { BrowserRouter, useLocation, useRoutes } from 'react-router-dom'
import { QueryClientProvider } from '@tanstack/react-query'
import { queryClient } from './queryClient'
import { AuthProvider } from '../features/auth'
import { RoomProvider, FloatingMiniPlayer } from '../features/room'
import { AudioSettingsProvider, UpdateNotice } from '../features/settings'
import { PremiumProvider, PremiumModal, usePremium } from '../features/payment'
import { Navigation } from './Navigation'
import { APP_ROUTES } from './routes'
import { pageTitleFor } from './pageTitle'
import { SERVICE_NAME } from '../shared/lib/brand'
import './App.css'
// 결제 CSS 중 결제 화면 밖에서도 매칭되는 규칙만 전역(App.css 뒤 같은 자리). 결제 화면 전용 규칙은 각 화면이 import한다 (P3-2 ③-2)
import '../features/payment/paymentGlobal.css'

/** 라우트가 바뀔 때마다 탭 제목을 'IndigoMuse — <화면>'으로 맞춘다 (P3-12 ①) */
function DocumentTitle() {
  const { pathname } = useLocation()
  useEffect(() => {
    document.title = pageTitleFor(pathname)
  }, [pathname])
  return null
}

function AppContent() {
  const { premiumModal, closePremiumModal } = usePremium()
  // 라우트 정의는 routeTable.ts(경로·제목) + routes.tsx(화면) 한곳 (P3-8)
  const routes = useRoutes(APP_ROUTES)

  return (
    <div className="app">
      <DocumentTitle />
      <Navigation />
      <main className="app-content">
        {/* 지연 로딩 화면(routes.tsx)을 처음 내려받는 동안은 비워 둔다. 라우터 이동은 transition이라 이전 화면을 유지한다 */}
        <Suspense fallback={null}>{routes}</Suspense>
      </main>
      <footer className="app-footer">
        <p>{SERVICE_NAME} · 연주는 데스크톱 앱에서, 관람은 웹에서.</p>
      </footer>

      {/* 미니 플레이어 - 합주실에서 다른 페이지로 이동했을 때 표시 */}
      <FloatingMiniPlayer />

      {/* 데스크톱 자동 업데이트 알림 (P2-1) */}
      <UpdateNotice />

      <PremiumModal
        isOpen={premiumModal.isOpen}
        onClose={closePremiumModal}
        feature={premiumModal.feature}
        requiredPlan={premiumModal.requiredPlan}
      />
    </div>
  )
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <AuthProvider>
          <PremiumProvider>
            <AudioSettingsProvider>
              <RoomProvider>
                <AppContent />
              </RoomProvider>
            </AudioSettingsProvider>
          </PremiumProvider>
        </AuthProvider>
      </BrowserRouter>
    </QueryClientProvider>
  )
}

export default App
