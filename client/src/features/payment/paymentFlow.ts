/**
 * 결제 승인 흐름 (P3-5b) — UI와 분리한 순수 로직 (vitest로 검증).
 *
 * 1) confirm 성공 → 완료
 * 2) confirm 결과 미확정(503·게이트웨이 오류·응답 유실) → '결제 확인 중':
 *    GET /api/payment/orders/:orderId 를 지수 백오프로 조회해 최종 결과(paid/failed/...)를 기다린다.
 *    진행 상태는 storage에 남겨 새로고침해도 confirm을 다시 보내지 않고 조회를 이어간다.
 * 3) 그 밖의 실패(400·404·409·410 등)는 확정 실패
 */

export type PaidPlan = 'standard' | 'pro'

/** 서버 주문 상태 (server/src/services/paymentOrderService.ts의 OrderStatus와 같다) */
export type OrderStatus = 'created' | 'processing' | 'paid' | 'failed' | 'cancelled' | 'expired'

export interface ConfirmParams {
  paymentKey: string
  orderId: string
  amount: number
  planType: PaidPlan
}

export interface ConfirmOutcome {
  /** HTTP 상태. 응답을 못 받았으면(네트워크 오류) 0 */
  httpStatus: number
  success: boolean
  error?: string
}

export interface OrderStatusResult {
  /** HTTP 상태. 응답을 못 받았으면 0 */
  httpStatus: number
  status?: OrderStatus
  error?: string
}

export interface KeyValueStorage {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

export interface PaymentFlowDeps {
  confirm(params: ConfirmParams): Promise<ConfirmOutcome>
  getStatus(orderId: string): Promise<OrderStatusResult>
  sleep(ms: number): Promise<void>
  now(): number
  /** 없거나 접근이 막혀 있으면(사파리 프라이빗 등) 새로고침 복원만 안 된다 */
  storage?: KeyValueStorage | null
  /** '결제 확인 중' 화면으로 바꿀 때 */
  onChecking?(): void
  /** 화면을 떠나면 true → 조회 중단 */
  isCancelled?(): boolean
}

export type PaymentFlowResult =
  | { kind: 'paid'; planType: PaidPlan }
  | { kind: 'failed'; error: string }
  /** 확인 시간 초과 — 결제는 서버 재조정 작업이 마무리한다. 다시 확인 가능 */
  | { kind: 'timeout' }
  /** 화면을 떠나 중단됨 */
  | { kind: 'aborted' }
  /** 로그인 만료(401/403) — 다시 로그인한 뒤 확인. 저장 상태는 유지 */
  | { kind: 'unauthorized' }

/** 조회 간격: 2s → 4s → 8s → 15s → 30s(이후 반복). 결제 API 분당 30회 제한보다 충분히 낮다 */
export const POLL_DELAYS_MS = [2_000, 4_000, 8_000, 15_000, 30_000]
/** 한 번의 '결제 확인 중' 대기 한도. 서버 재조정 주기(5분) + 최소 대기(2분)보다 길게 */
export const MAX_CHECK_MS = 10 * 60_000
export const CHECKING_STORAGE_KEY = 'bandspace.payment.checking'

export function pollDelay(attempt: number): number {
  return POLL_DELAYS_MS[Math.min(Math.max(attempt, 0), POLL_DELAYS_MS.length - 1)]
}

/** 결과를 알 수 없는 confirm 응답: 서버 503(결과 미확정), 프록시 502/504, 응답 유실(0) */
export function isUncertainConfirm(httpStatus: number): boolean {
  return httpStatus === 0 || httpStatus === 502 || httpStatus === 503 || httpStatus === 504
}

export interface CheckingState extends ConfirmParams {
  /** 이번 확인 창의 시작 시각 — 새로고침해도 유지되어 대기 한도가 늘어나지 않는다 (리뷰 #8 L1) */
  startedAt: number
  /** 이번 확인 창에서 confirm을 이미 다시 보냈는지 (새로고침해도 유지) */
  resent?: boolean
}

/** 이보다 오래된 저장 상태는 버린다 (토스 승인 유효시간·주문 만료보다 충분히 길게) */
export const CHECKING_STATE_MAX_AGE_MS = 24 * 60 * 60_000

export function loadChecking(storage: KeyValueStorage | null | undefined, now: number = Date.now()): CheckingState | null {
  if (!storage) return null
  try {
    const raw = storage.getItem(CHECKING_STORAGE_KEY)
    if (!raw) return null
    const s = JSON.parse(raw) as Partial<CheckingState>
    if (typeof s.orderId !== 'string' || typeof s.paymentKey !== 'string' || typeof s.amount !== 'number' ||
      (s.planType !== 'standard' && s.planType !== 'pro')) return null
    const startedAt = Number(s.startedAt) || 0
    if (now - startedAt > CHECKING_STATE_MAX_AGE_MS) {
      clearChecking(storage)
      return null
    }
    return { orderId: s.orderId, paymentKey: s.paymentKey, amount: s.amount, planType: s.planType, startedAt, resent: s.resent === true }
  } catch {
    return null
  }
}

function saveChecking(storage: KeyValueStorage | null | undefined, state: CheckingState) {
  try { storage?.setItem(CHECKING_STORAGE_KEY, JSON.stringify(state)) } catch { /* 저장 불가: 복원만 포기 */ }
}

export function clearChecking(storage: KeyValueStorage | null | undefined) {
  try { storage?.removeItem(CHECKING_STORAGE_KEY) } catch { /* 무시 */ }
}

const pickParams = (s: ConfirmParams): ConfirmParams =>
  ({ paymentKey: s.paymentKey, orderId: s.orderId, amount: s.amount, planType: s.planType })

const STATUS_MESSAGES: Record<'failed' | 'cancelled' | 'expired', string> = {
  failed: '결제가 완료되지 않았습니다. 다시 시도해 주세요.',
  cancelled: '결제가 취소되었습니다.',
  expired: '주문이 만료되었습니다. 결제를 처음부터 다시 진행해 주세요.',
}

/**
 * 결제 승인 → (필요하면) 결과 조회.
 * 같은 주문의 '확인 중' 상태가 저장돼 있으면(새로고침) confirm을 다시 보내지 않고, 저장된 대기 한도 안에서 조회를 이어간다.
 */
export async function runPaymentFlow(params: ConfirmParams, deps: PaymentFlowDeps): Promise<PaymentFlowResult> {
  const saved = loadChecking(deps.storage, deps.now())
  if (saved && saved.orderId === params.orderId) {
    deps.onChecking?.()
    return pollOrder(saved, deps)
  }

  const r = await deps.confirm(pickParams(params))
  if (r.success) {
    clearChecking(deps.storage)
    return { kind: 'paid', planType: params.planType }
  }
  if (isUncertainConfirm(r.httpStatus)) {
    const state: CheckingState = { ...pickParams(params), startedAt: deps.now() }
    saveChecking(deps.storage, state)
    deps.onChecking?.()
    return pollOrder(state, deps)
  }
  return { kind: 'failed', error: r.error || '결제 승인에 실패했습니다.' }
}

/** '다시 확인': 새 확인 창(대기 한도·재전송 1회)을 열고 조회한다 */
export async function recheckOrder(params: ConfirmParams, deps: PaymentFlowDeps): Promise<PaymentFlowResult> {
  const state: CheckingState = { ...pickParams(params), startedAt: deps.now(), resent: false }
  saveChecking(deps.storage, state)
  return pollOrder(state, deps)
}

/**
 * 주문 상태를 지수 백오프로 조회해 최종 결과를 기다린다 (확인 창 시작부터 MAX_CHECK_MS, 최소 1회는 조회).
 * - paid/failed/cancelled/expired → 저장 상태를 지우고 결과 반환
 * - processing → 계속
 * - created(서버가 confirm을 받지 못함: 요청 유실·DB 일시 오류) → 확인 창마다 confirm을 한 번 다시 보낸다 (서버 멱등)
 * - 401/403(로그인 만료) → 중단, 저장 상태 유지 (리뷰 #8 L2)
 * - 조회 자체 실패(네트워크·5xx·429) → 계속, 404(주문 없음) → 실패
 * 시간 초과면 저장 상태를 남겨 둔다 (다시 확인 시 이어서 조회)
 */
export async function pollOrder(state: CheckingState, deps: PaymentFlowDeps): Promise<PaymentFlowResult> {
  const deadline = state.startedAt + MAX_CHECK_MS
  for (let attempt = 0; attempt === 0 || deps.now() < deadline; attempt++) {
    await deps.sleep(pollDelay(attempt))
    if (deps.isCancelled?.()) return { kind: 'aborted' }

    const s = await deps.getStatus(state.orderId)
    if (s.httpStatus === 401 || s.httpStatus === 403) return { kind: 'unauthorized' }
    if (s.httpStatus === 404) {
      clearChecking(deps.storage)
      return { kind: 'failed', error: s.error || '주문을 찾을 수 없습니다.' }
    }
    if (s.httpStatus !== 200 || !s.status) continue

    switch (s.status) {
      case 'paid':
        clearChecking(deps.storage)
        return { kind: 'paid', planType: state.planType }
      case 'failed':
      case 'cancelled':
      case 'expired':
        clearChecking(deps.storage)
        return { kind: 'failed', error: STATUS_MESSAGES[s.status] }
      case 'created': {
        if (state.resent) break
        state = { ...state, resent: true }
        saveChecking(deps.storage, state)
        const c = await deps.confirm(pickParams(state))
        if (c.success) {
          clearChecking(deps.storage)
          return { kind: 'paid', planType: state.planType }
        }
        if (c.httpStatus === 401 || c.httpStatus === 403) return { kind: 'unauthorized' }
        if (!isUncertainConfirm(c.httpStatus)) {
          clearChecking(deps.storage)
          return { kind: 'failed', error: c.error || '결제 승인에 실패했습니다.' }
        }
        break
      }
      case 'processing':
        break
    }
  }
  return { kind: 'timeout' }
}
