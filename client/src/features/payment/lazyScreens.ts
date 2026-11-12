// 결제 화면은 라우트에 들어갈 때 내려받는다(P3-2 ③-2): 토스 SDK·결제 화면 CSS가 메인 번들에서 빠진다.
// PremiumProvider·PlanStatus 등 앱 전역에서 쓰는 것은 index.ts에서 그대로 즉시 로드한다.
import { lazy } from 'react'

export const Pricing = lazy(() => import('./Pricing').then(m => ({ default: m.Pricing })))
export const Payment = lazy(() => import('./Payment').then(m => ({ default: m.Payment })))
export const PaymentSuccess = lazy(() => import('./PaymentSuccess').then(m => ({ default: m.PaymentSuccess })))
export const PaymentFail = lazy(() => import('./PaymentFail').then(m => ({ default: m.PaymentFail })))
