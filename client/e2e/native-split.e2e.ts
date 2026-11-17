/**
 * 엔진 런타임 묶음 분리(perf/web-native-split): 연주 세션 본체·관람 방송(nativeRuntime-*.js)은 데스크톱 앱에서만 받는다.
 * 웹 관람(합주실 첫 화면)은 받지 않고, 데스크톱 앱(가짜 Tauri)은 첫 그리기 전에 받아 두어 연주 시작이 그대로 된다.
 */
import { BOB, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { installFakeTauri, tauriCalls } from './mocks/tauri'
import { expect, test } from './test'

const isRuntimeChunk = (url: string) => /\/assets\/nativeRuntime-[^/]+\.js$/.test(url)

test('웹 관람: 합주실 첫 화면에서 엔진 런타임 묶음을 받지 않는다', async ({ page, mock }) => {
  const requested: string[] = []
  page.on('request', r => { if (isRuntimeChunk(r.url())) requested.push(r.url()) })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.waitForLoadState('networkidle')
  expect(requested).toEqual([])
})

test('데스크톱 앱: 엔진 런타임을 받아 두고, 연주 참여가 엔진을 바로 시작한다', async ({ page, mock }) => {
  const requested: string[] = []
  page.on('request', r => { if (isRuntimeChunk(r.url())) requested.push(r.url()) })
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: { running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [] },
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  expect(requested).toHaveLength(1)

  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  expect((await tauriCalls(page)).filter(c => c === 'engine_start')).toHaveLength(1)
  await expect(page.getByRole('button', { name: /관람자로 전환/ })).toBeVisible()
  expect(requested).toHaveLength(1)
})
