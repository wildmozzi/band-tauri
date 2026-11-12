// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { DevServerSettingsSection } from './DevServerSettings'
import { DEV_SERVER_STORAGE_KEY } from '../../shared/lib/apiUrl'

describe('DevServerSettingsSection', () => {
  afterEach(() => {
    cleanup()
    vi.unstubAllEnvs()
    vi.unstubAllGlobals()
    window.localStorage.clear()
  })

  it('개발 프록시가 꺼져 있으면(운영 빌드) 그리지 않는다', () => {
    const { container } = render(<DevServerSettingsSection enabled={false} />)
    expect(container.innerHTML).toBe('')
  })

  it('사설 IP를 저장하고 새로고침한다', () => {
    vi.stubGlobal('__BAND_DEV_PROXY__', 'http')
    const reload = vi.fn()
    render(<DevServerSettingsSection enabled reload={reload} />)
    expect(screen.getByText(/지금 연결: http:\/\/localhost:8080/)).toBeTruthy()
    fireEvent.change(screen.getByLabelText('개발 서버 주소'), { target: { value: '192.168.0.10:8080' } })
    fireEvent.click(screen.getByRole('button', { name: '저장 후 새로고침' }))
    expect(window.localStorage.getItem(DEV_SERVER_STORAGE_KEY)).toBe('192.168.0.10:8080')
    expect(reload).toHaveBeenCalledTimes(1)
  })

  it('공인 주소·도메인은 저장하지 않고 알린다', () => {
    vi.stubGlobal('__BAND_DEV_PROXY__', 'http')
    const reload = vi.fn()
    render(<DevServerSettingsSection enabled reload={reload} />)
    fireEvent.change(screen.getByLabelText('개발 서버 주소'), { target: { value: 'band-server.fly.dev' } })
    fireEvent.click(screen.getByRole('button', { name: '저장 후 새로고침' }))
    expect(screen.getByRole('alert').textContent).toContain('사설 IP')
    expect(window.localStorage.getItem(DEV_SERVER_STORAGE_KEY)).toBeNull()
    expect(reload).not.toHaveBeenCalled()
  })

  it('기본값 버튼은 저장값을 지운다', () => {
    vi.stubGlobal('__BAND_DEV_PROXY__', 'http')
    window.localStorage.setItem(DEV_SERVER_STORAGE_KEY, '10.0.0.5')
    const reload = vi.fn()
    render(<DevServerSettingsSection enabled reload={reload} />)
    expect(screen.getByText(/지금 연결: http:\/\/10\.0\.0\.5:8080/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '기본값' }))
    expect(window.localStorage.getItem(DEV_SERVER_STORAGE_KEY)).toBeNull()
    expect(reload).toHaveBeenCalledTimes(1)
  })
})
