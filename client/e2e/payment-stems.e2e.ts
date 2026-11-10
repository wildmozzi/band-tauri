import { ALICE, STEM_HISTORY } from './fixtures'
import { expect, test } from './test'

test.describe('결제', () => {
  test('결제 진입 → 승인 503 → 결제 확인 중 → 주문 조회로 완료', async ({ page, mock, consoleErrors }) => {
    // 승인 응답 503은 의도한 시나리오 (브라우저가 실패 응답을 콘솔에 남긴다)
    consoleErrors.allow(/status of 503.*\/api\/payment\/confirm/)

    const orderStatuses = ['processing', 'paid']
    mock.api.on('POST /api/payment/confirm', (_call, route) =>
      route.fulfill({
        status: 503,
        headers: { 'Access-Control-Allow-Origin': '*', 'Content-Type': 'application/json' },
        body: JSON.stringify({ success: false, error: '결제 결과를 확인하지 못했습니다.' }),
      }),
    )
    mock.api.on('GET /api/payment/orders/*', (call, route) =>
      route.fulfill({
        status: 200,
        headers: { 'Access-Control-Allow-Origin': '*', 'Content-Type': 'application/json' },
        body: JSON.stringify({ order: { orderId: call.path.split('/').pop(), status: orderStatuses.shift() ?? 'paid' } }),
      }),
    )

    await mock.loginAs(ALICE)
    // 요금제 화면에서 진입 (직접 URL 진입은 아래 fixme 참고)
    await page.goto('/pricing')
    await expect(page.locator('.user-email')).toHaveText(ALICE.email)
    await page.locator('.standard-button').click()
    await expect(page).toHaveURL(/\/payment\?plan=standard$/)
    await expect(page.getByRole('heading', { name: '결제하기' })).toBeVisible()
    const pay = page.locator('.payment-submit-btn')
    await expect(pay).toBeDisabled()
    // 실제 체크박스는 display:none — 사용자가 누르는 체크 표시를 누른다
    await page.locator('.agreement-checkbox .checkmark').click()
    await expect(page.locator('.agreement-checkbox input[type=checkbox]')).toBeChecked()
    await pay.click()

    // 토스 결제창(스텁) → successUrl → 승인 503 → 결제 확인 중
    await expect(page).toHaveURL(/\/payment\/success\?planType=standard&paymentKey=pk_e2e_test&orderId=order_e2e_standard_1&amount=2900/)
    await expect(page.getByRole('status')).toContainText('결제 확인 중')
    // 결과 미확정 상태는 새로고침 복원용으로 저장된다
    expect(await page.evaluate(() => localStorage.getItem('bandspace.payment.checking'))).toContain('order_e2e_standard_1')

    // 조회: processing → paid (폴링 2초 + 4초)
    await expect(page.getByRole('heading', { name: '결제가 완료되었습니다!' })).toBeVisible({ timeout: 15_000 })
    await expect(page.locator('.payment-info')).toContainText('Standard 플랜')
    expect(await page.evaluate(() => localStorage.getItem('bandspace.payment.checking'))).toBeNull()

    // 서버 호출 순서와 본인 확인
    const order = mock.api.callsTo('POST', '/api/payment/orders')
    expect(order).toHaveLength(1)
    expect(order[0]).toMatchObject({ userId: ALICE.id, body: { planType: 'standard' } })
    const confirm = mock.api.callsTo('POST', '/api/payment/confirm')
    expect(confirm).toHaveLength(1)
    expect(confirm[0]).toMatchObject({
      userId: ALICE.id,
      body: { paymentKey: 'pk_e2e_test', orderId: 'order_e2e_standard_1', amount: 2900, planType: 'standard' },
    })
    expect(mock.api.callsTo('GET', '/api/payment/orders/*').map(c => c.path)).toEqual([
      '/api/payment/orders/order_e2e_standard_1',
      '/api/payment/orders/order_e2e_standard_1',
    ])
  })

  // 알려진 버그(앱 동작 변경은 이 PR 범위 밖): 로그인 상태로 /payment?plan=… 을 새로 열면 인증 확인 전(user=null)에
  // Payment가 /auth로 보내고, Auth는 로그인돼 있으면 from 대신 '/'로 보낸다 → 결제 화면에 도달하지 못한다.
  test.fixme('로그인 상태에서 결제 URL을 직접 열면 결제 화면이 보인다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.goto('/payment?plan=standard')
    await expect(page.getByRole('heading', { name: '결제하기' })).toBeVisible()
  })
})

test.describe('음원 분리', () => {
  test('로그인 사용자의 분리 히스토리를 보여 준다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.goto('/karaoke')
    await page.getByRole('button', { name: '이전 분리 결과 보기' }).click()

    const panel = page.locator('.history-panel')
    await expect(panel.getByRole('heading', { name: '분리 히스토리' })).toBeVisible()
    await expect(panel.locator('.history-filename')).toHaveText(STEM_HISTORY.map(s => s.original_filename))
    await expect(panel.locator('.history-item')).toHaveCount(STEM_HISTORY.length)
    expect(mock.api.callsTo('GET', '/api/music/stem-history').map(c => c.userId)).toEqual([ALICE.id])

    await page.getByRole('button', { name: '히스토리 닫기' }).click()
    await expect(panel).toHaveCount(0)
  })

  test('비로그인이면 히스토리 버튼이 없다', async ({ page, mock }) => {
    await page.goto('/karaoke')
    await expect(page.getByRole('button', { name: '🎵 음악재생 방' })).toBeVisible()
    await expect(page.getByRole('button', { name: '이전 분리 결과 보기' })).toHaveCount(0)
    expect(mock.api.callsTo('GET', '/api/music/stem-history')).toHaveLength(0)
  })
})
