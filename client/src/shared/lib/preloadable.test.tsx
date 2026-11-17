// @vitest-environment jsdom
// 미리 받아 둘 수 있는 지연 모듈 컴포넌트(웹 첫 묶음 축소: 연주자 전용 UI)
import { act, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { preloadableModule } from './preloadable'

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
