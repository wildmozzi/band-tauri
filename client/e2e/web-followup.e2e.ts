/**
 * P3-12d 웹 재점검 후속 회귀 테스트 (docs/reviews/2026-11-11-web-recheck.md의 M8·L4·L5·L6).
 */
import { ALICE, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

test('합주실 상세 390: 연주자 카드가 데스크톱 앱 안내에 가리지 않고 다 보인다 (M8)', async ({ page, mock }) => {
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  const panel = page.locator('.performers-panel')
  await expect(panel).toContainText(REMOTE_PERFORMER.nickname)

  const item = panel.locator('.performer-item').first()
  const list = panel.locator('.performers-list')
  const itemBox = (await item.boundingBox())!
  const listBox = (await list.boundingBox())!
  // 카드 전체가 목록 칸 안에 있다 (전에는 패널 max-height 200px에 눌려 윗부분 몇 px만 보였다)
  expect(itemBox.y + itemBox.height).toBeLessThanOrEqual(listBox.y + listBox.height + 1)
  // 안내(참여 토글)는 목록 아래에 있다
  const noticeBox = (await panel.locator('.participation-toggle').boundingBox())!
  expect(noticeBox.y).toBeGreaterThanOrEqual(listBox.y + listBox.height - 1)
})

test('방 만들기: 토글 스위치 3개에 접근 가능한 이름이 있고 키보드로 바꿀 수 있다 (L4)', async ({ page, mock }) => {
  await mock.loginAs(ALICE)
  await page.goto('/rooms/create')

  const freeJoin = page.getByRole('switch', { name: '자유 참여' })
  const isPublic = page.getByRole('switch', { name: '합주실 목록에 공개' })
  const usePassword = page.getByRole('switch', { name: '비밀번호 사용' })
  await expect(freeJoin).toHaveAccessibleDescription(/연주자로 참여할 수 있습니다/)
  await expect(isPublic).toBeChecked()
  await expect(usePassword).not.toBeChecked()

  await usePassword.focus()
  await page.keyboard.press('Space')
  await expect(usePassword).toBeChecked()
  await expect(page.getByLabel('합주실 비밀번호')).toBeVisible()
})

test.describe('결제 완료/실패 직접 진입 (L5·L6)', () => {
  test('/payment/success를 정보 없이 열면 실패가 아니라 안내를 보여 준다', async ({ page }) => {
    await page.goto('/payment/success')
    await expect(page.getByRole('heading', { level: 1, name: '확인할 결제 정보가 없습니다' })).toBeVisible()
    await expect(page.locator('body')).not.toContainText('결제 처리에 실패했습니다')
    await expect(page.getByRole('link', { name: '요금제 보기' })).toHaveAttribute('href', '/pricing')
  })

  test('/payment/success에 결제 정보가 있는데 비로그인이면 로그인 안내', async ({ page }) => {
    await page.goto('/payment/success?paymentKey=pk_e2e&orderId=order_e2e&amount=2900&planType=standard')
    await expect(page.getByRole('heading', { level: 1, name: '로그인이 필요합니다' })).toBeVisible()
    await expect(page.locator('body')).not.toContainText('결제 처리에 실패했습니다')
  })

  test('/payment/fail을 정보 없이 열면 안내만 보이고 콘솔 에러가 없다', async ({ page }) => {
    await page.goto('/payment/fail')
    await expect(page.getByRole('heading', { level: 1, name: '표시할 결제 실패 정보가 없습니다' })).toBeVisible()
    await expect(page.locator('body')).not.toContainText('결제에 실패했습니다')
  })

  test('/payment/fail에 실패 코드가 있으면 기존대로 사유를 보여 준다 (콘솔 error 없음)', async ({ page }) => {
    await page.goto('/payment/fail?code=PAY_PROCESS_CANCELED&orderId=order_e2e')
    await expect(page.getByRole('heading', { level: 1, name: '결제에 실패했습니다' })).toBeVisible()
    await expect(page.getByText('사용자가 결제를 취소했습니다.')).toBeVisible()
  })
})
