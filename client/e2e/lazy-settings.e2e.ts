/**
 * 오디오 설정 화면 지연 로딩(perf/d-web-load). 웹 관람 첫 화면(합주실)에는 설정 묶음을 내려받지 않고,
 * 🎛️ 모달을 열 때 내려받는다. "지연 측정 열기"(메트로놈 패널) 이벤트는 처음 열 때도 그 절로 스크롤한다.
 */
import { REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { installFakeTauri } from './mocks/tauri'
import { expect, test } from './test'

const isSettingsChunk = (url: string) => /\/assets\/AudioSettings-[^/]+\.js$/.test(url)

test('합주실 첫 화면에서는 오디오 설정 묶음을 내려받지 않고, 모달을 열 때 내려받는다', async ({ page, mock }) => {
  const requested: string[] = []
  page.on('request', r => { if (isSettingsChunk(r.url())) requested.push(r.url()) })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.waitForLoadState('networkidle')
  expect(requested, '첫 화면에서 설정 묶음 요청').toEqual([])

  await page.getByRole('button', { name: '오디오 설정' }).click()
  await expect(page.getByRole('dialog', { name: '오디오 설정' })).toBeVisible()
  expect(requested).toHaveLength(1)
})

test('"지연 측정 열기" 이벤트는 모달을 처음 열 때(지연 로딩)도 지연 측정 절로 스크롤한다 (데스크톱 앱)', async ({ page }) => {
  // 지연 측정 절은 데스크톱 앱에만 있다(가짜 Tauri). 이벤트는 합주실 메트로놈 패널이 보낸다
  await installFakeTauri(page, { engine_list_devices: [] })
  await page.setViewportSize({ width: 1280, height: 600 })
  await page.goto('/rooms')
  await page.evaluate(() => window.dispatchEvent(new Event('band:open-latency-measure')))
  const section = page.locator('#latency-measure')
  await expect(section).toBeAttached()
  await expect(section).toBeInViewport()
})
