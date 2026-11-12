import type { ButtonHTMLAttributes } from 'react'
import { cx } from './cx'

const VARIANT_CLASS = {
  primary: 'btn-primary',
  secondary: 'btn-secondary',
} as const

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: keyof typeof VARIANT_CLASS
}

/**
 * 공용 버튼 (P3-8). 기존 .btn-primary / .btn-secondary 스타일을 그대로 쓴다.
 * type 기본값은 'button' — 폼 제출 버튼은 type="submit"을 명시한다(암묵적 submit 방지).
 */
export function Button({ variant = 'primary', type = 'button', className, ...rest }: ButtonProps) {
  return <button type={type} className={cx(VARIANT_CLASS[variant], className)} {...rest} />
}
