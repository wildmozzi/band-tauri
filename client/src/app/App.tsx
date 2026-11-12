import { useEffect } from 'react'
import { BrowserRouter, useLocation, useRoutes } from 'react-router-dom'
import { QueryClientProvider } from '@tanstack/react-query'
import { queryClient } from './lib/queryClient'
import { AuthProvider } from './contexts/AuthContext'
import { RoomProvider } from './contexts/RoomContext'
import { AudioSettingsProvider } from './contexts/AudioSettingsContext'
import { PremiumProvider } from './contexts/PremiumContext'
import { Navigation } from './components/Navigation'
import { FloatingMiniPlayer } from './components/FloatingMiniPlayer'
import { PremiumModal } from './components/PremiumModal'
import { UpdateNotice } from './components/UpdateNotice'
import { APP_ROUTES } from './routes'
import { pageTitleFor } from './lib/pageTitle'
import { SERVICE_NAME } from './lib/brand'
import { usePremium } from './contexts/usePremium'
import './App.css'
import './pages/Pricing.css'
import './components/PremiumModal.css'
import './pages/Payment.css'

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
        {routes}
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
