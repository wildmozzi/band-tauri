/** 공용 Modal 접근성(P3-8): role=dialog·aria-modal·이름, 포커스 트랩, Esc 닫기, 포커스 복귀 */
import { REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

test('합주실 정보 모달: 대화상자로 열리고 Tab이 안에서 돌며 Esc로 닫힌다', async ({ page, mock }) => {
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  const opener = page.getByRole('button', { name: /정보/ })
  await opener.click()

  const dialog = page.getByRole('dialog', { name: '방 정보' })
  await expect(dialog).toBeVisible()
  await expect(dialog).toHaveAttribute('aria-modal', 'true')
  await expect(dialog).toBeFocused()

  // Tab을 여러 번 눌러도 포커스가 대화상자 밖으로 나가지 않는다
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press('Tab')
    expect(await dialog.evaluate(el => el.contains(document.activeElement))).toBe(true)
  }
  await page.keyboard.press('Shift+Tab')
  expect(await dialog.evaluate(el => el.contains(document.activeElement))).toBe(true)

  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(opener).toBeFocused()

  // 닫기 버튼(이름 '닫기')으로도 닫힌다
  await opener.click()
  await page.getByRole('dialog', { name: '방 정보' }).getByRole('button', { name: '닫기' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
})

test('오디오 설정 모달(웹): 대화상자로 열리고 Esc로 닫히며 포커스가 열기 버튼으로 돌아온다 (P3-8a)', async ({ page }) => {
  await page.goto('/')
  const opener = page.getByRole('button', { name: '🎛️' })
  await opener.click()

  const dialog = page.getByRole('dialog', { name: '오디오 설정' })
  await expect(dialog).toBeVisible()
  await expect(dialog).toHaveAttribute('aria-modal', 'true')
  await expect(dialog).toBeFocused()
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press('Tab')
    expect(await dialog.evaluate(el => el.contains(document.activeElement))).toBe(true)
  }

  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(opener).toBeFocused()

  // 배경을 눌러도 닫힌다
  await opener.click()
  await expect(dialog).toBeVisible()
  await page.mouse.click(5, 5)
  await expect(dialog).toHaveCount(0)
})
