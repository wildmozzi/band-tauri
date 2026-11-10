import { JAZZ_ROOM, POP_ROOM, PRIVATE_ROOM, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

test.describe('합주실 목록', () => {
  test('공개 방만 보이고 비공개 방은 숨겨진다, 장르 필터가 동작한다', async ({ page }) => {
    await page.goto('/rooms')
    const cards = page.locator('.room-card')
    await expect(cards).toHaveCount(3)
    await expect(cards.locator('h3')).toHaveText([ROCK_ROOM.title, `🔒 ${JAZZ_ROOM.title}`, POP_ROOM.title])
    await expect(page.getByText(PRIVATE_ROOM.title)).toHaveCount(0)

    // 비밀번호 방 표시, 승인제 표시
    const jazz = cards.filter({ hasText: JAZZ_ROOM.title })
    await expect(jazz.getByTitle('비밀번호 필요')).toBeVisible()
    await expect(jazz.locator('.stat-value.approval')).toHaveText('승인')

    // 장르 필터
    await page.locator('.room-filters').getByRole('button', { name: '재즈' }).click()
    await expect(cards).toHaveCount(1)
    await expect(cards.first()).toContainText(JAZZ_ROOM.title)

    await page.locator('.room-filters').getByRole('button', { name: '팝' }).click()
    // 비공개 팝 방은 필터에서도 보이지 않는다
    await expect(cards).toHaveCount(1)
    await expect(cards.first()).toContainText(POP_ROOM.title)

    await page.locator('.room-filters').getByRole('button', { name: '메탈' }).click()
    await expect(page.getByText('해당 장르의 합주실이 없습니다.')).toBeVisible()

    await page.locator('.room-filters').getByRole('button', { name: '전체' }).click()
    await expect(cards).toHaveCount(3)
  })

  test('방 코드(초대 링크)로 비공개 방에 입장한다', async ({ page, mock }) => {
    await page.goto('/rooms')
    const codeInput = page.getByLabel('방 코드 또는 초대 링크')

    // 알아볼 수 없는 코드
    await codeInput.fill('not-a-room-code')
    await page.locator('.room-code-join').getByRole('button', { name: '입장' }).click()
    await expect(page.getByText('방 코드나 초대 링크를 확인해주세요.')).toBeVisible()

    // 초대 링크 형식
    await codeInput.fill(`https://band.e2e.test/rooms/${PRIVATE_ROOM.id.toUpperCase()}`)
    await page.locator('.room-code-join').getByRole('button', { name: '입장' }).click()
    await expect(page).toHaveURL(new RegExp(`/rooms/${PRIVATE_ROOM.id}$`))
    await expect(page.getByRole('heading', { level: 1, name: PRIVATE_ROOM.title })).toBeVisible()

    // 비공개 방은 목록 조회가 아니라 get_room RPC로 읽는다, 시그널링 join은 그 방 id로
    expect(mock.supabase.db.count('POST', 'rpc/get_room')).toBeGreaterThan(0)
    await expect.poll(() => mock.signaling.messages('join').map(m => m.roomId)).toContain(PRIVATE_ROOM.id)
  })
})
