/**
 * 오디오 장치 설정은 PC별 저장(P2-27 후속, 사용자 결정 2026-11-21). 드라이버·장치·입력 채널·버퍼는 이 PC의 localStorage에만
 * 두고 계정(profiles.audio_settings)으로 보내지 않으며, 계정에서 온 값으로 덮어쓰지 않는다. 코덱·프레임·지터는 계정에 남는다.
 * 계정에 이 변경 전의 옛 장치 값이 있으면 이 PC에 저장값이 없을 때만 한 번 초기값으로 가져온다 — 그 장치가 이 PC에 없으면
 * #251의 폴백(이번 실행만 기본 장치)이 그대로 동작한다. 실제 엔진은 돌지 않는다(가짜 Tauri).
 */
import type { DeviceInfo, EngineConfig, EngineStats } from '../src/native/engineApi'
import { BOB, ROCK_ROOM } from './fixtures'
import type { MockBackend } from './mocks'
import { installFakeTauri, tauriCallArgs } from './mocks/tauri'
import { expect, test } from './test'

const DEVICE_KEY = 'band_device_settings_v1'
const DEVICE_FIELDS = ['inputDriver', 'outputDriver', 'inputDevice', 'outputDevice', 'inputChannels', 'bufferSize']

const stats: EngineStats = {
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [],
}
const dev = (id: string, isInput: boolean, isDefault = false): DeviceInfo => ({
  id, name: id.slice(id.indexOf('::') + 2), driver: 'wasapiShared', isInput, isDefault, channels: 2, sampleRates: [48000],
})
/** 이 PC: 내장 마이크·스피커 + 이 PC에 꽂힌 USB 인터페이스 */
const devices: DeviceInfo[] = [
  dev('wasapi::내장 마이크', true, true),
  dev('wasapi::스피커', false, true),
  dev('wasapi::이 PC USB 입력', true),
  dev('wasapi::이 PC USB 출력', false),
]
/** 다른 PC에서 저장한 계정 값(이 변경 전 형식 — 장치 필드 포함) */
const OTHER_PC_ACCOUNT = {
  version: 2,
  engine: {
    inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: 'wasapi::다른 PC 인터페이스', outputDevice: 'wasapi::다른 PC 인터페이스',
    inputChannels: [0], bufferSize: 256, codec: 'pcm16', frameMs: 2.5, channels: 1, jitterMinMs: 0, jitterMaxMs: 60,
  },
}

function setAccountSettings(mock: MockBackend, value: unknown) {
  const row = mock.supabase.db.table('profiles').find((p) => p.id === BOB.id)
  if (!row) throw new Error('BOB 프로필 없음')
  row.audio_settings = value
}

async function joinAndStart(page: import('@playwright/test').Page, mock: MockBackend) {
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  const starts = (await tauriCallArgs(page, 'engine_start')) as { config: EngineConfig }[]
  expect(starts).toHaveLength(1)
  return starts[0].config
}

test.beforeEach(async ({ page }) => {
  await installFakeTauri(page, {
    engine_supported_drivers: ['wasapiExclusive', 'wasapiShared'],
    engine_list_devices: devices,
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
})

test('다른 PC 장치 값이 계정에 있어도 이 PC 저장값으로 엔진을 연다 — 코덱·프레임은 계정 값, 폴백 띠 없음', async ({ page, mock }) => {
  setAccountSettings(mock, OTHER_PC_ACCOUNT)
  const here = { inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: 'wasapi::이 PC USB 입력', outputDevice: 'wasapi::이 PC USB 출력', inputChannels: [1], bufferSize: 64 }
  await page.addInitScript(([key, v]: [string, string]) => localStorage.setItem(key, v), [DEVICE_KEY, JSON.stringify(here)] as [string, string])

  const config = await joinAndStart(page, mock)
  expect(config).toMatchObject({ ...here, codec: 'pcm16', frameMs: 2.5, jitterMaxMs: 60 })
  await expect(page.getByTestId('audio-fallback-notice')).toHaveCount(0)

  // 이 PC 저장값은 그대로, 계정에는 쓰지 않았다(편집 없음)
  expect(await page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? 'null') as unknown, DEVICE_KEY)).toMatchObject(here)
  expect(mock.supabase.db.table('profiles').find((p) => p.id === BOB.id)?.audio_settings).toEqual(OTHER_PC_ACCOUNT)
})

test('저장값 없는 새 PC: 계정의 옛 장치 값을 한 번 가져오고, 그 장치가 없으니 이번 실행만 기본 장치(#251 폴백)', async ({ page, mock }) => {
  setAccountSettings(mock, OTHER_PC_ACCOUNT)

  const config = await joinAndStart(page, mock)
  expect(config).toMatchObject({ inputDevice: null, outputDevice: null, bufferSize: 256, codec: 'pcm16' })
  await expect(page.getByTestId('audio-fallback-notice')).toContainText("저장된 장치 '다른 PC 인터페이스'를 찾을 수 없어 기본 장치로 시작했습니다.")
  // 가져온 값은 이 PC 저장값이 된다(저장된 설정은 그대로 — 폴백은 저장하지 않는다)
  await expect
    .poll(() => page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? 'null') as Record<string, unknown> | null, DEVICE_KEY))
    .toMatchObject({ inputDevice: 'wasapi::다른 PC 인터페이스', bufferSize: 256 })
})

test('설정 화면: 이 PC에만 저장되는 값을 표시하고, 장치를 바꿔도 계정에는 장치 필드를 보내지 않는다', async ({ page, mock }) => {
  setAccountSettings(mock, OTHER_PC_ACCOUNT)
  await mock.loginAs(BOB)
  await page.goto('/settings/audio')
  await expect(page.getByTestId('device-local-note')).toContainText('이 PC에만 저장됩니다')
  // 이행: 계정의 옛 장치 값(이 PC에 없음)이 보인다
  await expect(page.locator('#input-device')).toHaveValue('wasapi::다른 PC 인터페이스')

  await page.locator('#input-device').selectOption('wasapi::이 PC USB 입력')
  const row = () => mock.supabase.db.table('profiles').find((p) => p.id === BOB.id)?.audio_settings as { engine: Record<string, unknown> } | undefined
  await expect.poll(() => row()?.engine && !('inputDevice' in row()!.engine), { timeout: 5000 }).toBe(true)
  for (const k of DEVICE_FIELDS) expect(k in row()!.engine).toBe(false)
  expect(row()!.engine).toMatchObject({ codec: 'pcm16', frameMs: 2.5 })
  expect(await page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? 'null') as unknown, DEVICE_KEY)).toMatchObject({ inputDevice: 'wasapi::이 PC USB 입력' })
})
