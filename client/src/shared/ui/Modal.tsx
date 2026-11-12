import { useEffect, useId, useRef, type ReactNode } from 'react'
import './ui.css'
import './modal.css'

/** 열린 모달 스택 — Esc·Tab은 맨 위 모달만 처리한다 (모달 위에 모달이 뜨는 경우) */
const openStack: symbol[] = []

const FOCUSABLE =
  'a[href], area[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), iframe, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]'

function focusablesIn(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(n => !n.closest('[hidden], [inert]'))
}

/**
 * 안쪽 요소 클래스 덮어쓰기. 기본값은 공용 마크업(.modal-backdrop / .modal-content / .modal-header / .modal-body / .close-btn).
 * 자기 CSS를 따로 쓰던 모달(프리미엄·오디오 설정·에코 측정)이 화면을 그대로 둔 채 공용 동작(포커스·Esc)을 쓰게 한다.
 */
export interface ModalClassNames {
  backdrop?: string
  content?: string
  header?: string
  body?: string
  close?: string
  description?: string
}

export interface ModalProps {
  /** 바깥 래퍼 클래스 — 화면별 모달 스타일(예: 'room-info-modal') */
  className: string
  /** 머리글 제목. aria-labelledby로 대화상자 이름이 된다 */
  title: ReactNode
  onClose: () => void
  children: ReactNode
  /** 제목 아래 설명 문단(머리글 안). aria-describedby로 연결된다 */
  description?: ReactNode
  /** 머리글의 × 닫기 버튼. 본문에 닫기 버튼이 따로 있으면 false (기본 true) */
  closeButton?: boolean
  classNames?: ModalClassNames
}

/**
 * 공용 모달 (P3-8). 기존 마크업과 같은 DOM을 그린다:
 *   .{className} > .modal-backdrop + .modal-content[role=dialog] > .modal-header(h2 + .close-btn) + .modal-body
 * classNames로 안쪽 요소 클래스를 바꿀 수 있고(기존 CSS를 그대로 쓰는 모달), description은 머리글 안 설명이다(aria-describedby).
 * 접근성: role="dialog" + aria-modal + aria-labelledby, 열면 대화상자로 포커스, Tab/Shift+Tab 포커스 트랩,
 * Esc로 닫기, 닫으면 열기 전 포커스로 돌아간다.
 */
export function Modal({ className, title, onClose, children, description, closeButton = true, classNames = {} }: ModalProps) {
  const titleId = useId()
  const descriptionId = useId()
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
      <div className={classNames.backdrop ?? 'modal-backdrop'} onClick={onClose} />
      <div
        className={classNames.content ?? 'modal-content'}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description != null ? descriptionId : undefined}
        tabIndex={-1}
        ref={contentRef}
      >
        <div className={classNames.header ?? 'modal-header'}>
          <h2 id={titleId}>{title}</h2>
          {description != null && (
            <p id={descriptionId} className={classNames.description ?? 'modal-description'}>
              {description}
            </p>
          )}
          {closeButton && (
            <button type="button" onClick={onClose} className={classNames.close ?? 'close-btn'} aria-label="닫기">
              ×
            </button>
          )}
        </div>
        <div className={classNames.body ?? 'modal-body'}>{children}</div>
      </div>
    </div>
  )
}
