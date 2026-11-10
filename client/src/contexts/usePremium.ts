import { createContext, useContext } from 'react'

export type UserPlan = 'free' | 'standard' | 'pro'

export interface FeatureLimit {
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

export const PLAN_LIMITS: Record<UserPlan, FeatureLimit> = {
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

export interface PremiumContextType {
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

export const PremiumContext = createContext<PremiumContextType | undefined>(undefined)

export function usePremium() {
  const context = useContext(PremiumContext)
  if (context === undefined) {
    throw new Error('usePremium must be used within a PremiumProvider')
  }
  return context
}
