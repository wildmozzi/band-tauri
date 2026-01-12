import { BrowserRouter, Routes, Route } from 'react-router-dom'
import { AuthProvider } from './contexts/AuthContext'
import { RoomProvider } from './contexts/RoomContext'
import { AudioSettingsProvider } from './contexts/AudioSettingsContext'
import { PremiumProvider } from './contexts/PremiumContext'
import { Navigation } from './components/Navigation'
import { FloatingMiniPlayer } from './components/FloatingMiniPlayer'
import { PremiumModal } from './components/PremiumModal'
import { Home } from './pages/Home'
import { Auth } from './pages/Auth'
import { Rooms } from './pages/Rooms'
import { CreateRoom } from './pages/CreateRoom'
import { RoomDetail } from './pages/RoomDetail'
import { Recording } from './pages/Recording'
import { MixLab } from './pages/MixLab'
import { Community } from './pages/Community'
import { CreatePost } from './pages/CreatePost'
import { PostDetail } from './pages/PostDetail'
import { EditPost } from './pages/EditPost'
import { AudioSettingsPage } from './pages/AudioSettingsPage'
import { Pricing } from './pages/Pricing'
import { Payment } from './pages/Payment'
import { PaymentSuccess } from './pages/PaymentSuccess'
import { PaymentFail } from './pages/PaymentFail'
import { Profile } from './pages/Profile'
import { Karaoke } from './pages/Karaoke'
import { usePremium } from './contexts/PremiumContext'
import './App.css'
import './pages/Pricing.css'
import './components/PremiumModal.css'
import './pages/Payment.css'

function AppContent() {
  const { premiumModal, closePremiumModal } = usePremium()

  return (
    <div className="app">
      <Navigation />
      <main className="app-content">
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/auth" element={<Auth />} />
          <Route path="/rooms" element={<Rooms />} />
          <Route path="/rooms/create" element={<CreateRoom />} />
          <Route path="/rooms/:roomId" element={<RoomDetail />} />
          <Route path="/recording" element={<Recording />} />
          <Route path="/mix" element={<MixLab />} />
          <Route path="/community" element={<Community />} />
          <Route path="/community/create" element={<CreatePost />} />
          <Route path="/community/:postId" element={<PostDetail />} />
          <Route path="/community/edit/:postId" element={<EditPost />} />
          <Route path="/karaoke" element={<Karaoke />} />
          <Route path="/settings/audio" element={<AudioSettingsPage />} />
          <Route path="/pricing" element={<Pricing />} />
          <Route path="/payment" element={<Payment />} />
          <Route path="/payment/success" element={<PaymentSuccess />} />
          <Route path="/payment/fail" element={<PaymentFail />} />
          <Route path="/profile" element={<Profile />} />
        </Routes>
      </main>
      <footer className="app-footer">
        <p>BandSpace · Syncroom-inspired 데모. 데스크톱 / 모바일 확장을 준비 중입니다.</p>
      </footer>

      {/* 미니 플레이어 - 합주실에서 다른 페이지로 이동했을 때 표시 */}
      <FloatingMiniPlayer />

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
  )
}

export default App
