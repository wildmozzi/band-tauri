// @vitest-environment jsdom
// 앱 루트 배너 입구: 배너 묶음 import가 거절돼도 앱의 나머지 트리는 그대로 그려진다(#266 리뷰 M-1)
import { render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { UpdateNotice } from './UpdateNoticeGate'

vi.mock('../../native/updaterGate', () => ({ isUpdaterAvailable: () => true }))
vi.mock('./UpdateNotice', () => {
  throw new Error('청크를 불러오지 못함')
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('UpdateNoticeGate', () => {
  it('배너 묶음 import가 거절되면 null을 그리고 에러를 남기며, 형제 트리는 그대로 그려진다', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    render(
      <div>
        <UpdateNotice />
        <p>합주실 화면</p>
      </div>,
    )
    await waitFor(() => expect(error).toHaveBeenCalledWith('[UpdateNotice] 불러오기 실패:', expect.anything()))
    expect(screen.getByText('합주실 화면')).toBeTruthy()
    expect(document.querySelector('.update-notice')).toBeNull()
  })
})
