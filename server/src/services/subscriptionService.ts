import { PLAN_PRICES, PaymentResponse } from '../types/payment'
import { supabaseAdmin } from '../lib/supabase'
import { ConflictError, StoreUnavailableError } from '../repositories/errors'
import {
  createSupabaseSubscriptionRepository,
  createUnavailableSubscriptionRepository,
  type PaidPlan,
  type SubscriptionRecord,
  type SubscriptionRepository,
} from '../repositories/subscriptionRepository'
import { paymentService } from './paymentService'
import { ServiceError } from './errors'

/** 구독 기간 (일) */
export const SUBSCRIPTION_PERIOD_DAYS = 30

/** 결제대행(토스) 경계. 테스트에서 교체한다 */
export interface PaymentGateway {
  confirmPayment(paymentKey: string, orderId: string, amount: number): Promise<PaymentResponse>
  cancelPayment(paymentKey: string, cancelReason: string): Promise<PaymentResponse>
}

export interface SubscriptionServiceOptions {
  /** DB 기록 재시도 간격(ms). 길이 = 재시도 횟수 */
  retryDelaysMs?: number[]
  now?: () => Date
}

/** GET /subscription 응답의 user 객체 (클라이언트 계약 — 형태 변경 금지) */
export interface UserSubscriptionView {
  id: string
  planType: 'free' | PaidPlan
  subscription: {
    id: string
    status: string
    startDate: Date
    endDate: Date
    amount: number
  } | null
}

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms))

/**
 * 결제/구독 업무 로직.
 * - 금액은 서버 가격표(PLAN_PRICES)로만 결정한다.
 * - 토스 승인 성공 후 DB 기록은 재시도하고, 끝내 실패하면 승인을 취소(보상)한다.
 * - paymentKey/orderId는 DB 유니크 키 → 같은 요청 재전송은 기존 구독을 돌려준다(멱등).
 * - 저장소를 쓸 수 없으면(StoreUnavailableError) 결제를 승인하지 않는다.
 */
export class SubscriptionService {
  private readonly retryDelaysMs: number[]
  private readonly now: () => Date

  constructor(
    private readonly repo: SubscriptionRepository,
    private readonly gateway: PaymentGateway,
    opts: SubscriptionServiceOptions = {},
  ) {
    this.retryDelaysMs = opts.retryDelaysMs ?? [200, 1000]
    this.now = opts.now ?? (() => new Date())
  }

  /** 결제 승인 → 구독 기록. 실패는 ServiceError / StoreUnavailableError로 throw */
  async confirmPayment(userId: string, paymentKey: string, orderId: string, planType: PaidPlan): Promise<SubscriptionRecord> {
    const amount = PLAN_PRICES[planType]

    // 1) 멱등성 확인 (겸 저장소 사용 가능 확인: 테이블이 없으면 여기서 503 — 승인 전에 막는다)
    const existing = await this.findRecorded(userId, paymentKey, orderId, planType)
    if (existing.kind === 'same') {
      console.log(`[PAYMENT] 중복 승인 요청 — 기존 구독 반환 (orderId=${orderId})`)
      return existing.subscription
    }
    if (existing.kind === 'conflict') {
      throw new ServiceError(409, { success: false, error: '이미 처리된 결제입니다.' })
    }

    // 2) 토스 승인 (토스가 실제 주문 금액과 대조하므로 불일치하면 승인 실패)
    const paymentResult = await this.gateway.confirmPayment(paymentKey, orderId, amount)
    if (!paymentResult.success) {
      throw new ServiceError(400, { ...paymentResult, success: false })
    }

    // 3) DB 기록 (결제+구독 한 트랜잭션). 일시 오류는 재시도
    let lastError: unknown
    for (let attempt = 0; ; attempt++) {
      try {
        return await this.repo.recordConfirmedPayment({
          userId, orderId, paymentKey, planType, amount, periodDays: SUBSCRIPTION_PERIOD_DAYS,
        })
      } catch (err) {
        lastError = err
        const retryable = !(err instanceof ConflictError) && !(err instanceof StoreUnavailableError)
        if (!retryable || attempt >= this.retryDelaysMs.length) break
        console.warn(`[PAYMENT] 결제 기록 실패, 재시도 ${attempt + 1}/${this.retryDelaysMs.length} (orderId=${orderId}):`, errMessage(err))
        await sleep(this.retryDelaysMs[attempt])
      }
    }

    const ctx = `userId=${userId} orderId=${orderId} paymentKey=${paymentKey} amount=${amount} plan=${planType}`

    // 동시 중복 요청: 다른 요청이 같은 결제를 먼저 기록했으면 그 결과를 쓴다.
    // 같은 paymentKey가 이미 기록돼 있으면 취소하면 그 (유효한) 결제가 환불되므로 절대 보상 취소하지 않는다.
    if (lastError instanceof ConflictError) {
      const again = await this.findRecorded(userId, paymentKey, orderId, planType).catch(() => null)
      if (again?.kind === 'same') return again.subscription
      if (!again || (again.kind === 'conflict' && again.paymentKeyRecorded)) {
        console.error(`[PAYMENT][CRITICAL] 결제 키 충돌 — 자동 취소하지 않음, 수동 확인 필요 (${ctx}):`, errMessage(lastError))
        throw new ServiceError(409, { success: false, error: '이미 처리된 결제입니다.' })
      }
    }

    // 4) 보상: 승인을 취소해 "돈은 빠졌는데 구독은 없는" 상태를 막는다
    console.error(`[PAYMENT][COMPENSATE] 승인 후 DB 기록 실패 → 결제 취소 시도 (${ctx}):`, errMessage(lastError))
    const cancel = await this.gateway.cancelPayment(paymentKey, '결제 기록 실패로 자동 취소').catch(
      (e): PaymentResponse => ({ success: false, error: errMessage(e) }),
    )
    if (!cancel.success) {
      console.error(`[PAYMENT][CRITICAL] 자동 취소 실패 — 수동 조치 필요 (${ctx}): ${cancel.error}`)
      throw new ServiceError(500, { success: false, error: '결제 처리 중 오류가 발생했습니다. 고객센터에 문의해 주세요.' })
    }
    console.error(`[PAYMENT][COMPENSATE] 자동 취소 완료 (${ctx})`)
    throw new ServiceError(lastError instanceof StoreUnavailableError ? 503 : 500, {
      success: false,
      error: '결제 기록에 실패해 결제를 자동 취소했습니다. 잠시 후 다시 시도해 주세요.',
    })
  }

  /**
   * 이미 기록된 결제 확인.
   * - same: 같은 사용자·paymentKey·orderId·플랜의 확정 결제 → 기존 구독
   * - conflict: 키가 겹치지만 다른 결제 (paymentKeyRecorded: 같은 paymentKey가 기록돼 있음)
   * - none
   */
  private async findRecorded(userId: string, paymentKey: string, orderId: string, planType: PaidPlan): Promise<
    | { kind: 'none' }
    | { kind: 'same'; subscription: SubscriptionRecord }
    | { kind: 'conflict'; paymentKeyRecorded: boolean }
  > {
    const existing = await this.repo.findPaymentsByKeyOrOrder(paymentKey, orderId)
    if (existing.length === 0) return { kind: 'none' }
    const same = existing.length === 1 ? existing[0] : null
    if (
      same && same.userId === userId && same.paymentKey === paymentKey && same.orderId === orderId &&
      same.planType === planType && same.status === 'confirmed' && same.subscriptionId
    ) {
      const sub = await this.repo.findSubscriptionById(same.subscriptionId)
      if (sub) return { kind: 'same', subscription: sub }
    }
    return { kind: 'conflict', paymentKeyRecorded: existing.some(p => p.paymentKey === paymentKey) }
  }

  /** 본인의 활성 구독 결제 취소 */
  async cancelPayment(userId: string, paymentKey: string | undefined, reason: string): Promise<void> {
    const subscription = await this.repo.findActiveSubscription(userId, this.now())
    if (!subscription || !subscription.paymentKey) {
      throw new ServiceError(404, { success: false, error: '활성 구독을 찾을 수 없습니다.' })
    }
    if (paymentKey !== undefined && paymentKey !== subscription.paymentKey) {
      throw new ServiceError(403, { success: false, error: '본인의 결제만 취소할 수 있습니다.' })
    }

    const cancelResult = await this.gateway.cancelPayment(subscription.paymentKey, reason)
    if (!cancelResult.success) {
      throw new ServiceError(400, { ...cancelResult, success: false })
    }

    let lastError: unknown
    for (let attempt = 0; ; attempt++) {
      try {
        if (await this.repo.cancelSubscription(subscription.id, { paymentCancelled: true, reason })) return
        throw new ServiceError(400, { success: false, error: '구독 취소에 실패했습니다.' })
      } catch (err) {
        if (err instanceof ServiceError) throw err
        lastError = err
        if (err instanceof StoreUnavailableError || attempt >= this.retryDelaysMs.length) break
        await sleep(this.retryDelaysMs[attempt])
      }
    }
    // 토스 취소는 이미 됐다 → 환불은 정상. DB만 수동 반영하면 된다
    console.error(
      `[PAYMENT][RECONCILE] 토스 취소 성공, DB 반영 실패 — 수동 반영 필요 (userId=${userId} subscriptionId=${subscription.id} paymentKey=${subscription.paymentKey}):`,
      errMessage(lastError),
    )
    throw new ServiceError(500, { success: false, error: '구독 취소에 실패했습니다.' })
  }

  /** 현재 플랜과 활성 구독 */
  async getUserSubscription(userId: string): Promise<UserSubscriptionView> {
    const sub = await this.repo.findActiveSubscription(userId, this.now())
    if (!sub) return { id: userId, planType: 'free', subscription: null }
    return {
      id: userId,
      planType: sub.planType,
      subscription: {
        id: sub.id,
        status: sub.status,
        startDate: sub.startDate,
        endDate: sub.endDate,
        amount: sub.amount,
      },
    }
  }

  /** 무료 플랜으로 변경 = 활성 구독 해지 (환불 없음 — 기존 동작) */
  async downgradeToFree(userId: string): Promise<void> {
    const sub = await this.repo.findActiveSubscription(userId, this.now())
    if (sub) await this.repo.cancelSubscription(sub.id, { paymentCancelled: false, reason: '무료 플랜으로 변경' })
  }

  /** 기간이 지난 구독을 expired로 (조회는 end_date로 거르므로 표시용 정리) */
  async expireDueSubscriptions(): Promise<number> {
    return this.repo.expireDueSubscriptions(this.now())
  }
}

function errMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

let defaultService: SubscriptionService | null = null

/** 운영 기본 인스턴스: Supabase 미설정이면 모든 결제 API가 503 (메모리 폴백 없음) */
export function getDefaultSubscriptionService(): SubscriptionService {
  if (!defaultService) {
    const repo = supabaseAdmin
      ? createSupabaseSubscriptionRepository(supabaseAdmin)
      : createUnavailableSubscriptionRepository()
    defaultService = new SubscriptionService(repo, paymentService)
  }
  return defaultService
}

/** 만료 구독 정리 (1시간마다). index.ts에서 호출 */
export function scheduleSubscriptionExpiry(service: SubscriptionService = getDefaultSubscriptionService()) {
  setInterval(() => {
    service.expireDueSubscriptions().then(
      (n) => { if (n > 0) console.log(`[PAYMENT] 만료 구독 ${n}건 처리`) },
      (err) => console.error('[PAYMENT] 만료 구독 처리 실패:', errMessage(err)),
    )
  }, 60 * 60 * 1000).unref()
}
