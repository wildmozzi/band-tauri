// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { DiagnosticsExport } from './DiagnosticsExport'
import { diagStatusText } from '../../native/diagFormat'

describe('DiagnosticsExport', () => {
  afterEach(cleanup)

  it('웹(비데스크톱)에서는 숨긴다', () => {
    const { container } = render(<DiagnosticsExport available={false} />)
    expect(container.innerHTML).toBe('')
  })

  it('저장 경로를 안내한다', async () => {
    const exportFn = vi.fn().mockResolvedValue('D:\\band-diag-20261111-090807.zip')
    render(<DiagnosticsExport available exportFn={exportFn} />)
    fireEvent.click(screen.getByRole('button', { name: /진단 내보내기/ }))
    expect(await screen.findByRole('status')).toHaveProperty('textContent', expect.stringContaining('band-diag-20261111-090807.zip'))
    expect(exportFn).toHaveBeenCalledTimes(1)
  })

  it('취소와 에러를 구분한다', async () => {
    render(<DiagnosticsExport available compact exportFn={() => Promise.resolve(null)} />)
    fireEvent.click(screen.getByRole('button'))
    expect((await screen.findByRole('status')).textContent).toBe('저장을 취소했습니다.')
    cleanup()
    render(<DiagnosticsExport available exportFn={() => Promise.reject('진단 파일 저장 실패: 권한')} />)
    fireEvent.click(screen.getByRole('button'))
    expect((await screen.findByRole('status')).textContent).toBe('내보내기 실패: 진단 파일 저장 실패: 권한')
  })

  it('진행 중·대기 상태에는 문구가 없다', () => {
    expect(diagStatusText({ kind: 'idle' })).toBeNull()
    expect(diagStatusText({ kind: 'busy' })).toBeNull()
  })
})
