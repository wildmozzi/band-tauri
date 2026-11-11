import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, useLocation, useSearchParams } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { usePremium } from '../contexts/usePremium'
import { SupportContact } from '../components/SupportContact'
import { ALL_PLAN_FEATURES, NO_PAID_ONLY_NOTICE, PLANNED_NOTICE, plannedFeaturesFor } from '../lib/planFeatures'
import { paymentApi } from '../services/paymentApi'
import {
  loadChecking,
  recheckOrder,
  runPaymentFlow,
  type ConfirmParams,
  type KeyValueStorage,
  type PaymentFlowDeps,
  type PaymentFlowResult,
} from '../services/paymentFlow'

const PLAN_INFO: Record<string, { name: string; price: number }> = {
  standard: { name: 'Standard 플랜', price: 2900 },
  pro: { name: 'Pro 플랜', price: 6900 }
}

/** localStorage 접근이 막힌 환경(프라이빗 모드 등)이면 null — 새로고침 복원만 포기한다 */
function browserStorage(): KeyValueStorage | null {
  try {
    return window.localStorage
  } catch {
    return null
  }
}

/**
 * confirming: 승인 요청 중 / checking: 결과 미확정(503) → 주문 상태 조회 중 / done: 결과 확정 또는 확인 지연
 */
type Phase = 'confirming' | 'checking' | 'done'

export function PaymentSuccess() {
  const { user, loading: authLoading } = useAuth()
  const { setUserPlan } = usePremium()
  const setUserPlanRef = useRef(setUserPlan)
  setUserPlanRef.current = setUserPlan
  const [searchParams] = useSearchParams()
  const location = useLocation()
  const [phase, setPhase] = useState<Phase>('confirming')
  const [result, setResult] = useState<PaymentFlowResult | null>(null)
  /** '다시 확인' 횟수 — 0이면 승인부터, 이후는 상태 조회만 */
  const [recheck, setRecheck] = useState(0)

  // 토스 successUrl 파라미터 (paymentKey·orderId·amount는 토스가 붙인다)
  const paymentKey = searchParams.get('paymentKey')
  const orderId = searchParams.get('orderId')
  const amount = searchParams.get('amount')
  const planType = searchParams.get('planType')

  // URL 우선, 없으면 저장된 '결제 확인 중' 상태로 복원
  const params = useMemo<ConfirmParams | null>(() => {
    const n = Number(amount)
    if (paymentKey && orderId && amount && Number.isInteger(n) && (planType === 'standard' || planType === 'pro')) {
      return { paymentKey, orderId, amount: n, planType }
    }
    return loadChecking(browserStorage())
  }, [paymentKey, orderId, amount, planType])

  const userId = user?.id ?? null

  useEffect(() => {
    if (authLoading) return
    if (!params || !userId) {
      setResult({ kind: 'failed', error: userId ? '결제 정보가 올바르지 않습니다.' : '로그인이 필요합니다. 로그인 후 다시 확인해 주세요.' })
      setPhase('done')
      return
    }

    let cancelled = false
    const deps: PaymentFlowDeps = {
      confirm: async (p) => {
        const r = await paymentApi.confirmPayment(p)
        return { httpStatus: r.httpStatus ?? 0, success: r.success, error: r.error }
      },
      getStatus: (id) => paymentApi.getOrderStatus(id),
      sleep: (ms) => new Promise(resolve => setTimeout(resolve, ms)),
      now: () => Date.now(),
      storage: browserStorage(),
      onChecking: () => { if (!cancelled) setPhase('checking') },
      isCancelled: () => cancelled,
    }

    const run = recheck > 0 ? recheckOrder(params, deps) : runPaymentFlow(params, deps)
    run.then((r) => {
      if (cancelled || r.kind === 'aborted') return
      if (r.kind === 'paid') setUserPlanRef.current(r.planType)
      setResult(r)
      setPhase('done')
    }).catch((error) => {
      if (cancelled) return
      console.error('Payment processing error:', error)
      setResult({ kind: 'failed', error: '결제 처리 중 오류가 발생했습니다.' })
      setPhase('done')
    })
    return () => { cancelled = true }
  }, [params, userId, authLoading, recheck])

  // 직접 진입: 확인할 결제 정보(토스가 붙이는 URL 파라미터, 저장된 '결제 확인 중' 상태)가 없다 (P3-12d)
  if (!params) {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <h1>확인할 결제 정보가 없습니다</h1>
          <p>이 화면은 결제를 마친 뒤 자동으로 열립니다. 구독 상태는 내 프로필에서 확인할 수 있습니다.</p>
          <div className="action-buttons">
            <Link to="/pricing" className="primary-action">
              요금제 보기
            </Link>
            <Link to="/profile" className="secondary-action">
              내 구독 확인
            </Link>
          </div>
        </div>
      </div>
    )
  }

  // 결제 정보는 있는데 로그인하지 않음: 실패가 아니라 로그인 안내 (로그인 후 이 주소로 돌아와 이어서 확인)
  if (!authLoading && !userId) {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <h1>로그인이 필요합니다</h1>
          <p>결제 결과를 확인하려면 로그인해 주세요. 중복 결제되지 않으니 다시 결제하지 마세요.</p>
          <div className="action-buttons">
            <Link to="/auth" state={{ from: location.pathname + location.search }} className="primary-action">
              로그인하기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>
        </div>
      </div>
    )
  }

  // 승인 요청 중
  if (phase === 'confirming') {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <div className="loading-spinner"></div>
          <h2>결제를 처리하고 있습니다...</h2>
          <p>잠시만 기다려주세요.</p>
        </div>
      </div>
    )
  }

  // 결과 미확정 → 주문 상태 조회 중
  if (phase === 'checking') {
    return (
      <div className="payment-success-page">
        <div className="success-container" role="status" aria-live="polite">
          <div className="loading-spinner"></div>
          <h2>결제 확인 중</h2>
          <p>결제 결과를 확인하고 있습니다. 최대 몇 분 걸릴 수 있습니다.</p>
          <p>이 화면을 새로고침하거나 닫아도 결제는 안전하게 처리되며, 중복 결제되지 않습니다.</p>
        </div>
      </div>
    )
  }

  // 로그인 만료: 확인 상태는 저장돼 있으므로 로그인 후 이 화면으로 돌아오면 이어서 확인한다
  if (result?.kind === 'unauthorized') {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <h1>로그인이 필요합니다</h1>
          <p>결제 확인 중 로그인이 만료되었습니다. 다시 로그인한 뒤 결제 결과를 확인해 주세요. 중복 결제되지 않으니 다시 결제하지 마세요.</p>
          <div className="action-buttons">
            <Link to="/auth" state={{ from: '/payment/success' }} className="primary-action">
              로그인하기
            </Link>
            <button
              type="button"
              className="secondary-action"
              onClick={() => { setPhase('checking'); setRecheck(n => n + 1) }}
            >
              다시 확인하기
            </button>
          </div>
        </div>
      </div>
    )
  }

  // 확인 지연: 서버 재조정 작업이 마무리한다
  if (result?.kind === 'timeout') {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <h1>결제 확인이 지연되고 있습니다</h1>
          <p>결제가 완료됐다면 잠시 후 구독에 자동으로 반영됩니다. 중복 결제되지 않으니 다시 결제하지 마세요.</p>
          <div className="action-buttons">
            <button
              type="button"
              className="primary-action"
              onClick={() => { setPhase('checking'); setRecheck(n => n + 1) }}
            >
              다시 확인하기
            </button>
            <Link to="/profile" className="secondary-action">
              내 구독 확인
            </Link>
          </div>
        </div>
      </div>
    )
  }

  // 결제 실패
  if (result?.kind !== 'paid') {
    return (
      <div className="payment-success-page">
        <div className="success-container">
          <div className="error-icon">❌</div>
          <h1>결제 처리에 실패했습니다</h1>
          <p className="error-message">{result?.kind === 'failed' ? result.error : '결제 처리 중 오류가 발생했습니다.'}</p>
          <div className="action-buttons">
            <Link to="/pricing" className="primary-action">
              다시 시도하기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>
        </div>
      </div>
    )
  }

  const paymentResult = {
    planType: result.planType,
    planName: PLAN_INFO[result.planType].name,
    price: PLAN_INFO[result.planType].price,
  }

  return (
    <div className="payment-success-page">
      <div className="success-container">
        <div className="success-icon">
          <div className="checkmark">✓</div>
        </div>
        
        <div className="success-content">
          <h1>결제가 완료되었습니다!</h1>
          <p className="success-message">
            <strong>{paymentResult.planName}</strong>을 성공적으로 구독하셨습니다.
          </p>
          
          <div className="payment-info">
            <div className="info-row">
              <span>구독 플랜</span>
              <span>{paymentResult.planName}</span>
            </div>
            <div className="info-row">
              <span>결제 금액</span>
              <span>₩{paymentResult.price?.toLocaleString()}/월</span>
            </div>
            <div className="info-row">
              <span>다음 결제일</span>
              <span>{new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toLocaleDateString()}</span>
            </div>
          </div>

          {/* P3-12a: 요금제로 열리는 기능이 아직 없다 — 실제와 다른 혜택(인원·클라우드·자동 믹싱 등)을 보이지 않는다 */}
          <div className="next-steps">
            <h3>지금 쓸 수 있는 기능</h3>
            <p className="plan-notice">{NO_PAID_ONLY_NOTICE}</p>
            <ul className="plan-feature-list">
              {ALL_PLAN_FEATURES.map(f => <li key={f}>✅ {f}</li>)}
            </ul>
            <h3>준비 중</h3>
            <p className="plan-notice">{PLANNED_NOTICE}</p>
            <ul className="plan-feature-list planned">
              {plannedFeaturesFor(paymentResult.planType).map(f => <li key={f}>⏳ {f}</li>)}
            </ul>
          </div>

          <div className="action-buttons">
            <Link to="/rooms" className="primary-action">
              합주실 만들기
            </Link>
            <Link to="/" className="secondary-action">
              홈으로 가기
            </Link>
          </div>

          <SupportContact />
        </div>
      </div>
    </div>
  )
}