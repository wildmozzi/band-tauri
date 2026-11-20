/**
 * P3-12a 요금제·인원·문의 문구가 실제 구현과 맞는지 (판매 문구에 미구현 기능·옛 인원이 없어야 한다).
 */
import { expect, test } from './test'

test.describe('요금제 문구 (P3-12a)', () => {
  test('요금제 화면: 실제 인원 규칙, 미구현 기능은 준비 중으로 분리', async ({ page }) => {
    await page.goto('/pricing')
    const body = page.locator('body')
    await expect(page.locator('.pricing-notice')).toContainText('유료 플랜에만 열리는 기능이 없습니다')
    await expect(body).toContainText('연주 최대 6명')
    await expect(body).toContainText('관람 포함 최대 20명')
    for (const old of ['최대 4명', '최대 8명', '7일 제한', '자동 백업', '광고 표시']) {
      await expect(body).not.toContainText(old)
    }
    // 유료 카드의 '주요 기능'에는 미구현 기능이 없고, 준비 중 목록에만 있다
    const pro = page.locator('.pro-plan')
    await expect(pro.locator('.feature-highlight')).not.toContainText('자동 믹싱')
    await expect(pro.locator('.feature-planned')).toContainText('자동 믹싱')
  })

  test('결제 실패 화면: 문의 메일이 없으면 지어낸 주소 대신 요금제 관리 안내', async ({ page }) => {
    await page.goto('/payment/fail?code=PAY_PROCESS_CANCELED&orderId=order_e2e')
    const support = page.locator('.support-info')
    await expect(support).not.toContainText('bandspace')
    await expect(support.locator('a[href^="mailto:"]')).toHaveCount(0)
    await expect(support.getByRole('link', { name: '프로필 → 요금제 관리' })).toHaveAttribute('href', '/profile?tab=plan')
    await expect(support).toContainText('공개 게시판')
  })
})
