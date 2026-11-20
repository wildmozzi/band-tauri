import { createContext, useContext } from 'react'
import { MAX_PERFORMERS } from '../../native/engineApi'
import { ROOM_CAPACITY_MAX } from '../rooms'

export type UserPlan = 'free' | 'standard' | 'pro'

/**
 * 요금제별 한도 (표시용). 실제 동작과 같게 둔다 (P3-12a):
 * 서버·클라이언트 어디에서도 요금제로 막는 기능이 없으므로 세 요금제가 모두 같다.
 * 미구현 기능(클라우드 저장·자동 믹싱·예약 등)은 여기 두지 않는다 — planFeatures.ts의 PLANNED_FEATURES.
 * 요금제 게이트를 새로 걸면 서버에서 강제하고, 그때 여기 값을 나눈다.
 */
export interface FeatureLimit {
  /** 동시 연주자 상한 — 엔진 MAX_PERFORMERS (RoomProvider·sessionLogic이 강제) */
  maxPerformers: number
  /** 방 정원(관람 포함) 상한 — 서버 roomCapacity·DB CHECK(013) */
  maxRoomCapacity: number
  /** 비공개 방·비밀번호 방 — 무료 포함 모두 허용 (백로그 P2-6a 결정) */
  canCreatePrivateRooms: boolean
}

const SAME_FOR_ALL_PLANS: FeatureLimit = {
  maxPerformers: MAX_PERFORMERS,
  maxRoomCapacity: ROOM_CAPACITY_MAX,
  canCreatePrivateRooms: true,
}

export const PLAN_LIMITS: Record<UserPlan, FeatureLimit> = {
  free: SAME_FOR_ALL_PLANS,
  standard: SAME_FOR_ALL_PLANS,
  pro: SAME_FOR_ALL_PLANS,
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
