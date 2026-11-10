import { ALICE } from './fixtures'
import { expect, test } from './test'

test('홈 화면이 렌더되고 합주실로 이동할 수 있다', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '초저지연 온라인 합주실' })).toBeVisible()
  await expect(page.getByRole('link', { name: '로그인' })).toBeVisible()

  await page.getByRole('link', { name: '합주실 열기' }).click()
  await expect(page).toHaveURL(/\/rooms$/)
  await expect(page.getByRole('heading', { name: '합주실 찾기' })).toBeVisible()
})

test('로그인: 틀린 비밀번호는 오류, 맞는 비밀번호는 세션 생성 후 홈으로', async ({ page, mock, consoleErrors }) => {
  // 틀린 비밀번호 → 가짜 Auth가 400 (브라우저가 실패 응답을 콘솔에 남긴다)
  consoleErrors.allow(/Failed to load resource: the server responded with a status of 400.*\/auth\/v1\/token/)

  await page.goto('/auth')
  await page.getByPlaceholder('you@bandspace.com').fill(ALICE.email)
  await page.getByPlaceholder('8자 이상 입력').fill('wrong-password')
  await page.locator('form.auth-form').getByRole('button', { name: '로그인' }).click()
  await expect(page.getByText('Invalid login credentials')).toBeVisible()
  await expect(page).toHaveURL(/\/auth$/)

  await page.getByPlaceholder('8자 이상 입력').fill(ALICE.password)
  await page.locator('form.auth-form').getByRole('button', { name: '로그인' }).click()

  await expect(page).toHaveURL(/\/$/)
  await expect(page.locator('.user-email')).toHaveText(ALICE.email)
  await expect(page.getByRole('button', { name: '로그아웃' })).toBeVisible()

  // 세션이 저장돼 새로고침 후에도 유지되고, 서버 API에는 Bearer 토큰으로 본인 확인이 간다
  await page.reload()
  await expect(page.locator('.user-email')).toHaveText(ALICE.email)
  await expect.poll(() => mock.api.callsTo('GET', '/api/payment/subscription').map(c => c.userId)).toContain(ALICE.id)

  // 로그아웃
  await page.getByRole('button', { name: '로그아웃' }).click()
  await expect(page.locator('nav').getByRole('link', { name: '로그인' })).toBeVisible()
})
