// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { Badge, Button, Field, Modal } from './index'

afterEach(cleanup)

function Harness({ onClose = () => {} }: { onClose?: () => void }) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button onClick={() => setOpen(true)}>열기</button>
      {open && (
        <Modal
          className="room-info-modal"
          title="방 정보"
          onClose={() => {
            onClose()
            setOpen(false)
          }}
        >
          <input aria-label="첫째" />
          <button>마지막</button>
        </Modal>
      )}
    </>
  )
}

describe('Modal', () => {
  it('기존 마크업 구조 + role=dialog·aria-modal·aria-labelledby', () => {
    const { container } = render(
      <Modal className="room-info-modal" title="방 정보" onClose={() => {}}>
        <p>본문</p>
      </Modal>,
    )
    const dialog = screen.getByRole('dialog', { name: '방 정보' })
    expect(dialog.getAttribute('aria-modal')).toBe('true')
    expect(dialog.className).toBe('modal-content')
    const root = container.firstElementChild as HTMLElement
    expect(root.className).toBe('room-info-modal')
    expect(root.querySelector(':scope > .modal-backdrop')).not.toBeNull()
    expect(dialog.querySelector('.modal-header > h2')?.textContent).toBe('방 정보')
    expect(dialog.querySelector('.modal-header > .close-btn')?.textContent).toBe('×')
    expect(dialog.querySelector('.modal-body')?.textContent).toBe('본문')
    expect(screen.getByRole('button', { name: '닫기' })).toBeTruthy()
  })

  it('열면 대화상자에 포커스, Esc로 닫고 연 버튼으로 포커스가 돌아간다', () => {
    const onClose = vi.fn()
    render(<Harness onClose={onClose} />)
    const opener = screen.getByRole('button', { name: '열기' })
    opener.focus()
    fireEvent.click(opener)
    expect(document.activeElement).toBe(screen.getByRole('dialog'))
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(onClose).toHaveBeenCalledTimes(1)
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(document.activeElement).toBe(opener)
  })

  it('Tab/Shift+Tab 포커스가 대화상자 안을 돈다', () => {
    render(<Harness />)
    fireEvent.click(screen.getByRole('button', { name: '열기' }))
    const close = screen.getByRole('button', { name: '닫기' })
    const last = screen.getByRole('button', { name: '마지막' })
    // 대화상자 자체에서 Shift+Tab → 마지막 요소
    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true })
    expect(document.activeElement).toBe(last)
    // 마지막에서 Tab → 첫 요소(닫기 버튼)
    fireEvent.keyDown(document, { key: 'Tab' })
    expect(document.activeElement).toBe(close)
    // 첫 요소에서 Shift+Tab → 마지막
    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true })
    expect(document.activeElement).toBe(last)
  })

  it('겹친 모달은 맨 위만 Esc로 닫힌다', () => {
    const outer = vi.fn()
    const inner = vi.fn()
    render(
      <Modal className="a" title="바깥" onClose={outer}>
        <Modal className="b" title="안쪽" onClose={inner}>
          <p>x</p>
        </Modal>
      </Modal>,
    )
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(inner).toHaveBeenCalledTimes(1)
    expect(outer).not.toHaveBeenCalled()
  })

  it('배경을 누르면 닫는다', () => {
    const onClose = vi.fn()
    const { container } = render(
      <Modal className="x" title="t" onClose={onClose}>
        <p>x</p>
      </Modal>,
    )
    fireEvent.click(container.querySelector('.modal-backdrop')!)
    expect(onClose).toHaveBeenCalledTimes(1)
  })
})

describe('Button / Field / Badge', () => {
  it('Button: 변형 클래스, 기본 type=button, submit 명시', () => {
    render(
      <>
        <Button>기본</Button>
        <Button variant="secondary" className="wide">보조</Button>
        <Button type="submit">제출</Button>
      </>,
    )
    const primary = screen.getByRole('button', { name: '기본' })
    expect(primary.className).toBe('btn-primary')
    expect(primary.getAttribute('type')).toBe('button')
    expect(screen.getByRole('button', { name: '보조' }).className).toBe('btn-secondary wide')
    expect(screen.getByRole('button', { name: '제출' }).getAttribute('type')).toBe('submit')
  })

  it('Field: label이 컨트롤에 연결되고, 도움말 id를 만든다', () => {
    const { container } = render(
      <Field id="email" label="이메일 *" hint="회사 메일">
        <input id="email" aria-describedby="email-hint" />
      </Field>,
    )
    expect(container.firstElementChild?.className).toBe('form-group')
    const input = screen.getByLabelText('이메일 *')
    expect(input.id).toBe('email')
    expect(document.getElementById('email-hint')?.textContent).toBe('회사 메일')
  })

  it('Badge: kind + tone 클래스', () => {
    render(<Badge kind="status-badge" tone="live">LIVE</Badge>)
    expect(screen.getByText('LIVE').className).toBe('status-badge live')
  })
})
