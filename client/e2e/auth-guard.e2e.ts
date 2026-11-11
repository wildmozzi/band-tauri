/**
 * P3-10f: 로그인이 필요한 페이지의 공통 가드(useRequireAuth).
 * - 로그인 상태로 주소창·새로고침으로 직접 들어오면 세션 복원을 기다린 뒤 그 페이지를 보여 준다(/auth로 튕기지 않는다).
 * - 비로그인이면 /auth로 보내고, 로그인하면 원래 경로(쿼리 포함)로 돌아온다.
 */
import type { Page } from '@playwright/test'
import { ALICE, GUITAR_POST } from './fixtures'
import { expect, test } from './test'

interface GuardedRoute {
  name: string
  path: string
  /** 페이지가 실제로 보였는지 확인하는 요소 */
  ready: (page: Page) => ReturnType<Page['getByRole']>
}

const ROUTES: GuardedRoute[] = [
  { name: '글쓰기', path: '/community/create', ready: p => p.getByRole('heading', { level: 1, name: '새 게시물 작성' }) },
  { name: '글 수정', path: `/community/edit/${GUITAR_POST.id}`, ready: p => p.getByRole('heading', { level: 1, name: '게시물 수정' }) },
  { name: '프로필', path: '/profile', ready: p => p.getByRole('heading', { name: '프로필 설정' }) },
  { name: '결제', path: '/payment?plan=standard', ready: p => p.getByRole('heading', { name: '결제하기' }) },
  { name: '방 만들기', path: '/rooms/create', ready: p => p.getByRole('heading', { level: 1, name: '새 합주실 만들기' }) },
]

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

for (const route of ROUTES) {
  test.describe(`로그인 필요 페이지: ${route.name} (${route.path})`, () => {
    test('로그인 상태로 직접 진입·새로고침하면 그 페이지가 보인다', async ({ page, mock }) => {
      await mock.loginAs(ALICE)
      // 어느 시점에라도 /auth로 이동하면 기록한다(잠깐 튕겼다 돌아오는 것도 실패)
      const visited: string[] = []
      // (framenavigated는 history API로 바뀐 같은 문서 안 이동도 알린다)
      page.on('framenavigated', f => {
        if (f === page.mainFrame()) visited.push(new URL(f.url()).pathname)
      })

      await page.goto(route.path)
      await expect(route.ready(page)).toBeVisible()
      await expect(page).toHaveURL(new RegExp(`${escape(route.path)}$`))

      await page.reload()
      await expect(route.ready(page)).toBeVisible()
      await expect(page).toHaveURL(new RegExp(`${escape(route.path)}$`))
      expect(visited.filter(p => p === '/auth')).toEqual([])
    })

    test('비로그인이면 /auth로 가고, 로그인하면 원래 경로로 돌아온다', async ({ page }) => {
      await page.goto(route.path)
      await expect(page).toHaveURL(/\/auth$/)
      await page.getByPlaceholder('you@example.com').fill(ALICE.email)
      await page.getByPlaceholder('8자 이상 입력').fill(ALICE.password)
      await page.locator('form.auth-form').getByRole('button', { name: '로그인' }).click()
      await expect(page).toHaveURL(new RegExp(`${escape(route.path)}$`))
      await expect(route.ready(page)).toBeVisible()
    })
  })
}

// 리뷰 L2: 프로필 화면 상태(bio·SNS)는 user에서 초기값을 받는다. 세션 복원 전에 화면을 만들면 빈 값으로 굳는다
test('로그인 상태로 /profile 직접 진입·새로고침하면 자기소개·SNS가 계정 값으로 채워진다', async ({ page, mock }) => {
  await mock.loginAs(ALICE)
  const bio = page.getByPlaceholder('자신을 소개해주세요')
  const instagram = page.getByPlaceholder('Instagram 사용자명')

  await page.goto('/profile')
  await expect(bio).toHaveValue(ALICE.bio!)
  await expect(instagram).toHaveValue(ALICE.instagram!)

  await page.reload()
  await expect(bio).toHaveValue(ALICE.bio!)
  await expect(instagram).toHaveValue(ALICE.instagram!)
})

test('세션 확인이 끝나기 전에는 /auth로 보내지 않는다 (사용자 조회를 붙잡아도 글쓰기 화면 유지)', async ({ page, mock }) => {
  await mock.loginAs(ALICE)
  const held = mock.supabase.holdAuthUser()
  await page.goto('/community/create')
  await expect.poll(() => held.waiting()).toBeGreaterThan(0)
  // 조회가 붙잡힌 동안: /auth로 이동하지 않는다
  await page.waitForTimeout(300)
  await expect(page).toHaveURL(/\/community\/create$/)
  held.release()
  await expect(page.getByRole('heading', { level: 1, name: '새 게시물 작성' })).toBeVisible()
  await expect(page).toHaveURL(/\/community\/create$/)
})
