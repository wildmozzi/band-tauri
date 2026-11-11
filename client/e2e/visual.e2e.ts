/**
 * 시각 회귀(P3-8): 주요 화면 6개를 스크린샷 기준 이미지와 비교한다.
 * 기준 이미지는 디자인 토큰·공용 컴포넌트로 바꾸기 **전** 코드에서 만들었다 — 리팩터가 화면을 바꾸면 실패한다.
 *
 * - 기준 이미지: e2e/__screenshots__/visual.e2e.ts/<이름>-<platform>.png (폰트 렌더링이 OS마다 달라 플랫폼별로 둔다.
 *   CI는 linux, 로컬 개발은 win32)
 * - 임계: 픽셀 비율 0.1%(maxDiffPixelRatio 0.001), 픽셀 색 차이 허용 threshold 0.2(Playwright 기본) —
 *   안티앨리어싱 잡음만 흡수하고 색·간격·레이아웃 변화는 잡는다.
 * - 상대 시간('n일 전')이 고정되게 브라우저 시계를, 게스트 닉네임(Guest+난수)이 고정되게 Math.random을 고정하고,
 *   애니메이션·캐럿은 끈다(toHaveScreenshot 기본).
 * - 화면을 일부러 바꾼 PR은 `npm run e2e -- visual --update-snapshots`로 기준을 갱신하고 PR에 이유를 적는다
 *   (linux 기준은 CI 아티팩트의 *-actual.png로 갱신).
 */
import type { Page } from '@playwright/test'
import { GUITAR_POST, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

const FIXED_NOW = new Date('2026-11-10T12:00:00.000Z')
const SHOT = { fullPage: true, maxDiffPixelRatio: 0.001 } as const

test.use({ viewport: { width: 1280, height: 800 } })

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(FIXED_NOW)
  // 게스트 닉네임(Guest+난수) 등 화면에 찍히는 난수를 고정한다 (Park–Miller LCG)
  await page.addInitScript(() => {
    let seed = 42
    Math.random = () => {
      seed = (seed * 16807) % 2147483647
      return (seed - 1) / 2147483646
    }
  })
})

/** 웹폰트·이미지 로딩이 끝난 뒤 찍는다 */
async function settle(page: Page) {
  await page.evaluate(() => document.fonts.ready)
  await page.waitForLoadState('networkidle')
}

test('홈', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('heading', { level: 1, name: '온라인 합주실' })).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('home.png', SHOT)
})

test('합주실 목록', async ({ page }) => {
  await page.goto('/rooms')
  await expect(page.locator('.room-card').filter({ hasText: ROCK_ROOM.title })).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('rooms.png', SHOT)
})

test('합주실 상세(웹 관람)', async ({ page, mock }) => {
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
  await expect(page.locator('.web-notice')).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('room-detail-spectator.png', SHOT)
})

test('커뮤니티 목록', async ({ page }) => {
  await page.goto('/community')
  await expect(page.getByRole('link', { name: GUITAR_POST.title }).first()).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('community.png', SHOT)
})

test('글 상세', async ({ page }) => {
  await page.goto(`/community/${GUITAR_POST.id}`)
  await expect(page.getByRole('heading', { level: 1, name: GUITAR_POST.title })).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('post-detail.png', SHOT)
})

test('404', async ({ page }) => {
  await page.goto('/no-such-page')
  await expect(page.getByRole('heading', { level: 1, name: '페이지를 찾을 수 없습니다' })).toBeVisible()
  await settle(page)
  await expect(page).toHaveScreenshot('not-found.png', SHOT)
})
