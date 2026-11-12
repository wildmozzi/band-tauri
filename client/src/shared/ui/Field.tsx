import type { ReactNode } from 'react'

export interface FieldProps {
  /** 안쪽 컨트롤의 id — label htmlFor로 연결한다 */
  id: string
  label: ReactNode
  /** 컨트롤 아래 도움말. `${id}-hint` id로 그린다 — 컨트롤에 aria-describedby를 같은 값으로 단다 */
  hint?: ReactNode
  /** 래퍼 클래스 (화면별: 'form-group', 'edit-form-group' …) */
  className?: string
  children: ReactNode
}

/** 공용 폼 필드 (P3-8): label + 컨트롤 + 도움말. 기존 마크업(div > label + control)과 같은 DOM */
export function Field({ id, label, hint, className = 'form-group', children }: FieldProps) {
  return (
    <div className={className}>
      <label htmlFor={id}>{label}</label>
      {children}
      {hint !== undefined && (
        <small id={`${id}-hint`} className="form-hint">
          {hint}
        </small>
      )}
    </div>
  )
}
