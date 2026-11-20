/**
 * 지연 로딩 라우트 진입 smoke (P3-2 ③-2): React.lazy로 나눈 화면(데모 3종·커뮤니티·프로필·결제)이
 * 직접 진입과 앱 안 이동(내비게이션 링크) 모두에서 그려지고, 탭 제목이 맞는지 본다. 콘솔 에러 0은 공통 fixture가 검사한다.
 */
import type { Page } from '@playwright/test'
import { ALICE, GUITAR_POST } from './fixtures'
import { expect, test } from './test'

const h1 = (name: string) => (p: Page) => p.getByRole('heading', { level: 1, name })

const LAZY_ROUTES = [
  { path: '/recording', title: '녹음', ready: h1('개인 레코딩 스튜디오') },
  { path: '/mix', title: 'Mix Lab', ready: h1('Mix Lab') },
  { path: '/karaoke', title: '노래방', ready: h1('노래방 (데모)') },
  { path: '/community', title: '커뮤니티', ready: (p: Page) => p.getByRole('link', { name: GUITAR_POST.title }).first() },
  { path: `/community/${GUITAR_POST.id}`, title: '커뮤니티', ready: h1(GUITAR_POST.title) },
  { path: '/community/create', title: '글쓰기', login: true, ready: h1('새 게시물 작성') },
  { path: `/community/edit/${GUITAR_POST.id}`, title: '글 수정', login: true, ready: h1('게시물 수정') },
  { path: '/profile', title: '프로필', login: true, ready: (p: Page) => p.getByRole('heading', { name: '프로필 설정' }) },
  { path: '/pricing', title: '요금제', ready: h1('합주의 새로운 경험을 시작하세요') },
  { path: '/payment?plan=standard', title: '결제', login: true, ready: (p: Page) => p.getByRole('heading', { name: '결제하기' }) },
  { path: '/payment/success', title: '결제 완료', ready: h1('확인할 결제 정보가 없습니다') },
  { path: '/payment/fail', title: '결제 실패', ready: h1('표시할 결제 실패 정보가 없습니다') },
]

test.describe('지연 로딩 라우트 직접 진입', () => {
  for (const route of LAZY_ROUTES) {
    test(`${route.path}`, async ({ page, mock }) => {
      if (route.login) await mock.loginAs(ALICE)
      await page.goto(route.path)
      await expect(route.ready(page)).toBeVisible()
      await expect(page).toHaveTitle(new RegExp(`— ${route.title}$`))
    })
  }
})

test('내비게이션으로 즉시 로드 화면 ↔ 지연 로딩 화면을 오간다', async ({ page }) => {
  await page.goto('/')
  await expect(h1('온라인 합주실')(page)).toBeVisible()
  const nav = page.locator('nav')

  await nav.getByRole('link', { name: '커뮤니티' }).click()
  await expect(page.getByRole('link', { name: GUITAR_POST.title }).first()).toBeVisible()
  await nav.getByRole('link', { name: '노래방(데모)' }).click()
  await expect(h1('노래방 (데모)')(page)).toBeVisible()
  await nav.getByRole('link', { name: 'Mix Lab' }).click()
  await expect(h1('Mix Lab')(page)).toBeVisible()
  await nav.getByRole('link', { name: '녹음' }).click()
  await expect(h1('개인 레코딩 스튜디오')(page)).toBeVisible()
  // 합주 경로(즉시 로드)로 돌아온다
  await nav.getByRole('link', { name: '합주실', exact: true }).click()
  await expect(page).toHaveURL(/\/rooms$/)
  await expect(page.locator('.room-card').first()).toBeVisible()
})
