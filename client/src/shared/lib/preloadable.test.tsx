// @vitest-environment jsdom
// 미리 받아 둘 수 있는 지연 모듈 컴포넌트(웹 첫 묶음 축소: 연주자 전용 UI)
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { useEffect, useState } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { PRELOAD_FAILED_MESSAGE, preloadableModule } from './preloadable'

const Hello = ({ name }: { name: string }) => <p>안녕 {name}</p>

describe('preloadableModule', () => {
  it('받아 둔 모듈의 컴포넌트는 첫 그리기에 바로 그린다(Suspense 폴백 없음)', async () => {
    const load = vi.fn(async () => ({ Hello }))
    const m = preloadableModule(load)
    const Comp = m.component('Hello')
    await m.preload()
    expect(m.isLoaded()).toBe(true)
    render(<Comp name="연주자" />)
    // await 없이 바로 보인다
    expect(screen.getByText('안녕 연주자')).toBeTruthy()
    expect(load).toHaveBeenCalledTimes(1)
  })

  it('받아 두지 않았으면 그릴 때 받아 그린다(한 번만 받는다)', async () => {
    const load = vi.fn(async () => ({ Hello }))
    const m = preloadableModule(load)
    const Comp = m.component('Hello')
    render(<><Comp name="A" /><Comp name="B" /></>)
    expect(screen.queryByText('안녕 A')).toBeNull()
    expect(await screen.findByText('안녕 A')).toBeTruthy()
    expect(screen.getByText('안녕 B')).toBeTruthy()
    expect(load).toHaveBeenCalledTimes(1)
  })

  it('받기에 실패하면 다음 preload가 다시 시도한다', async () => {
    const load = vi.fn().mockRejectedValueOnce(new Error('청크 없음')).mockResolvedValue({ Hello })
    const m = preloadableModule(load as () => Promise<{ Hello: typeof Hello }>)
    await expect(m.preload()).rejects.toThrow('청크 없음')
    expect(m.isLoaded()).toBe(false)
    await act(async () => {
      await m.preload()
    })
    expect(m.isLoaded()).toBe(true)
    expect(load).toHaveBeenCalledTimes(2)
  })
})

describe('preloadableModule — 받기 실패·다시 시도 (#221 리뷰 M-1)', () => {
  afterEach(() => cleanup())

  it('preload 없이 그렸는데 받기에 실패하면 그 조각만 안내로 바뀌고 나머지 트리는 살아 있다', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const load = vi.fn().mockRejectedValue(new Error('청크 없음'))
    const m = preloadableModule(load as () => Promise<{ Hello: typeof Hello }>)
    const Comp = m.component('Hello')
    render(<div><p>옆 화면</p><Comp name="A" /></div>)
    expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining(PRELOAD_FAILED_MESSAGE))
    expect(screen.getByText('옆 화면')).toBeTruthy()
    expect(screen.getByRole('button', { name: '다시 시도' })).toBeTruthy()
    error.mockRestore()
  })

  it('다시 시도는 거절을 기억하지 않고 새로 받아 그린다', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const load = vi.fn().mockRejectedValueOnce(new Error('청크 없음')).mockResolvedValue({ Hello })
    const m = preloadableModule(load as () => Promise<{ Hello: typeof Hello }>)
    const Comp = m.component('Hello')
    render(<Comp name="B" />)
    fireEvent.click(await screen.findByRole('button', { name: '다시 시도' }))
    expect(await screen.findByText('안녕 B')).toBeTruthy()
    expect(screen.queryByRole('alert')).toBeNull()
    expect(load).toHaveBeenCalledTimes(2)
    error.mockRestore()
  })

  it('지연으로 그린 인스턴스는 받은 뒤 부모가 다시 그려도 리마운트되지 않는다(지역 상태 유지, 리뷰 L-1)', async () => {
    let mounts = 0
    function Counter() {
      const [n, setN] = useState(0)
      useEffect(() => {
        mounts++
      }, [])
      return <button onClick={() => setN(n + 1)}>눌림 {n}</button>
    }
    const m = preloadableModule(async () => ({ Counter }))
    const Comp = m.component('Counter')
    const { rerender } = render(<div data-tick="0"><Comp /></div>)
    fireEvent.click(await screen.findByText('눌림 0'))
    expect(screen.getByText('눌림 1')).toBeTruthy()
    expect(m.isLoaded()).toBe(true)
    rerender(<div data-tick="1"><Comp /></div>)
    rerender(<div data-tick="2"><Comp /></div>)
    expect(screen.getByText('눌림 1')).toBeTruthy()
    expect(mounts).toBe(1)
  })
})
