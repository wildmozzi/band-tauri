// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { SupportContact } from './SupportContact'

const renderWith = (email: string) =>
  render(<MemoryRouter><SupportContact email={email} /></MemoryRouter>)

describe('SupportContact (P3-12a)', () => {
  afterEach(cleanup)

  it('문의 메일이 설정돼 있으면 mailto 링크', () => {
    renderWith('help@example.test')
    const link = screen.getByRole('link', { name: 'help@example.test' })
    expect(link.getAttribute('href')).toBe('mailto:help@example.test')
  })

  it('문의 메일이 없으면 메일 대신 요금제 관리 화면을 안내한다', () => {
    const { container } = renderWith('')
    expect(container.querySelector('a[href^="mailto:"]')).toBeNull()
    const link = screen.getByRole('link', { name: '프로필 → 요금제 관리' })
    expect(link.getAttribute('href')).toBe('/profile?tab=plan')
  })

  it('공개 게시판에 결제 정보를 올리지 말라고 알리고, 옛 주소를 쓰지 않는다', () => {
    const { container } = renderWith('')
    expect(container.textContent).toContain('공개 게시판')
    expect(container.textContent).not.toContain('bandspace')
  })
})
