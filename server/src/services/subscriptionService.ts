import { Subscription, User, UserPlan } from '../types/payment'

// 간단한 메모리 저장소 (실제 구현에서는 데이터베이스 사용)
class SubscriptionService {
  private subscriptions = new Map<string, Subscription>()
  private users = new Map<string, User>()

  // 사용자 생성/업데이트
  createOrUpdateUser(userData: Partial<User> & { id: string }): User {
    const existingUser = this.users.get(userData.id)
    const user: User = {
      id: userData.id,
      email: userData.email || existingUser?.email || '',
      nickname: userData.nickname || existingUser?.nickname || `User${userData.id.slice(0, 4)}`,
      planType: userData.planType || existingUser?.planType || 'free',
      subscriptionId: userData.subscriptionId || existingUser?.subscriptionId,
      createdAt: existingUser?.createdAt || new Date(),
      updatedAt: new Date()
    }
    
    this.users.set(user.id, user)
    return user
  }

  // 사용자 조회
  getUser(userId: string): User | null {
    return this.users.get(userId) || null
  }

  // 구독 생성
  createSubscription(
    userId: string, 
    planType: UserPlan, 
    paymentKey: string, 
    amount: number
  ): Subscription {
    const subscriptionId = `sub_${userId}_${Date.now()}`
    const startDate = new Date()
    const endDate = new Date(startDate.getTime() + 30 * 24 * 60 * 60 * 1000) // 30일 후

    const subscription: Subscription = {
      id: subscriptionId,
      userId,
      planType,
      status: 'active',
      startDate,
      endDate,
      paymentKey,
      amount,
      createdAt: new Date(),
      updatedAt: new Date()
    }

    this.subscriptions.set(subscriptionId, subscription)

    // 사용자 플랜 업데이트
    this.createOrUpdateUser({
      id: userId,
      planType,
      subscriptionId
    })

    return subscription
  }

  // 구독 조회
  getSubscription(subscriptionId: string): Subscription | null {
    return this.subscriptions.get(subscriptionId) || null
  }

  // 사용자의 활성 구독 조회
  getUserActiveSubscription(userId: string): Subscription | null {
    for (const subscription of this.subscriptions.values()) {
      if (
        subscription.userId === userId && 
        subscription.status === 'active' &&
        subscription.endDate > new Date()
      ) {
        return subscription
      }
    }
    return null
  }

  // 구독 취소
  cancelSubscription(subscriptionId: string): boolean {
    const subscription = this.subscriptions.get(subscriptionId)
    if (!subscription) return false

    subscription.status = 'cancelled'
    subscription.updatedAt = new Date()

    // 사용자 플랜을 무료로 변경
    this.createOrUpdateUser({
      id: subscription.userId,
      planType: 'free',
      subscriptionId: undefined
    })

    return true
  }

  // 만료된 구독 처리
  processExpiredSubscriptions(): void {
    const now = new Date()
    for (const subscription of this.subscriptions.values()) {
      if (
        subscription.status === 'active' && 
        subscription.endDate <= now
      ) {
        subscription.status = 'expired'
        subscription.updatedAt = now

        // 사용자 플랜을 무료로 변경
        this.createOrUpdateUser({
          id: subscription.userId,
          planType: 'free',
          subscriptionId: undefined
        })
      }
    }
  }

  // 구독 갱신 (자동결제)
  renewSubscription(subscriptionId: string, paymentKey: string): boolean {
    const subscription = this.subscriptions.get(subscriptionId)
    if (!subscription || subscription.status !== 'active') return false

    // 새로운 종료일 설정 (현재 종료일에서 30일 추가)
    subscription.endDate = new Date(subscription.endDate.getTime() + 30 * 24 * 60 * 60 * 1000)
    subscription.paymentKey = paymentKey
    subscription.updatedAt = new Date()

    return true
  }

  // 모든 사용자 구독 목록 (관리자용)
  getAllSubscriptions(): Subscription[] {
    return Array.from(this.subscriptions.values())
  }

  // 사용자 플랜 변경
  changeUserPlan(userId: string, newPlan: UserPlan): boolean {
    const user = this.getUser(userId)
    if (!user) return false

    // 기존 구독이 있으면 취소
    if (user.subscriptionId) {
      this.cancelSubscription(user.subscriptionId)
    }

    // 새 플랜으로 업데이트
    this.createOrUpdateUser({
      id: userId,
      planType: newPlan,
      subscriptionId: undefined
    })

    return true
  }
}

export const subscriptionService = new SubscriptionService()

// 주기적으로 만료된 구독 처리 (1시간마다)
setInterval(() => {
  subscriptionService.processExpiredSubscriptions()
}, 60 * 60 * 1000)