/**
 * 오디오 장치 분실 뒤 자동 재시작(#206 후보). 엔진이 deviceLost(running=false)를 보고하면 합주를 끝내지 않고
 * 엔진만 다시 연다 — 안내 띠, 서버에 연주 중단(stop-performing)을 보내지 않음, 다시 start-performing,
 * 다시 여는 동안에도 연주자 화면 유지(연주 참여하기 버튼이 나타나지 않음).
 * 실제 엔진은 돌지 않는다(가짜 Tauri). 실제 장치 전환 청취는 [사람] 항목.
 *
 * 재시작은 0.5초 남짓이라 "다시 시작하는 중" 띠를 Playwright 폴링이 놓칠 수 있다(#208 리뷰 M-2): 페이지 안 MutationObserver가
 * 띠 문구·연주 참여 버튼의 변화를 모두 기록하고, 테스트는 그 기록과 마지막 상태로 단언한다.
 */
import type { Page } from '@playwright/test'
import type { EngineStats } from '../src/native/engineApi'
import { BOB, ROCK_ROOM } from './fixtures'
import { installFakeTauri, setTauriResponse, tauriCalls } from './mocks/tauri'
import { expect, test } from './test'

const stats = (over: Partial<EngineStats> = {}): EngineStats => ({
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [], ...over,
})

interface UiSample { notice: string | null; joinButton: boolean }

/** 띠 문구·"연주 참여하기" 버튼이 바뀔 때마다 기록한다(goto 전에 부른다) */
async function recordUi(page: Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { __e2eUi: UiSample[] }
    w.__e2eUi = []
    const sample = () => {
      const notice = document.querySelector('.device-notice')?.textContent ?? null
      const joinButton = Array.from(document.querySelectorAll('button')).some(b => b.textContent?.includes('연주 참여하기'))
      const last = w.__e2eUi[w.__e2eUi.length - 1]
      if (!last || last.notice !== notice || last.joinButton !== joinButton) w.__e2eUi.push({ notice, joinButton })
    }
    new MutationObserver(sample).observe(document, { subtree: true, childList: true, characterData: true })
  })
}
const uiHistory = (page: Page) => page.evaluate(() => (window as unknown as { __e2eUi: UiSample[] }).__e2eUi)

test('연주 중 오디오 장치를 잃으면 안내 띠를 보이고 엔진만 다시 시작한다 — 연주자 화면·연주는 계속된다', async ({ page, mock }) => {
  await recordUi(page)
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats(),
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  await expect(page.getByRole('button', { name: /관람자로 전환/ })).toBeVisible()
  const before = (await uiHistory(page)).length

  // 헤드폰을 꽂아 기본 장치가 바뀌었다: 엔진이 장치를 잃고 멈춤. 다시 시작한 엔진은 정상 통계를 낸다
  await setTauriResponse(page, '__engineStatsAfterStart', stats())
  await setTauriResponse(page, 'engine_stats', stats({ running: false, deviceLost: true }))

  // 마지막 상태: 성공 띠, 엔진 시작 2회
  const banner = page.locator('.device-notice')
  await expect(banner).toContainText('엔진을 다시 시작했습니다. 합주는 계속됩니다.', { timeout: 10_000 })
  await expect.poll(async () => (await tauriCalls(page)).filter(c => c === 'engine_start').length).toBe(2)

  // 기록: "다시 시작하는 중" → "다시 시작했습니다" 순서로 보였고, 그동안 연주 참여 버튼(관람자 화면)은 나타나지 않았다
  const history = (await uiHistory(page)).slice(before)
  const notices = history.map(h => h.notice).filter((n): n is string => n !== null)
  const recovering = notices.findIndex(n => n.includes('엔진을 다시 시작하는 중'))
  const recovered = notices.findIndex(n => n.includes('엔진을 다시 시작했습니다'))
  expect(recovering, JSON.stringify(history)).toBeGreaterThanOrEqual(0)
  expect(recovered).toBeGreaterThan(recovering)
  expect(history.some(h => h.joinButton), JSON.stringify(history)).toBe(false)

  // 서버에는 다시 start-performing(연주 중단 알림 없음), 연주자 화면 그대로
  expect(mock.signaling.messages('start-performing')).toHaveLength(2)
  expect(mock.signaling.messages('stop-performing')).toHaveLength(0)
  await expect(page.getByRole('button', { name: /관람자로 전환/ })).toBeVisible()
})
