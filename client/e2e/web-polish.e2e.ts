/**
 * P3-12 웹 점검 발견 사항(ego-browser, 2026-11-10) 회귀 테스트.
 * 번호는 docs/BACKLOG.md P3-12 행의 ①~⑪.
 */
import type { Page } from '@playwright/test'
import { ALICE, GUITAR_POST, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

/** 렌더 중 navigate() 호출 시 React Router가 남기는 경고를 모은다 (⑥) */
function collectNavigateWarnings(page: Page): string[] {
  const warnings: string[] = []
  page.on('console', msg => {
    if (msg.type() === 'warning' && /navigate\(\)/.test(msg.text())) warnings.push(msg.text())
  })
  return warnings
}

test.describe('탭 제목·서비스명 (①②)', () => {
  test('라우트마다 IndigoMuse 탭 제목, 이전 이름(BandSpace)과 템플릿 제목이 없다', async ({ page }) => {
    await page.goto('/')
    await expect(page).toHaveTitle('IndigoMuse — 온라인 합주실')
    await expect(page.locator('nav .brand')).toContainText('IndigoMuse')
    await expect(page.locator('.app-footer')).toContainText('IndigoMuse')
    await expect(page.locator('body')).not.toContainText('BandSpace')

    await page.locator('nav').getByRole('link', { name: '합주실' }).click()
    await expect(page).toHaveTitle('IndigoMuse — 합주실')
    await page.locator('nav').getByRole('link', { name: '커뮤니티' }).click()
    await expect(page).toHaveTitle('IndigoMuse — 커뮤니티')

    await page.goto('/auth')
    await expect(page).toHaveTitle('IndigoMuse — 로그인')
    await expect(page.locator('.auth-header h1')).toHaveText('IndigoMuse')
    await expect(page.locator('body')).not.toContainText('BandSpace')
  })
})

test.describe('홈 문구 (③④)', () => {
  test('근거 없는 수치·리전·경쟁사명이 없고 사실만 적는다', async ({ page }) => {
    await page.goto('/')
    const body = page.locator('body')
    await expect(page.getByRole('heading', { level: 1, name: '온라인 합주실' })).toBeVisible()
    for (const banned of [/18\s*ms/, /3\s*지역/, /KR · JP · US/, /서울 · 도쿄 · LA/, /20ms 이하/, /SYNCROOM/i]) {
      await expect(body).not.toContainText(banned)
    }
    await expect(page.locator('.hero-stats')).toContainText('최대 6명')
    await expect(page.locator('.hero-stats')).toContainText('웹 관람')
  })

  test('홈에 데스크톱 앱 다운로드 버튼 (VITE_DESKTOP_DOWNLOAD_URL)', async ({ page }) => {
    await page.goto('/')
    const download = page.locator('.hero-actions').getByRole('link', { name: '📥 데스크톱 앱 다운로드' })
    await expect(download).toHaveAttribute('href', 'https://download.e2e.test/band-desktop')
    await expect(download).toHaveAttribute('target', '_blank')
  })
})

test.describe('커뮤니티 목록 링크 (⑤)', () => {
  test('글 제목이 실제 링크라 새 탭으로 열 수 있고, 키보드로 이동한다', async ({ page }) => {
    await page.goto('/community')
    const link = page.getByRole('link', { name: GUITAR_POST.title }).first()
    await expect(link).toHaveAttribute('href', `/community/${GUITAR_POST.id}`)

    await link.focus()
    await page.keyboard.press('Enter')
    await expect(page).toHaveURL(new RegExp(`/community/${GUITAR_POST.id}$`))
    await expect(page.getByRole('heading', { level: 1, name: GUITAR_POST.title })).toBeVisible()
  })

  test('카드의 빈 곳을 눌러도 상세로 가고, 좋아요는 이동하지 않는다', async ({ page }) => {
    await page.goto('/community')
    const like = page.getByTestId(`post-like-${GUITAR_POST.id}`)
    page.once('dialog', d => void d.dismiss()) // 비로그인 좋아요 → 로그인 안내 alert
    await like.click()
    await expect(page).toHaveURL(/\/community$/)

    const card = page.locator('article.community-post-card', { has: page.getByTestId(`post-like-${GUITAR_POST.id}`) })
    const box = await card.boundingBox()
    if (!box) throw new Error('card not visible')
    await page.mouse.click(box.x + box.width - 8, box.y + box.height - 8)
    await expect(page).toHaveURL(new RegExp(`/community/${GUITAR_POST.id}$`))
  })
})

test.describe('렌더 중 navigate 제거 (⑥)', () => {
  test('비로그인 /profile → /auth, 경고 없음', async ({ page }) => {
    const warnings = collectNavigateWarnings(page)
    await page.goto('/profile')
    await expect(page).toHaveURL(/\/auth$/)
    await expect(page.locator('form.auth-form')).toBeVisible()
    expect(warnings).toEqual([])
  })

  test('/payment(플랜 없음) → /pricing, 경고 없음', async ({ page }) => {
    const warnings = collectNavigateWarnings(page)
    await page.goto('/payment')
    await expect(page).toHaveURL(/\/pricing$/)
    expect(warnings).toEqual([])
  })

  test('로그인 상태로 /profile 직접 진입하면 프로필이 보인다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.goto('/profile')
    await expect(page.getByRole('heading', { name: '프로필 설정' })).toBeVisible()
    await expect(page).toHaveURL(/\/profile$/)
    await expect(page).toHaveTitle('IndigoMuse — 프로필')
  })
})

test('없는 경로는 404 화면 (⑦)', async ({ page }) => {
  await page.goto('/no-such-page')
  await expect(page.getByRole('heading', { level: 1, name: '페이지를 찾을 수 없습니다' })).toBeVisible()
  await expect(page).toHaveTitle('IndigoMuse — 페이지를 찾을 수 없음')
  await page.locator('.not-found-page').getByRole('link', { name: '홈으로' }).click()
  await expect(page).toHaveURL(/\/$/)
})

test('방 만들기: 최대 인원은 관람 포함, 연주자는 최대 6명이라고 안내한다 (⑧)', async ({ page, mock }) => {
  await mock.loginAs(ALICE)
  await page.goto('/rooms/create')
  const select = page.getByLabel('최대 인원 (관람 포함)')
  await expect(select).toHaveAccessibleDescription('연주자는 최대 6명이고, 나머지 인원은 관람으로 입장합니다.')
  await expect(select.locator('option:checked')).toHaveText('8명 (연주 6 + 관람 2)')
  await expect(select.locator('option').first()).toHaveText('2명 (모두 연주 가능)')
})

test('합주실: 채팅 입력창이 스크롤 없이 첫 화면에 보인다 (⑨)', async ({ page }) => {
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  const chat = page.locator('.chat-panel')
  await expect(chat.getByPlaceholder('메시지 입력...')).toBeInViewport({ ratio: 1 })
  await expect(chat.getByRole('button', { name: '전송' })).toBeInViewport({ ratio: 1 })
  expect(await page.evaluate(() => window.scrollY)).toBe(0)
})

test.describe('결제 (⑩⑪)', () => {
  test('비로그인으로 결제 URL을 열면 로그인 후 그 결제 화면으로 돌아온다', async ({ page }) => {
    await page.goto('/payment?plan=pro')
    await expect(page).toHaveURL(/\/auth$/)
    await page.getByPlaceholder('you@example.com').fill(ALICE.email)
    await page.getByPlaceholder('8자 이상 입력').fill(ALICE.password)
    await page.locator('form.auth-form').getByRole('button', { name: '로그인' }).click()
    await expect(page).toHaveURL(/\/payment\?plan=pro$/)
    await expect(page.getByRole('heading', { name: '결제하기' })).toBeVisible()
  })

  test('약관 체크박스는 접근 가능한 이름이 있고 키보드로 켤 수 있다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.goto('/payment?plan=standard')
    const agree = page.getByRole('checkbox', { name: /이용약관.*개인정보처리방침에 동의합니다/ })
    await expect(agree).not.toBeChecked()
    await expect(page.locator('.payment-submit-btn')).toBeDisabled()

    await agree.focus()
    await expect(agree).toBeFocused()
    await page.keyboard.press('Space')
    await expect(agree).toBeChecked()
    await expect(page.locator('.payment-submit-btn')).toBeEnabled()
  })
})
