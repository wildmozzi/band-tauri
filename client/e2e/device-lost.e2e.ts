/**
 * 오디오 장치 분실 뒤 자동 재시작(#206 후보). 엔진이 deviceLost(running=false)를 보고하면 합주를 끝내지 않고
 * 엔진만 다시 연다 — 안내 띠, 서버에 연주 중단(stop-performing)을 보내지 않음, 다시 start-performing.
 * 실제 엔진은 돌지 않는다(가짜 Tauri). 실제 장치 전환 청취는 [사람] 항목.
 */
import type { EngineStats } from '../src/native/engineApi'
import { BOB, ROCK_ROOM } from './fixtures'
import { installFakeTauri, setTauriResponse, tauriCalls } from './mocks/tauri'
import { expect, test } from './test'

const stats = (over: Partial<EngineStats> = {}): EngineStats => ({
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [], ...over,
})

test('연주 중 오디오 장치를 잃으면 안내 띠를 보이고 엔진만 다시 시작한다 — 연주는 계속된다', async ({ page, mock }) => {
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

  // 헤드폰을 꽂아 기본 장치가 바뀌었다: 엔진이 장치를 잃고 멈춤. 다시 시작한 엔진은 정상 통계를 낸다
  await setTauriResponse(page, '__engineStatsAfterStart', stats())
  await setTauriResponse(page, 'engine_stats', stats({ running: false, deviceLost: true }))
  const banner = page.locator('.device-notice')
  await expect(banner).toContainText('엔진을 다시 시작하는 중', { timeout: 10_000 })
  await expect(banner).toContainText('엔진을 다시 시작했습니다. 합주는 계속됩니다.', { timeout: 10_000 })

  // 엔진만 다시 열었다: 시작 2번, 서버에는 다시 start-performing(연주 중단 알림 없음)
  expect((await tauriCalls(page)).filter(c => c === 'engine_start')).toHaveLength(2)
  expect(mock.signaling.messages('start-performing')).toHaveLength(2)
  expect(mock.signaling.messages('stop-performing')).toHaveLength(0)
  // 연주 중 화면 그대로(관람자로 전환 버튼 = 연주 중)
  await expect(page.getByRole('button', { name: /관람자로 전환/ })).toBeVisible()
})
