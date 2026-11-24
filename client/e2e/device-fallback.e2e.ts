/**
 * 저장된 오디오 장치가 이 PC에 없는 채로 연주 시작(P2-27). 다른 PC에서 고른 인터페이스(계정 동기화)나 뽑아 둔 인터페이스가
 * 저장돼 있어도 이번 실행만 기본 장치로 엔진을 시작하고 합주실에 알림 띠를 보인다. 저장된 설정은 그대로 둔다.
 * 실제 엔진은 돌지 않는다(가짜 Tauri). 실제 인터페이스를 뽑고 시작하는 확인은 [사람] 항목.
 */
import type { DeviceInfo, EngineConfig, EngineStats } from '../src/native/engineApi'
import { DEFAULT_ENGINE_CONFIG } from '../src/native/engineConfig'
import { BOB, ROCK_ROOM } from './fixtures'
import { installFakeTauri, tauriCallArgs } from './mocks/tauri'
import { expect, test } from './test'

/** 장치 쪽 값(드라이버·장치·입력 채널·버퍼)은 이 PC에만 저장한다(P2-27 후속) */
const STORAGE_KEY = 'band_device_settings_v1'
const SAVED_IN = 'wasapi::Focusrite USB Input'
const SAVED_OUT = 'wasapi::Focusrite USB Output'

const stats: EngineStats = {
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [],
}
const devices: DeviceInfo[] = [
  { id: 'wasapi::내장 마이크', name: '내장 마이크', driver: 'wasapiShared', isInput: true, isDefault: true, channels: 2, sampleRates: [48000] },
  { id: 'wasapi::스피커', name: '스피커', driver: 'wasapiShared', isInput: false, isDefault: true, channels: 2, sampleRates: [48000] },
]

test('저장된 장치가 없는 PC에서 연주 시작: 이번 실행만 기본 장치로 엔진을 열고 알림 띠, 저장값은 그대로', async ({ page, mock }) => {
  const saved: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: SAVED_IN, outputDevice: SAVED_OUT }
  await page.addInitScript(
    ([key, stored]: [string, string]) => localStorage.setItem(key, stored),
    [STORAGE_KEY, JSON.stringify(saved)] as [string, string],
  )
  await installFakeTauri(page, {
    engine_supported_drivers: ['wasapiExclusive', 'wasapiShared'],
    engine_list_devices: devices,
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)

  // 엔진에는 기본 장치(null)로 넘긴다
  const starts = (await tauriCallArgs(page, 'engine_start')) as { config: EngineConfig }[]
  expect(starts).toHaveLength(1)
  expect(starts[0].config).toMatchObject({ inputDevice: null, outputDevice: null, inputDriver: 'wasapiShared' })

  const banner = page.getByTestId('audio-fallback-notice')
  await expect(banner).toContainText("저장된 장치 'Focusrite USB Input', 'Focusrite USB Output'를 찾을 수 없어 기본 장치로 시작했습니다.")
  await expect(banner).toContainText('저장된 설정은 그대로 둡니다')

  // 저장된 설정(이 PC의 장치 설정)은 그대로
  const stored = await page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null') as EngineConfig, STORAGE_KEY)
  expect(stored).toMatchObject({ inputDevice: SAVED_IN, outputDevice: SAVED_OUT })

  // 닫으면 사라진다
  await banner.getByRole('button', { name: '닫기' }).click()
  await expect(banner).toHaveCount(0)
})

test('이 PC에 장치가 하나도 없으면 엔진을 열지 않고 원인과 할 일을 보인다', async ({ page, mock }) => {
  await installFakeTauri(page, {
    engine_supported_drivers: ['wasapiExclusive', 'wasapiShared'],
    engine_list_devices: [],
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()

  await expect(page.getByText(/이 PC에서 사용할 수 있는 입력 장치\(WASAPI 공유\)와 출력 장치\(WASAPI 공유\)를 찾지 못했습니다/)).toBeVisible()
  expect(await tauriCallArgs(page, 'engine_start')).toHaveLength(0)
  expect(mock.signaling.messages('start-performing')).toHaveLength(0)
})

test('테스트 모니터를 먼저 켜도 합주실 띠가 뜬다 — 테스트 모니터 알림과 합주실 띠는 따로 센다(#251 리뷰 Low-5)', async ({ page, mock }) => {
  const saved: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: SAVED_IN, outputDevice: SAVED_OUT }
  await page.addInitScript(
    ([key, stored]: [string, string]) => localStorage.setItem(key, stored),
    [STORAGE_KEY, JSON.stringify(saved)] as [string, string],
  )
  await installFakeTauri(page, {
    engine_supported_drivers: ['wasapiExclusive', 'wasapiShared'],
    engine_list_devices: devices,
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()

  // 같은 앱 실행에서 오디오 설정 → 테스트 모니터를 먼저 켠다(폴백 알림이 테스트 모니터 쪽에 뜬다)
  await page.getByRole('button', { name: '오디오 설정' }).click()
  await page.getByRole('button', { name: '테스트 시작' }).click()
  await expect(page.getByTestId('driver-notice')).toContainText('기본 장치로 시작했습니다')
  expect((await tauriCallArgs(page, 'engine_test_start')) as { config: EngineConfig }[]).toHaveLength(1)
  await page.getByRole('button', { name: '완료' }).click()

  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  await expect(page.getByTestId('audio-fallback-notice')).toContainText('기본 장치로 시작했습니다')
})
