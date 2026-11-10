import { PLAN_PRICES, PaymentResponse, TossPaymentLookup } from '../types/payment'
import { supabaseAdmin } from '../lib/supabase'
import { ConflictError, StoreUnavailableError } from '../repositories/errors'
import {
  createSupabaseSubscriptionRepository,
  createUnavailableSubscriptionRepository,
  type PaidPlan,
  type PaymentIntent,
  type SubscriptionRecord,
  type SubscriptionRepository,
} from '../repositories/subscriptionRepository'
import { paymentService } from './paymentService'
import { ServiceError } from './errors'

/** 구독 기간 (일) */
export const SUBSCRIPTION_PERIOD_DAYS = 30

/** 결제대행(토스) 경계. 테스트에서 교체한다 */
export interface PaymentGateway {
  /** outcome이 없으면 success로 판단 (approved/rejected) */
  confirmPayment(paymentKey: string, orderId: string, amount: number): Promise<PaymentResponse>
  cancelPayment(paymentKey: string, cancelReason: string): Promise<PaymentResponse>
  /** 결제 상태 조회. 조회 자체가 실패하면 throw */
  lookupPayment(paymentKey: string): Promise<TossPaymentLookup>
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

export interface ReconcileResult {
  recorded: number
  discarded: number
  unresolved: number
}

/** 토스 조회 결과 판정: approved(DONE) / not-approved(확정 실패·없음) / unknown(진행 중·조회 실패) */
type Verdict = 'approved' | 'not-approved' | 'unknown'

const NOT_APPROVED_STATUSES = new Set(['CANCELED', 'PARTIAL_CANCELED', 'ABORTED', 'EXPIRED'])

/** 이 시간보다 오래된 pending만 재조정한다 (진행 중인 요청과 겹치지 않게: 토스 타임아웃 + 재시도보다 길게) */
export const RECONCILE_MIN_AGE_MS = 2 * 60_000
/** 이보다 오래 미해결이면 CRITICAL 로그 */
const RECONCILE_ALERT_AGE_MS = 60 * 60_000

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms))

const PENDING_MESSAGE = '결제 결과를 확인하고 있습니다. 잠시 후 구독 정보를 다시 확인해 주세요.'

/**
 * 결제/구독 업무 로직.
 * - 금액은 서버 가격표(PLAN_PRICES)로만 결정한다.
 * - 토스 승인 요청 전에 payments에 pending 행을 남긴다 → 승인 결과를 모르는 채 끊겨도 재조정 작업이 복구한다.
 * - 승인 결과가 불명(타임아웃·응답 유실·ALREADY_PROCESSED 등)이면 토스 조회로 실제 상태를 확인한다.
 * - 승인 후 DB 기록은 재시도하고, 기록 여부를 다시 확인한 뒤에만 보상 취소한다.
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
    const intent: PaymentIntent = { userId, paymentKey, orderId, planType, amount: PLAN_PRICES[planType] }

    // 1) 멱등성 확인 + pending 행 (겸 저장소 확인: 테이블이 없으면 여기서 503 — 승인 전에 막는다)
    let existing = await this.findRecorded(intent)
    if (existing.kind === 'none') {
      try {
        await this.repo.createPendingPayment(intent)
      } catch (err) {
        if (!(err instanceof ConflictError)) throw err
        existing = await this.findRecorded(intent) // 동시 요청이 먼저 만들었다
      }
    }
    if (existing.kind === 'same') {
      console.log(`[PAYMENT] 중복 승인 요청 — 기존 구독 반환 (orderId=${orderId})`)
      return this.activeOrConflict(existing.subscription)
    }
    if (existing.kind === 'conflict') {
      throw new ServiceError(409, { success: false, error: '이미 처리된 결제입니다.' })
    }
    // none(방금 pending 생성) 또는 pending(이전 요청의 응답 유실 후 재시도)

    // 2) 토스 승인 (토스가 실제 주문 금액과 대조하므로 불일치하면 승인 실패)
    const result = await this.gateway.confirmPayment(paymentKey, orderId, intent.amount)
    const outcome = result.outcome ?? (result.success ? 'approved' : 'rejected')

    if (outcome === 'rejected') {
      await this.discardPending(orderId)
      throw new ServiceError(400, { success: false, error: result.error ?? '결제 승인에 실패했습니다.' })
    }
    if (outcome === 'unknown') {
      // 응답 유실·타임아웃·ALREADY_PROCESSED: 실제 상태를 조회로 확인
      const verdict = await this.verifyWithToss(intent)
      if (verdict === 'not-approved') {
        await this.discardPending(orderId)
        throw new ServiceError(400, { success: false, error: result.error ?? '결제 승인에 실패했습니다.' })
      }
      if (verdict === 'unknown') {
        // pending 행이 남아 있으므로 재조정 작업이 마무리한다. 400을 주면 클라이언트가 실패로 확정하므로 503
        console.error(`[PAYMENT][CRITICAL] 승인 결과 불명 — 재조정 대기 (${ctxOf(intent)}): ${result.code ?? ''} ${result.error ?? ''}`)
        throw new ServiceError(503, { success: false, error: PENDING_MESSAGE })
      }
      console.warn(`[PAYMENT] 승인 응답 불명(${result.code ?? 'no-response'}) → 조회 결과 DONE, 기록 진행 (orderId=${orderId})`)
    }

    // 3) DB 기록
    return this.recordApproved(intent)
  }

  /** 승인된 결제를 기록. 재시도 → 재확인 → (필요할 때만) 보상 취소 */
  private async recordApproved(intent: PaymentIntent): Promise<SubscriptionRecord> {
    const ctx = ctxOf(intent)
    let lastError: unknown
    for (let attempt = 0; ; attempt++) {
      try {
        return await this.repo.recordConfirmedPayment({ ...intent, periodDays: SUBSCRIPTION_PERIOD_DAYS })
      } catch (err) {
        lastError = err
        const retryable = !(err instanceof ConflictError) && !(err instanceof StoreUnavailableError)
        if (!retryable || attempt >= this.retryDelaysMs.length) break
        console.warn(`[PAYMENT] 결제 기록 실패, 재시도 ${attempt + 1}/${this.retryDelaysMs.length} (orderId=${intent.orderId}):`, errMessage(lastError))
        await sleep(this.retryDelaysMs[attempt])
      }
    }

    // 보상 전에 반드시 다시 확인: 커밋은 됐는데 응답만 유실됐을 수 있다 (M1)
    const again = await this.findRecorded(intent).catch(() => null)
    if (again?.kind === 'same') return again.subscription

    // DB를 확인할 수 없거나, 우리 pending 행이 그대로 있고 일시 오류였다 → 환불하지 않고 재조정 작업에 맡긴다.
    // (승인은 확정이므로 DB가 돌아오면 재조정이 구독을 기록한다)
    if (again === null || (again.kind === 'pending' && !(lastError instanceof ConflictError))) {
      console.error(`[PAYMENT][RECONCILE] 승인됨, DB 기록 실패 — 재조정 작업이 기록 예정 (${ctx}):`, errMessage(lastError))
      throw new ServiceError(503, { success: false, error: PENDING_MESSAGE })
    }

    // 같은 paymentKey가 (다른 주문/사용자로) 이미 기록돼 있으면 취소하면 그 결제가 환불된다 → 절대 취소하지 않음
    if (again.kind === 'conflict' && again.paymentKeyRecorded) {
      console.error(`[PAYMENT][CRITICAL] 결제 키 충돌 — 자동 취소하지 않음, 수동 확인 필요 (${ctx}):`, errMessage(lastError))
      throw new ServiceError(409, { success: false, error: '이미 처리된 결제입니다.' })
    }

    // 보상: 승인을 취소해 "돈은 빠졌는데 구독은 없는" 상태를 막는다
    console.error(`[PAYMENT][COMPENSATE] 승인 후 DB 기록 실패 → 결제 취소 시도 (${ctx}):`, errMessage(lastError))
    const cancel = await this.gateway.cancelPayment(intent.paymentKey, '결제 기록 실패로 자동 취소').catch(
      (e): PaymentResponse => ({ success: false, error: errMessage(e) }),
    )
    if (!cancel.success) {
      console.error(`[PAYMENT][CRITICAL] 자동 취소 실패 — 수동 조치 필요 (${ctx}): ${cancel.error}`)
      throw new ServiceError(500, { success: false, error: '결제 처리 중 오류가 발생했습니다. 고객센터에 문의해 주세요.' })
    }
    console.error(`[PAYMENT][COMPENSATE] 자동 취소 완료 (${ctx})`)
    await this.discardPending(intent.orderId)
    throw new ServiceError(lastError instanceof StoreUnavailableError ? 503 : 500, {
      success: false,
      error: '결제 기록에 실패해 결제를 자동 취소했습니다. 잠시 후 다시 시도해 주세요.',
    })
  }

  /** 토스 조회로 승인 여부 판정 */
  private async verifyWithToss(intent: PaymentIntent): Promise<Verdict> {
    let look: TossPaymentLookup
    try {
      look = await this.gateway.lookupPayment(intent.paymentKey)
    } catch (err) {
      console.error(`[PAYMENT] 결제 조회 실패 (orderId=${intent.orderId}):`, errMessage(err))
      return 'unknown'
    }
    if (!look.found) return 'not-approved'
    const p = look.payment
    if (p.orderId !== intent.orderId || p.totalAmount !== intent.amount) {
      // 다른 주문의 결제이거나 금액이 다르다 → 이 주문의 승인으로 인정하지 않음
      console.error(`[PAYMENT][CRITICAL] 조회 결과 불일치 (${ctxOf(intent)}): toss orderId=${p.orderId} amount=${p.totalAmount} status=${p.status}`)
      return 'not-approved'
    }
    if (p.status === 'DONE') return 'approved'
    if (NOT_APPROVED_STATUSES.has(p.status)) return 'not-approved'
    return 'unknown' // READY / IN_PROGRESS / WAITING_FOR_DEPOSIT
  }

  /** pending 행 정리 (실패는 로그만 — 재조정 작업이 다시 판정한다) */
  private async discardPending(orderId: string) {
    try {
      await this.repo.deletePendingPayment(orderId)
    } catch (err) {
      console.error(`[PAYMENT] pending 정리 실패 (orderId=${orderId}):`, errMessage(err))
    }
  }

  /**
   * 미확정(pending) 결제 재조정: 서버 기동 시·주기적으로 실행.
   * 토스 조회 → DONE이면 구독 기록, 확정 실패면 pending 삭제, 모르면 다음 회차.
   */
  async reconcilePendingPayments(minAgeMs = RECONCILE_MIN_AGE_MS): Promise<ReconcileResult> {
    const now = this.now()
    const rows = await this.repo.listPendingPayments(new Date(now.getTime() - minAgeMs), 100)
    const res: ReconcileResult = { recorded: 0, discarded: 0, unresolved: 0 }
    for (const row of rows) {
      const intent: PaymentIntent = { userId: row.userId, paymentKey: row.paymentKey, orderId: row.orderId, planType: row.planType, amount: row.amount }
      const verdict = await this.verifyWithToss(intent)
      try {
        if (verdict === 'approved') {
          await this.repo.recordConfirmedPayment({ ...intent, periodDays: SUBSCRIPTION_PERIOD_DAYS })
          console.log(`[PAYMENT][RECONCILE] 승인 확인 → 구독 기록 (${ctxOf(intent)})`)
          res.recorded++
          continue
        }
        if (verdict === 'not-approved') {
          await this.repo.deletePendingPayment(row.orderId)
          console.log(`[PAYMENT][RECONCILE] 미승인 → pending 삭제 (orderId=${row.orderId})`)
          res.discarded++
          continue
        }
      } catch (err) {
        console.error(`[PAYMENT][RECONCILE] 처리 실패 (${ctxOf(intent)}):`, errMessage(err))
      }
      res.unresolved++
      if (now.getTime() - row.createdAt.getTime() > RECONCILE_ALERT_AGE_MS) {
        console.error(`[PAYMENT][CRITICAL] pending 결제가 오래 미해결 — 수동 확인 필요 (${ctxOf(intent)}, since ${row.createdAt.toISOString()})`)
      }
    }
    return res
  }

  /**
   * 이미 기록된 결제 확인 (orderId·paymentKey 기준).
   * - same: 같은 사용자·paymentKey·orderId·플랜의 확정 결제 → 기존 구독
   * - pending: 같은 조합의 미확정 결제 (이전 요청이 결과를 모른 채 끝남)
   * - conflict: 키가 겹치지만 다른 결제 (paymentKeyRecorded: 같은 paymentKey가 기록돼 있음)
   * - none
   */
  private async findRecorded(intent: PaymentIntent): Promise<
    | { kind: 'none' }
    | { kind: 'same'; subscription: SubscriptionRecord }
    | { kind: 'pending' }
    | { kind: 'conflict'; paymentKeyRecorded: boolean }
  > {
    const existing = await this.repo.findPaymentsByKeyOrOrder(intent.paymentKey, intent.orderId)
    if (existing.length === 0) return { kind: 'none' }
    const one = existing.length === 1 ? existing[0] : null
    const sameTuple = !!one && one.userId === intent.userId && one.paymentKey === intent.paymentKey &&
      one.orderId === intent.orderId && one.planType === intent.planType && one.amount === intent.amount
    if (sameTuple && one!.status === 'pending') return { kind: 'pending' }
    if (sameTuple && one!.status === 'confirmed' && one!.subscriptionId) {
      const sub = await this.repo.findSubscriptionById(one!.subscriptionId)
      if (sub) return { kind: 'same', subscription: sub }
    }
    return { kind: 'conflict', paymentKeyRecorded: existing.some(p => p.paymentKey === intent.paymentKey) }
  }

  /** 멱등 재요청인데 구독이 이미 해지/만료됐으면 성공으로 보이지 않게 409 (L2) */
  private activeOrConflict(sub: SubscriptionRecord): SubscriptionRecord {
    if (sub.status === 'active' && sub.endDate > this.now()) return sub
    throw new ServiceError(409, { success: false, error: '이미 처리된 결제입니다. 해당 구독은 현재 활성 상태가 아닙니다.' })
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
        // 재시도 회차에서 false: 앞선 시도가 커밋됐는데 응답만 유실됐을 수 있다 (L1)
        if (attempt > 0) {
          const now = await this.repo.findSubscriptionById(subscription.id)
          if (now?.status === 'cancelled') return
        }
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

function ctxOf(i: PaymentIntent): string {
  return `userId=${i.userId} orderId=${i.orderId} paymentKey=${i.paymentKey} amount=${i.amount} plan=${i.planType}`
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

const RECONCILE_INTERVAL_MS = 5 * 60_000
const EXPIRY_INTERVAL_MS = 60 * 60_000

/**
 * 주기 작업 (index.ts에서 호출):
 * - 미확정 결제 재조정: 기동 직후 1회 + 5분마다 (서버 재시작으로 끊긴 승인 복구)
 * - 만료 구독 정리: 1시간마다
 */
export function scheduleSubscriptionJobs(service: SubscriptionService = getDefaultSubscriptionService()) {
  const reconcile = () => service.reconcilePendingPayments().then(
    (r) => { if (r.recorded || r.discarded || r.unresolved) console.log(`[PAYMENT][RECONCILE] recorded=${r.recorded} discarded=${r.discarded} unresolved=${r.unresolved}`) },
    (err) => console.error('[PAYMENT][RECONCILE] 실행 실패:', errMessage(err)),
  )
  setTimeout(reconcile, 10_000).unref()
  setInterval(reconcile, RECONCILE_INTERVAL_MS).unref()
  setInterval(() => {
    service.expireDueSubscriptions().then(
      (n) => { if (n > 0) console.log(`[PAYMENT] 만료 구독 ${n}건 처리`) },
      (err) => console.error('[PAYMENT] 만료 구독 처리 실패:', errMessage(err)),
    )
  }, EXPIRY_INTERVAL_MS).unref()
}
