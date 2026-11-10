import type { HTMLAttributes } from 'react'
import { cx } from './cx'

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  /** 배지 종류 클래스 (예: 'status-badge', 'genre-badge', 'live-badge') */
  kind: string
  /** 상태 수식 클래스 (예: 'active', 'live') */
  tone?: string
}

/** 공용 배지 (P3-8): <span class="{kind} {tone}"> */
export function Badge({ kind, tone, className, ...rest }: BadgeProps) {
  return <span className={cx(kind, tone, className)} {...rest} />
}
