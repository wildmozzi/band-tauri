/**
 * 배포본에 없는 드라이버 표시(P2-7). 가짜 Tauri IPC(mocks/tauri.ts)로 "ASIO 없이 빌드된 엔진"을 흉내 낸다:
 * 저장된 ASIO 설정은 WASAPI 독점으로 되돌리고 안내, ASIO 선택지는 비활성 + 이유.
 * 옛 엔진(engine_supported_drivers 응답 없음)이면 예전처럼 모두 고를 수 있다.
 */
import type { DriverKind, EngineConfig } from '../src/native/engineApi'
import { DEFAULT_ENGINE_CONFIG, STORED_SETTINGS_VERSION } from '../src/native/engineConfig'
import { installFakeTauri } from './mocks/tauri'
import { expect, test } from './test'

const asioConfig: EngineConfig = { ...DEFAULT_ENGINE_CONFIG, inputDriver: 'asio', outputDriver: 'asio' }

async function seedSettings(page: import('@playwright/test').Page, engine: EngineConfig) {
  await page.addInitScript((stored: string) => localStorage.setItem('band_engine_config_v2', stored), JSON.stringify({ version: STORED_SETTINGS_VERSION, engine }))
}

test('ASIO 없는 배포본: 저장된 ASIO를 독점으로 되돌리고, ASIO 선택지는 비활성 + 이유', async ({ page }) => {
  const supported: DriverKind[] = ['wasapiExclusive', 'wasapiShared']
  await installFakeTauri(page, { engine_list_devices: [], engine_supported_drivers: supported })
  await seedSettings(page, asioConfig)
  await page.goto('/settings/audio')

  await expect(page.getByTestId('driver-notice')).toContainText('WASAPI 독점 모드로 바꿨습니다')
  const input = page.locator('#input-driver')
  await expect(input).toHaveValue('wasapiExclusive')
  await expect(page.locator('#output-driver')).toHaveValue('wasapiExclusive')
  await expect(input.locator('option[value="asio"]')).toBeDisabled()
  await expect(input.locator('option[value="wasapiShared"]')).toBeEnabled()
  await expect(page.getByTestId('driver-unsupported')).toHaveText('이 배포본에는 ASIO가 포함되어 있지 않습니다 — WASAPI 독점 모드를 쓰세요')
})

test('옛 엔진(지원 목록 없음): 모든 드라이버를 그대로 고를 수 있다', async ({ page }) => {
  await installFakeTauri(page, { engine_list_devices: [] })
  await seedSettings(page, asioConfig)
  await page.goto('/settings/audio')

  const input = page.locator('#input-driver')
  await expect(input).toHaveValue('asio')
  await expect(input.locator('option[value="asio"]')).toBeEnabled()
  await expect(page.getByTestId('driver-notice')).toHaveCount(0)
  await expect(page.getByTestId('driver-unsupported')).toHaveCount(0)
})
