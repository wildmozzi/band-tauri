import { createContext, useContext, useState, ReactNode, useEffect } from 'react'
import { useAuth } from './AuthContext'
import { paymentApi } from '../services/paymentApi'

type UserPlan = 'free' | 'standard' | 'pro'

interface FeatureLimit {
  maxParticipants: number
  canCreatePrivateRooms: boolean
  hasCloudStorage: boolean
  cloudStorageDays: number | null // null = unlimited
  canDownloadFiles: boolean
  downloadLimitDays: number | null // null = unlimited
  hasMixLab: boolean
  mixLabPresets: number | null // null = unlimited
  hasAutoMixing: boolean
  canScheduleRooms: boolean
  hasTeamManagement: boolean
  hasSessionHistory: boolean
  showAds: boolean
  canShareFiles: boolean
  hasAdvancedEffects: boolean
}

const PLAN_LIMITS: Record<UserPlan, FeatureLimit> = {
  free: {
    maxParticipants: 4,
    canCreatePrivateRooms: false,
    hasCloudStorage: false,
    cloudStorageDays: null,
    canDownloadFiles: true, // 로컬만
    downloadLimitDays: null,
    hasMixLab: false,
    mixLabPresets: 0,
    hasAutoMixing: false,
    canScheduleRooms: false,
    hasTeamManagement: false,
    hasSessionHistory: false,
    showAds: true,
    canShareFiles: false,
    hasAdvancedEffects: false,
  },
  standard: {
    maxParticipants: 6,
    canCreatePrivateRooms: true,
    hasCloudStorage: true,
    cloudStorageDays: 30,
    canDownloadFiles: true,
    downloadLimitDays: 7,
    hasMixLab: true,
    mixLabPresets: 2,
    hasAutoMixing: false,
    canScheduleRooms: false,
    hasTeamManagement: false,
    hasSessionHistory: false,
    showAds: false,
    canShareFiles: true,
    hasAdvancedEffects: false,
  },
  pro: {
    maxParticipants: 8,
    canCreatePrivateRooms: true,
    hasCloudStorage: true,
    cloudStorageDays: null, // unlimited
    canDownloadFiles: true,
    downloadLimitDays: null, // unlimited
    hasMixLab: true,
    mixLabPresets: null, // unlimited
    hasAutoMixing: true,
    canScheduleRooms: true,
    hasTeamManagement: true,
    hasSessionHistory: true,
    showAds: false,
    canShareFiles: true,
    hasAdvancedEffects: true,
  },
}

interface PremiumContextType {
  userPlan: UserPlan
  setUserPlan: (plan: UserPlan) => void
  planLimits: FeatureLimit
  checkFeatureAccess: (feature: string, requiredPlan: 'standard' | 'pro') => boolean
  getFeatureLimit: <K extends keyof FeatureLimit>(feature: K) => FeatureLimit[K]
  showPremiumModal: (feature: string, requiredPlan: 'standard' | 'pro') => void
  premiumModal: {
    isOpen: boolean
    feature: string
    requiredPlan: 'standard' | 'pro'
  }
  closePremiumModal: () => void
  isFeatureDisabled: (feature: string, requiredPlan: 'standard' | 'pro') => boolean
}

const PremiumContext = createContext<PremiumContextType | undefined>(undefined)

interface PremiumProviderProps {
  children: ReactNode
}

export function PremiumProvider({ children }: PremiumProviderProps) {
  const { user } = useAuth()
  const [userPlan, setUserPlan] = useState<UserPlan>('free')
  const [premiumModal, setPremiumModal] = useState({
    isOpen: false,
    feature: '',
    requiredPlan: 'standard' as 'standard' | 'pro'
  })

  // 사용자 플랜 로드
  useEffect(() => {
    const loadUserPlan = async () => {
      if (user) {
        // 먼저 localStorage에서 플랜 정보 로드 (빠른 초기화)
        const savedPlan = localStorage.getItem(`userPlan_${user.id}`) as UserPlan
        if (savedPlan && ['free', 'standard', 'pro'].includes(savedPlan)) {
          setUserPlan(savedPlan)
        }

        // 결제 서버가 설정된 경우에만 서버에서 최신 정보 가져오기
        const apiUrl = import.meta.env.VITE_API_URL
        if (apiUrl) {
          try {
            const result = await paymentApi.getUserSubscription(user.id)
            if (result.success && result.user) {
              setUserPlan(result.user.planType)
              localStorage.setItem(`userPlan_${user.id}`, result.user.planType)
            }
          } catch {
            // 서버 연결 실패 시 localStorage 값 유지
          }
        }
      }
    }

    loadUserPlan()
  }, [user])

  // 플랜 변경 시 localStorage에 저장 (테스트용)
  const handleSetUserPlan = (plan: UserPlan) => {
    setUserPlan(plan)
    if (user) {
      localStorage.setItem(`userPlan_${user.id}`, plan)
    }
  }

  const planLimits = PLAN_LIMITS[userPlan]

  const checkFeatureAccess = (_feature: string, requiredPlan: 'standard' | 'pro'): boolean => {
    if (userPlan === 'free') return false
    if (requiredPlan === 'standard') return userPlan === 'standard' || userPlan === 'pro'
    if (requiredPlan === 'pro') return userPlan === 'pro'
    return false
  }

  const getFeatureLimit = <K extends keyof FeatureLimit>(feature: K): FeatureLimit[K] => {
    return planLimits[feature]
  }

  const isFeatureDisabled = (feature: string, requiredPlan: 'standard' | 'pro'): boolean => {
    return !checkFeatureAccess(feature, requiredPlan)
  }

  const showPremiumModal = (feature: string, requiredPlan: 'standard' | 'pro') => {
    setPremiumModal({
      isOpen: true,
      feature,
      requiredPlan
    })
  }

  const closePremiumModal = () => {
    setPremiumModal(prev => ({ ...prev, isOpen: false }))
  }

  return (
    <PremiumContext.Provider value={{
      userPlan,
      setUserPlan: handleSetUserPlan,
      planLimits,
      checkFeatureAccess,
      getFeatureLimit,
      isFeatureDisabled,
      showPremiumModal,
      premiumModal,
      closePremiumModal
    }}>
      {children}
    </PremiumContext.Provider>
  )
}

export function usePremium() {
  const context = useContext(PremiumContext)
  if (context === undefined) {
    throw new Error('usePremium must be used within a PremiumProvider')
  }
  return context
}