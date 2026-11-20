import type { HTMLAttributes } from 'react'

export interface CardProps extends HTMLAttributes<HTMLElement> {
  /** 시맨틱 요소: 목록 항목은 article, 화면 구획은 section */
  as?: 'div' | 'article' | 'section'
}

/** 공용 카드 컨테이너 (P3-8). 모양은 화면별 클래스(className)가 정한다 */
export function Card({ as: Tag = 'div', ...rest }: CardProps) {
  return <Tag {...rest} />
}
