import { useEffect, useId, useRef, type ReactNode } from 'react'
import './ui.css'

/** 열린 모달 스택 — Esc·Tab은 맨 위 모달만 처리한다 (모달 위에 모달이 뜨는 경우) */
const openStack: symbol[] = []

const FOCUSABLE =
  'a[href], area[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), iframe, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]'

function focusablesIn(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(n => !n.closest('[hidden], [inert]'))
}

export interface ModalProps {
  /** 바깥 래퍼 클래스 — 화면별 모달 스타일(예: 'room-info-modal') */
  className: string
  /** 머리글 제목. aria-labelledby로 대화상자 이름이 된다 */
  title: ReactNode
  onClose: () => void
  children: ReactNode
}

/**
 * 공용 모달 (P3-8). 기존 마크업과 같은 DOM을 그린다:
 *   .{className} > .modal-backdrop + .modal-content[role=dialog] > .modal-header(h2 + .close-btn) + .modal-body
 * 접근성: role="dialog" + aria-modal + aria-labelledby, 열면 대화상자로 포커스, Tab/Shift+Tab 포커스 트랩,
 * Esc로 닫기, 닫으면 열기 전 포커스로 돌아간다.
 */
export function Modal({ className, title, onClose, children }: ModalProps) {
  const titleId = useId()
  const contentRef = useRef<HTMLDivElement>(null)
  const onCloseRef = useRef(onClose)
  useEffect(() => {
    onCloseRef.current = onClose
  }, [onClose])

  useEffect(() => {
    const me = Symbol('modal')
    openStack.push(me)
    const el = contentRef.current
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    // 안쪽 요소가 autoFocus로 이미 포커스를 가져갔으면 그대로 둔다
    if (el && !el.contains(document.activeElement)) el.focus({ preventScroll: true })

    const onKeyDown = (e: KeyboardEvent) => {
      if (!el) return
      // 포커스가 든 대화상자가 처리한다. 포커스가 대화상자 밖이면 가장 나중에 열린 모달이 처리한다
      const owner = document.activeElement?.closest('[role="dialog"][aria-modal="true"]')
      if (owner ? owner !== el : openStack[openStack.length - 1] !== me) return
      if (e.key === 'Escape') {
        e.stopPropagation()
        onCloseRef.current()
        return
      }
      if (e.key !== 'Tab') return
      const items = focusablesIn(el)
      if (items.length === 0) {
        e.preventDefault()
        el.focus()
        return
      }
      const first = items[0]
      const last = items[items.length - 1]
      const active = document.activeElement
      if (e.shiftKey && (active === first || active === el || !el.contains(active))) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && (active === last || !el.contains(active))) {
        e.preventDefault()
        first.focus()
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('keydown', onKeyDown)
      const i = openStack.indexOf(me)
      if (i >= 0) openStack.splice(i, 1)
      if (previous && previous.isConnected) previous.focus({ preventScroll: true })
    }
  }, [])

  return (
    <div className={className}>
      <div className="modal-backdrop" onClick={onClose} />
      <div
        className="modal-content"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        ref={contentRef}
      >
        <div className="modal-header">
          <h2 id={titleId}>{title}</h2>
          <button type="button" onClick={onClose} className="close-btn" aria-label="닫기">×</button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  )
}
