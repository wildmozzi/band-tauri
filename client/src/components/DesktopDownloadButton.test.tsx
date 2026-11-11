// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import { DesktopDownloadButton } from './DesktopDownloadButton'

describe('DesktopDownloadButton', () => {
  afterEach(cleanup)

  it('주소가 있으면 새 탭 다운로드 링크', () => {
    render(<DesktopDownloadButton url="https://example.test/app.msi" />)
    const link = screen.getByRole('link', { name: '📥 데스크톱 앱 다운로드' })
    expect(link.getAttribute('href')).toBe('https://example.test/app.msi')
    expect(link.getAttribute('target')).toBe('_blank')
    expect(link.getAttribute('rel')).toContain('noopener')
  })

  it('주소가 비면 준비 중 비활성 버튼', () => {
    render(<DesktopDownloadButton url="" />)
    expect(screen.queryByRole('link')).toBeNull()
    const button = screen.getByRole('button', { name: '📥 다운로드 준비 중' }) as HTMLButtonElement
    expect(button.disabled).toBe(true)
  })
})
