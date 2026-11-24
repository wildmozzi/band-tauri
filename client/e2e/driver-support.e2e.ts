/**
 * 배포본에 없는 드라이버·이 PC에 없는 장치 표시(P2-7, P2-27). 가짜 Tauri IPC(mocks/tauri.ts)로 "ASIO 없이 빌드된 엔진"과
 * "저장된 장치가 없는 PC"를 흉내 낸다.
 * - 저장된 ASIO 설정은 **그대로 두고**(다른 PC·ASIO 빌드에서 쓰던 값) 이번 실행만 WASAPI 공유 기본 장치로 시작한다고 안내한다.
 *   ASIO 선택지는 비활성 + 이유(P2-7). 예전에는 저장값을 WASAPI 독점으로 덮어썼다(P2-27에서 제거).
 * - 저장된 장치 id가 목록에 없으면(드라이버를 쓸 수 없는 경우 포함) 선택지에 "(이 PC에 없음) 이름 — 기본 장치 사용"으로 남기고 안내한다. 저장값은 그대로.
 * 옛 엔진(engine_supported_drivers 응답 없음)이면 예전처럼 모두 고를 수 있다.
 */
import type { DeviceInfo, DriverKind, EngineConfig } from '../src/native/engineApi'
import { DEFAULT_ENGINE_CONFIG } from '../src/native/engineConfig'
import { installFakeTauri } from './mocks/tauri'
import { expect, test } from './test'

/** 장치 쪽 값(드라이버·장치·입력 채널·버퍼)은 이 PC에만 저장한다(P2-27 후속) */
const STORAGE_KEY = 'band_device_settings_v1'
const ASIO_DEV = 'asio::Focusrite USB ASIO'
const asioConfig: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio', outputDriver: 'asio', inputDevice: ASIO_DEV, outputDevice: ASIO_DEV }

async function seedSettings(page: import('@playwright/test').Page, engine: EngineConfig) {
  // 새로고침(reload)에도 다시 심지 않도록 처음 한 번만
  await page.addInitScript(
    ([key, stored]: [string, string]) => {
      if (!sessionStorage.getItem('__e2eSeeded')) {
        localStorage.setItem(key, stored)
        sessionStorage.setItem('__e2eSeeded', '1')
      }
    },
    [STORAGE_KEY, JSON.stringify(engine)] as [string, string],
  )
}

function storedEngine(page: import('@playwright/test').Page): Promise<EngineConfig> {
  return page.evaluate((key) => JSON.parse(localStorage.getItem(key) ?? 'null') as EngineConfig, STORAGE_KEY)
}

function device(over: Partial<DeviceInfo> & Pick<DeviceInfo, 'id' | 'name' | 'isInput'>): DeviceInfo {
  return { driver: 'wasapiShared', isDefault: false, channels: 2, sampleRates: [48000], ...over }
}

test('ASIO 없는 배포본: 저장된 ASIO는 그대로 두고 WASAPI 공유 기본 장치로 시작한다고 안내, ASIO 선택지는 비활성 + 이유', async ({ page }) => {
  const supported: DriverKind[] = ['wasapiExclusive', 'wasapiShared']
  await installFakeTauri(page, { engine_list_devices: [], engine_supported_drivers: supported })
  await seedSettings(page, asioConfig)
  await page.goto('/settings/audio')

  await expect(page.getByTestId('driver-notice')).toContainText('저장된 드라이버(ASIO)는 이 배포본에서 쓸 수 없어 WASAPI 공유 기본 장치로 시작합니다.')
  await expect(page.getByTestId('driver-notice')).toContainText('저장된 설정은 그대로 둡니다')
  // 저장값(ASIO)을 그대로 보인다 — 고를 수는 없다
  const input = page.locator('#input-driver')
  await expect(input).toHaveValue('asio')
  await expect(page.locator('#output-driver')).toHaveValue('asio')
  await expect(input.locator('option[value="asio"]')).toBeDisabled()
  await expect(input.locator('option[value="wasapiShared"]')).toBeEnabled()
  await expect(page.getByTestId('driver-unsupported').first()).toHaveText('이 배포본에는 ASIO가 포함되어 있지 않습니다 — WASAPI 독점 모드를 쓰세요')
  // 저장된 ASIO 장치도 "기본 장치"로 보이지 않고 이름 그대로 보인다(#251 리뷰 Low-3)
  for (const id of ['#input-device', '#output-device']) {
    await expect(page.locator(id)).toHaveValue(ASIO_DEV)
    await expect(page.locator(id).locator('option:checked')).toHaveText('(이 PC에 없음) Focusrite USB ASIO — 기본 장치 사용')
  }

  // 저장된 설정은 바뀌지 않는다(새로 고쳐도 ASIO)
  expect((await storedEngine(page)).inputDriver).toBe('asio')
  await page.reload()
  await expect(page.locator('#input-driver')).toHaveValue('asio')
  expect(await storedEngine(page)).toMatchObject({ inputDriver: 'asio', outputDriver: 'asio', inputDevice: ASIO_DEV, outputDevice: ASIO_DEV })
  await expect(page.locator('#input-device')).toHaveValue(ASIO_DEV)
})

test('저장된 장치가 이 PC에 없음: 선택지에 남기고 기본 장치로 시작한다고 안내, 저장값은 그대로', async ({ page }) => {
  const savedIn = 'wasapi::Focusrite USB Input'
  const savedOut = 'wasapi::Focusrite USB Output'
  await installFakeTauri(page, {
    engine_supported_drivers: ['wasapiExclusive', 'wasapiShared'],
    engine_list_devices: [
      device({ id: 'wasapi::내장 마이크', name: '내장 마이크', isInput: true, isDefault: true }),
      device({ id: 'wasapi::스피커', name: '스피커', isInput: false, isDefault: true }),
    ],
  })
  await seedSettings(page, { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: savedIn, outputDevice: savedOut })
  await page.goto('/settings/audio')

  await expect(page.getByTestId('driver-notice')).toContainText(
    "저장된 장치 'Focusrite USB Input', 'Focusrite USB Output'를 찾을 수 없어 기본 장치로 시작합니다.",
  )
  const input = page.locator('#input-device')
  await expect(input).toHaveValue(savedIn)
  await expect(input.locator(`option[value="${savedIn}"]`)).toHaveText('(이 PC에 없음) Focusrite USB Input — 기본 장치 사용')
  await expect(page.locator('#output-device')).toHaveValue(savedOut)
  expect(await storedEngine(page)).toMatchObject({ inputDevice: savedIn, outputDevice: savedOut })

  // 사용자가 직접 고르면 그때 저장되고 안내가 사라진다
  await input.selectOption('wasapi::내장 마이크')
  await page.locator('#output-device').selectOption('')
  await expect(page.getByTestId('driver-notice')).toHaveCount(0)
  await expect.poll(async () => (await storedEngine(page)).inputDevice).toBe('wasapi::내장 마이크')
})

test('옛 엔진(지원 목록 없음): 모든 드라이버를 그대로 고를 수 있다', async ({ page }) => {
  await installFakeTauri(page, { engine_list_devices: [] })
  await seedSettings(page, { ...asioConfig, inputDevice: null, outputDevice: null })
  await page.goto('/settings/audio')

  const input = page.locator('#input-driver')
  await expect(input).toHaveValue('asio')
  await expect(input.locator('option[value="asio"]')).toBeEnabled()
  await expect(page.getByTestId('driver-notice')).toHaveCount(0)
  await expect(page.getByTestId('driver-unsupported')).toHaveCount(0)
})
