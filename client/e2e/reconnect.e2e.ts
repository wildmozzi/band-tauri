import { REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

// P1-15b: 방 안에서 시그널링이 끊기면 합주실 화면은 그대로 두고 상단 띠로 알리고, 재연결되면 띠가 사라진다
test.describe('시그널링 재연결 (P1-15b)', () => {
  test('서버가 끊기면 화면 유지 + 띠 → 서버가 돌아오면 띠가 사라지고 같은 방에 다시 입장한다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    const title = page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })
    await expect(title).toBeVisible()
    const banner = page.locator('.reconnect-banner')
    await expect(banner).toHaveCount(0)
    expect(mock.signaling.messages('join')).toHaveLength(1)

    // 서버 장애: 지금 연결을 끊고 재연결 시도도 거부한다
    mock.signaling.setDown(true)
    await expect(banner).toHaveText('서버 연결이 끊겨 다시 연결하는 중…')
    await expect(banner).toHaveAttribute('role', 'status')
    // 전체 화면 "연결 중" 대신 합주실 화면이 그대로 있다
    await expect(title).toBeVisible()
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await expect(page.getByText('서버에 연결 중...')).toHaveCount(0)
    await expect.poll(() => mock.signaling.refused).toBeGreaterThanOrEqual(2) // 백오프 재시도가 실패하는 동안에도 띠 유지
    await expect(banner).toBeVisible()

    // 복구: 다음 재연결이 받아들여지면 띠가 사라지고 rejoin join을 한 번 보낸다
    mock.signaling.setDown(false)
    await expect(banner).toHaveCount(0, { timeout: 10_000 })
    await expect(title).toBeVisible()
    await expect.poll(() => mock.signaling.messages('join').length).toBe(2)
    expect(mock.signaling.messages('join')[1]).toMatchObject({ roomId: ROCK_ROOM.id, isRejoin: true })
  })
})
